//! ボリュームの自動検出。
//!
//! 固定ディスク上の NTFS ボリュームのみを対象にし、ボリューム GUID で一意化する
//! （同一ボリュームが複数のドライブ文字/マウントポイントに割り当てられていても 1 回だけ索引する）。
//! Win32 のボリューム情報 API は死にかけのデバイスや切断されたネットワークドライブで
//! 同期ブロックすることがあるため、1 ボリュームごとに別スレッドで期限付きで問い合わせ、
//! 期限内に返らないものは無視する（スレッドは放置する）。

use std::sync::mpsc;
use std::time::Duration;
use windows_sys::Win32::Foundation::{INVALID_HANDLE_VALUE, MAX_PATH};
use windows_sys::Win32::Storage::FileSystem::{
    FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationW,
    GetVolumePathNamesForVolumeNameW,
};

pub const DRIVE_FIXED: u32 = 3;

#[derive(Debug, Clone)]
pub struct VolumeInfo {
    /// `\\?\Volume{GUID}\`
    pub guid_path: String,
    /// `C:\` や `D:\mnt\x\` など。先頭はドライブ文字のものを優先して並べる。
    pub mount_points: Vec<String>,
    pub fs_name: String,
    pub label: String,
    pub serial: u32,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

impl VolumeInfo {
    /// `\\?\Volume{GUID}`（CreateFile 用、末尾 `\` なし）
    pub fn device_path(&self) -> String {
        self.guid_path.trim_end_matches('\\').to_string()
    }
    /// 表示用の代表名（`C:` など）
    pub fn display_root(&self) -> String {
        self.mount_points
            .first()
            .map(|m| m.trim_end_matches('\\').to_string())
            .unwrap_or_else(|| self.guid_path.clone())
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

fn list_volume_guids() -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = [0u16; MAX_PATH as usize];
    // SAFETY: buf は固定長で、長さを渡している。
    let h = unsafe { FindFirstVolumeW(buf.as_mut_ptr(), buf.len() as u32) };
    if h == INVALID_HANDLE_VALUE {
        return out;
    }
    loop {
        out.push(from_wide(&buf));
        if unsafe { FindNextVolumeW(h, buf.as_mut_ptr(), buf.len() as u32) } == 0 {
            break;
        }
    }
    unsafe { FindVolumeClose(h) };
    out
}

/// 1 ボリュームの情報を取得する（ブロックし得る呼び出しを含む）。
fn probe(guid_path: &str) -> Option<VolumeInfo> {
    let w = wide(guid_path);
    if unsafe { GetDriveTypeW(w.as_ptr()) } != DRIVE_FIXED {
        return None;
    }
    let mut label = [0u16; 256];
    let mut fs = [0u16; 64];
    let mut serial = 0u32;
    let mut max_comp = 0u32;
    let mut flags = 0u32;
    let ok = unsafe {
        GetVolumeInformationW(
            w.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            &mut serial,
            &mut max_comp,
            &mut flags,
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    let fs_name = from_wide(&fs);
    if fs_name != "NTFS" && fs_name != "ReFS" {
        return None;
    }
    let mut names = vec![0u16; 4096];
    let mut len = 0u32;
    let ok = unsafe { GetVolumePathNamesForVolumeNameW(w.as_ptr(), names.as_mut_ptr(), names.len() as u32, &mut len) };
    let mut mount_points: Vec<String> = Vec::new();
    if ok != 0 {
        for part in names[..len as usize].split(|&c| c == 0) {
            if !part.is_empty() {
                mount_points.push(String::from_utf16_lossy(part));
            }
        }
    }
    mount_points.sort_by_key(|m| (m.len() != 3, m.clone()));
    // マウントされていないボリューム（回復パーティション等）はユーザーから見えないので対象外
    if mount_points.is_empty() {
        return None;
    }
    let mut total = 0u64;
    let mut free = 0u64;
    unsafe { GetDiskFreeSpaceExW(w.as_ptr(), std::ptr::null_mut(), &mut total, &mut free) };
    Some(VolumeInfo {
        guid_path: guid_path.to_string(),
        mount_points,
        fs_name,
        label: from_wide(&label),
        serial,
        total_bytes: total,
        free_bytes: free,
    })
}

/// 固定ディスクの NTFS ボリュームを列挙する。`timeout` 内に応答しないボリュームは除外する。
pub fn discover_volumes(timeout: Duration) -> Vec<VolumeInfo> {
    let guids = list_volume_guids();
    let (tx, rx) = mpsc::channel();
    for g in guids.iter().cloned() {
        let tx = tx.clone();
        std::thread::Builder::new()
            .name(format!("probe {g}"))
            .spawn(move || {
                let r = probe(&g);
                let _ = tx.send((g, r));
            })
            .ok();
    }
    drop(tx);
    let deadline = std::time::Instant::now() + timeout;
    let mut out = Vec::new();
    let mut got = 0;
    while got < guids.len() {
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        match rx.recv_timeout(deadline - now) {
            Ok((g, r)) => {
                got += 1;
                match r {
                    Some(v) => out.push(v),
                    None => log::debug!("skip volume {g}"),
                }
            }
            Err(_) => break,
        }
    }
    if got < guids.len() {
        log::warn!("{} 個のボリュームが {timeout:?} 以内に応答しなかったため除外", guids.len() - got);
    }
    out.sort_by_key(|a| a.display_root());
    out
}
