//! USN ベースの列挙（ReFS 向け。NTFS でも動く）と、ファイル ID からの属性取得。
//!
//! ReFS には MFT が無いので、`FSCTL_ENUM_USN_DATA` で全ファイルの (ID, 親 ID, 属性, 名前) を列挙し、
//! サイズ・日時は `OpenFileById` で後から埋める。レコードは `USN_RECORD_V2`（64bit ID）と
//! `USN_RECORD_V3`（128bit ID）の両方を解釈する。
//!
//! 事前条件: `enum_usn_data` はボリュームを `GENERIC_READ` で開く必要があり管理者権限が要る。
//! ただし ReFS は `FSCTL_ENUM_USN_DATA` に応えない（error 1、2026-11-12 に Dev Drive で確認）ので、
//! ReFS の初回構築は `walk_with_ids`（ディレクトリ列挙でファイル ID・サイズ・日時をまとめて取る）で行う。
//! `FileIdReader` と `walk_with_ids` はルートディレクトリのハンドルを基点にするので非昇格でも動く。

use crate::mft::utf16_to_string;
use anyhow::{anyhow, Result};
use std::fs::File;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use windows_sys::Win32::Foundation::{GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetFileInformationByHandle, GetFileInformationByHandleEx, GetFinalPathNameByHandleW, OpenFileById,
    BY_HANDLE_FILE_INFORMATION, ExtendedFileIdType, FileIdInfo, FileIdType, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_128,
    FILE_ID_DESCRIPTOR, FILE_ID_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Ioctl::FSCTL_ENUM_USN_DATA;
use windows_sys::Win32::System::IO::DeviceIoControl;

/// 列挙で得られる 1 件。`id` / `parent` は V2 なら 64bit、V3 なら 128bit の値。
/// サイズ・日時は `walk_with_ids` でだけ埋まる（USN 列挙には無い）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnumRecord {
    pub id: u128,
    pub parent: u128,
    pub attrs: u32,
    pub name: String,
    pub size: u64,
    pub mtime: i64,
    pub ctime: i64,
    pub atime: i64,
}

/// `FSCTL_ENUM_USN_DATA` / `FSCTL_READ_USN_JOURNAL` の出力に並ぶ USN レコード列を解釈する。
/// `buf` は先頭 8 バイト（次の FRN / 次の USN）を除いたレコード部分。
pub fn parse_usn_records(buf: &[u8], mut sink: impl FnMut(EnumRecord, i64, u32)) {
    let mut p = 0usize;
    while p + 8 <= buf.len() {
        let b = &buf[p..];
        let len = u32::from_le_bytes(b[0..4].try_into().unwrap()) as usize;
        if len < 8 || p + len > buf.len() {
            break;
        }
        let major = u16::from_le_bytes([b[4], b[5]]);
        let rec = &b[..len];
        match major {
            2 if len >= 60 => {
                let name_len = u16::from_le_bytes([rec[56], rec[57]]) as usize;
                let name_ofs = u16::from_le_bytes([rec[58], rec[59]]) as usize;
                if name_ofs + name_len <= len {
                    sink(
                        EnumRecord {
                            id: u64::from_le_bytes(rec[8..16].try_into().unwrap()) as u128,
                            parent: u64::from_le_bytes(rec[16..24].try_into().unwrap()) as u128,
                            attrs: u32::from_le_bytes(rec[52..56].try_into().unwrap()),
                            name: utf16_to_string(&rec[name_ofs..name_ofs + name_len]),
                            ..Default::default()
                        },
                        i64::from_le_bytes(rec[24..32].try_into().unwrap()),
                        u32::from_le_bytes(rec[40..44].try_into().unwrap()),
                    );
                }
            }
            3 if len >= 76 => {
                let name_len = u16::from_le_bytes([rec[72], rec[73]]) as usize;
                let name_ofs = u16::from_le_bytes([rec[74], rec[75]]) as usize;
                if name_ofs + name_len <= len {
                    sink(
                        EnumRecord {
                            id: u128::from_le_bytes(rec[8..24].try_into().unwrap()),
                            parent: u128::from_le_bytes(rec[24..40].try_into().unwrap()),
                            attrs: u32::from_le_bytes(rec[68..72].try_into().unwrap()),
                            name: utf16_to_string(&rec[name_ofs..name_ofs + name_len]),
                            ..Default::default()
                        },
                        i64::from_le_bytes(rec[40..48].try_into().unwrap()),
                        u32::from_le_bytes(rec[56..60].try_into().unwrap()),
                    );
                }
            }
            _ => {}
        }
        p += (len + 7) & !7;
    }
}

/// `\\?\Volume{GUID}` を `GENERIC_READ` で開き（要管理者）、全レコードを列挙する。
/// 戻り値は件数。`cancel` が true になれば途中で止める。
pub fn enum_usn_data(volume: &str, cancel: &dyn Fn() -> bool, mut sink: impl FnMut(EnumRecord)) -> Result<u64> {
    let wide: Vec<u16> = volume.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: NUL 終端文字列。戻り値を検査する。
    let h = unsafe {
        CreateFileW(
            wide.as_ptr(),
            windows_sys::Win32::Foundation::GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(anyhow!("ボリューム {volume} を開けない (error {})", unsafe { GetLastError() }));
    }
    // SAFETY: 有効なハンドルの所有権を File に移す。
    let file = unsafe { File::from_raw_handle(h as _) };
    // MFT_ENUM_DATA_V1: StartFileReferenceNumber, LowUsn, HighUsn, MinMajorVersion=2, MaxMajorVersion=3
    let mut start: u64 = 0;
    let mut out = vec![0u8; 1 << 20];
    let mut count = 0u64;
    loop {
        if cancel() {
            break;
        }
        let mut input = [0u8; 28];
        input[0..8].copy_from_slice(&start.to_le_bytes());
        input[8..16].copy_from_slice(&0i64.to_le_bytes());
        input[16..24].copy_from_slice(&i64::MAX.to_le_bytes());
        input[24..26].copy_from_slice(&2u16.to_le_bytes());
        input[26..28].copy_from_slice(&3u16.to_le_bytes());
        let mut ret = 0u32;
        // SAFETY: 入出力バッファは有効なスライスで、長さも渡している。
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle() as _,
                FSCTL_ENUM_USN_DATA,
                input.as_ptr() as _,
                input.len() as u32,
                out.as_mut_ptr() as _,
                out.len() as u32,
                &mut ret,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let e = unsafe { GetLastError() };
            // ERROR_HANDLE_EOF: 列挙完了
            if e == 38 {
                break;
            }
            return Err(anyhow!("FSCTL_ENUM_USN_DATA failed: {e}"));
        }
        let n = ret as usize;
        if n < 8 {
            break;
        }
        start = u64::from_le_bytes(out[0..8].try_into().unwrap());
        parse_usn_records(&out[8..n], |r, _, _| {
            count += 1;
            sink(r);
        });
    }
    Ok(count)
}

/// `OpenFileById` で得る属性。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileIdInfo {
    pub attrs: u32,
    pub size: u64,
    pub ctime: i64,
    pub atime: i64,
    pub mtime: i64,
    /// このハンドルで見た 64bit ファイル ID（nFileIndexHigh/Low）
    pub id: u64,
}

/// ファイル ID からサイズ・日時・属性・パスを引く。ルートディレクトリのハンドルを基点にする
/// （`FILE_FLAG_BACKUP_SEMANTICS`。管理者権限は不要）。
pub struct FileIdReader {
    root: File,
}

fn ft(f: windows_sys::Win32::Foundation::FILETIME) -> i64 {
    ((f.dwHighDateTime as i64) << 32) | f.dwLowDateTime as i64
}

impl FileIdReader {
    /// `root` は `C:\` や `\\?\Volume{GUID}\` のようなディレクトリ。
    pub fn open(root: &str) -> Result<Self> {
        let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: NUL 終端文字列。戻り値を検査する。
        let h = unsafe {
            CreateFileW(
                wide.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(anyhow!("{root} を開けない (error {})", unsafe { GetLastError() }));
        }
        // SAFETY: 有効なハンドルの所有権を File に移す。
        Ok(Self { root: unsafe { File::from_raw_handle(h as _) } })
    }

    fn open_by_id(&self, id: u128) -> Option<File> {
        let mut desc = FILE_ID_DESCRIPTOR { dwSize: std::mem::size_of::<FILE_ID_DESCRIPTOR>() as u32, ..Default::default() };
        if id >> 64 == 0 {
            desc.Type = FileIdType;
            desc.Anonymous.FileId = id as i64;
        } else {
            desc.Type = ExtendedFileIdType;
            desc.Anonymous.ExtendedFileId = FILE_ID_128 { Identifier: id.to_le_bytes() };
        }
        // SAFETY: 記述子は初期化済み。戻り値を検査する。
        let h = unsafe {
            OpenFileById(
                self.root.as_raw_handle() as _,
                &desc,
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                FILE_FLAG_BACKUP_SEMANTICS,
            )
        };
        if h == INVALID_HANDLE_VALUE || h.is_null() {
            return None;
        }
        // SAFETY: 有効なハンドルの所有権を File に移す。
        Some(unsafe { File::from_raw_handle(h as _) })
    }

    /// サイズ・日時・属性。ID が無効（削除済みなど）なら None。
    pub fn info(&self, id: u128) -> Option<FileIdInfo> {
        let f = self.open_by_id(id)?;
        // SAFETY: 出力専用構造体。
        let mut bh: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(f.as_raw_handle() as _, &mut bh) } == 0 {
            return None;
        }
        Some(FileIdInfo {
            attrs: bh.dwFileAttributes,
            size: ((bh.nFileSizeHigh as u64) << 32) | bh.nFileSizeLow as u64,
            ctime: ft(bh.ftCreationTime),
            atime: ft(bh.ftLastAccessTime),
            mtime: ft(bh.ftLastWriteTime),
            id: ((bh.nFileIndexHigh as u64) << 32) | bh.nFileIndexLow as u64,
        })
    }

    /// パス（ファイルでもフォルダでも）の 128bit ファイル ID（NTFS では下位 64bit が FRN、上位は 0）。
    pub fn id_of_path(&self, path: &str) -> Option<u128> {
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: NUL 終端文字列。戻り値を検査する。
        let h = unsafe {
            CreateFileW(
                wide.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return None;
        }
        // SAFETY: 有効なハンドルの所有権を File に移す。
        let f = unsafe { File::from_raw_handle(h as _) };
        id128_of(&f)
    }

    /// フルパス（`\\?\` プレフィックスは外す）。
    pub fn path(&self, id: u128) -> Option<String> {
        let f = self.open_by_id(id)?;
        let mut buf = vec![0u16; 1024];
        loop {
            // SAFETY: バッファ長を渡している。
            let n = unsafe { GetFinalPathNameByHandleW(f.as_raw_handle() as _, buf.as_mut_ptr(), buf.len() as u32, 0) } as usize;
            if n == 0 {
                return None;
            }
            if n < buf.len() {
                let s = String::from_utf16_lossy(&buf[..n]);
                return Some(s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s));
            }
            buf.resize(n + 1, 0);
        }
    }
}

/// `root`（`R:\` や `\\?\Volume{GUID}\`）配下をディレクトリ列挙で辿り、各エントリの 128bit ファイル ID・
/// 親 ID・属性・サイズ・日時を `sink` に渡す（`FileIdExtdDirectoryInfo`: 1 ディレクトリ 1 系列の呼び出しで
/// まとめて取れる）。リパースポイントには降りない。戻り値はルートの ID。非昇格でも動く。
pub fn walk_with_ids(root: &str, cancel: &dyn Fn() -> bool, progress: &dyn Fn(usize), mut sink: impl FnMut(EnumRecord)) -> Result<u128> {
    const FILE_ID_EXTD_DIRECTORY_INFO: i32 = 19;
    const ERROR_NO_MORE_FILES: u32 = 18;
    fn open_dir(path: &str) -> Option<File> {
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: NUL 終端文字列。戻り値を検査する。
        let h = unsafe {
            CreateFileW(
                wide.as_ptr(),
                0x0001, // FILE_LIST_DIRECTORY
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return None;
        }
        // SAFETY: 有効なハンドルの所有権を File に移す。
        Some(unsafe { File::from_raw_handle(h as _) })
    }
    let root_path = {
        let r = root.trim_end_matches('\\');
        if r.starts_with(r"\\?\") { format!("{r}\\") } else { format!(r"\\?\{r}\") }
    };
    let root_dir = open_dir(&root_path).ok_or_else(|| anyhow!("{root} を開けない (error {})", unsafe { GetLastError() }))?;
    let root_id = id128_of(&root_dir).ok_or_else(|| anyhow!("{root} のファイル ID が取れない"))?;
    let mut stack: Vec<(File, u128, String)> = vec![(root_dir, root_id, root_path)];
    let mut buf = vec![0u8; 256 << 10];
    let mut count = 0usize;
    while let Some((dir, dir_id, dir_path)) = stack.pop() {
        if cancel() {
            break;
        }
        loop {
            // SAFETY: バッファ長を渡している。失敗は GetLastError で判定する。
            let ok = unsafe { GetFileInformationByHandleEx(dir.as_raw_handle() as _, FILE_ID_EXTD_DIRECTORY_INFO, buf.as_mut_ptr() as *mut _, buf.len() as u32) };
            if ok == 0 {
                let e = unsafe { GetLastError() };
                if e != ERROR_NO_MORE_FILES {
                    log::debug!("{dir_path}: 列挙失敗 error {e}");
                }
                break;
            }
            let mut p = 0usize;
            loop {
                let e = &buf[p..];
                if e.len() < 88 {
                    break;
                }
                let next = u32::from_le_bytes(e[0..4].try_into().unwrap()) as usize;
                let name_len = u32::from_le_bytes(e[60..64].try_into().unwrap()) as usize;
                let attrs = u32::from_le_bytes(e[56..60].try_into().unwrap());
                let id = u128::from_le_bytes(e[72..88].try_into().unwrap());
                let name = if 88 + name_len <= e.len() { utf16_to_string(&e[88..88 + name_len]) } else { String::new() };
                if name != "." && name != ".." && !name.is_empty() {
                    let is_dir = attrs & 0x10 != 0;
                    let is_reparse = attrs & 0x400 != 0;
                    sink(EnumRecord {
                        id,
                        parent: dir_id,
                        attrs,
                        size: if is_dir { 0 } else { i64::from_le_bytes(e[40..48].try_into().unwrap()).max(0) as u64 },
                        mtime: i64::from_le_bytes(e[24..32].try_into().unwrap()),
                        ctime: i64::from_le_bytes(e[8..16].try_into().unwrap()),
                        atime: i64::from_le_bytes(e[16..24].try_into().unwrap()),
                        name: name.clone(),
                    });
                    count += 1;
                    if count.is_multiple_of(20_000) {
                        progress(count);
                    }
                    if is_dir && !is_reparse {
                        let child = format!("{dir_path}{name}\\");
                        if let Some(f) = open_dir(&child) {
                            stack.push((f, id, child));
                        }
                    }
                }
                if next == 0 {
                    break;
                }
                p += next;
            }
        }
    }
    progress(count);
    Ok(root_id)
}

/// ハンドルの 128bit ファイル ID。
pub fn id128_of(f: &File) -> Option<u128> {
    // SAFETY: 出力専用構造体。サイズを渡している。
    let mut info: FILE_ID_INFO = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetFileInformationByHandleEx(f.as_raw_handle() as _, FileIdInfo, &mut info as *mut _ as *mut _, std::mem::size_of::<FILE_ID_INFO>() as u32)
    };
    (ok != 0).then(|| u128::from_le_bytes(info.FileId.Identifier))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v2(id: u64, parent: u64, attrs: u32, name: &str) -> Vec<u8> {
        let n: Vec<u8> = name.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let len = ((60 + n.len() + 7) & !7) as u32;
        let mut r = vec![0u8; len as usize];
        r[0..4].copy_from_slice(&len.to_le_bytes());
        r[4..6].copy_from_slice(&2u16.to_le_bytes());
        r[8..16].copy_from_slice(&id.to_le_bytes());
        r[16..24].copy_from_slice(&parent.to_le_bytes());
        r[24..32].copy_from_slice(&1234i64.to_le_bytes());
        r[40..44].copy_from_slice(&0x100u32.to_le_bytes());
        r[52..56].copy_from_slice(&attrs.to_le_bytes());
        r[56..58].copy_from_slice(&(n.len() as u16).to_le_bytes());
        r[58..60].copy_from_slice(&60u16.to_le_bytes());
        r[60..60 + n.len()].copy_from_slice(&n);
        r
    }

    fn v3(id: u128, parent: u128, attrs: u32, name: &str) -> Vec<u8> {
        let n: Vec<u8> = name.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let len = ((76 + n.len() + 7) & !7) as u32;
        let mut r = vec![0u8; len as usize];
        r[0..4].copy_from_slice(&len.to_le_bytes());
        r[4..6].copy_from_slice(&3u16.to_le_bytes());
        r[8..24].copy_from_slice(&id.to_le_bytes());
        r[24..40].copy_from_slice(&parent.to_le_bytes());
        r[40..48].copy_from_slice(&99i64.to_le_bytes());
        r[56..60].copy_from_slice(&0x200u32.to_le_bytes());
        r[68..72].copy_from_slice(&attrs.to_le_bytes());
        r[72..74].copy_from_slice(&(n.len() as u16).to_le_bytes());
        r[74..76].copy_from_slice(&76u16.to_le_bytes());
        r[76..76 + n.len()].copy_from_slice(&n);
        r
    }

    #[test]
    fn parses_v2_and_v3_records() {
        let mut buf = v2(0x0005_0000_0000_0010, 5, 0x20, "a.txt");
        buf.extend(v3((1u128 << 70) | 7, 1u128 << 70, 0x10, "フォルダ"));
        buf.extend(v2(99, 5, 0x20, "")); // 名前なし
        let mut got = Vec::new();
        parse_usn_records(&buf, |r, usn, reason| got.push((r, usn, reason)));
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].0, EnumRecord { id: 0x0005_0000_0000_0010, parent: 5, attrs: 0x20, name: "a.txt".into(), ..Default::default() });
        assert_eq!((got[0].1, got[0].2), (1234, 0x100));
        assert_eq!(got[1].0, EnumRecord { id: (1u128 << 70) | 7, parent: 1u128 << 70, attrs: 0x10, name: "フォルダ".into(), ..Default::default() });
        assert_eq!((got[1].1, got[1].2), (99, 0x200));
        assert_eq!(got[2].0.name, "");
    }

    #[test]
    fn walk_reports_ids_sizes_and_parents() {
        let dir = std::env::temp_dir().join("accio-walk-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), b"12345").unwrap();
        std::fs::write(dir.join("sub").join("b.bin"), b"xy").unwrap();
        let mut got = Vec::new();
        let root_id = walk_with_ids(&dir.to_string_lossy(), &|| false, &|_| {}, |r| got.push(r)).unwrap();
        assert_eq!(got.len(), 3, "{got:?}");
        let a = got.iter().find(|r| r.name == "a.txt").unwrap();
        let sub = got.iter().find(|r| r.name == "sub").unwrap();
        let b = got.iter().find(|r| r.name == "b.bin").unwrap();
        assert_eq!(a.size, 5);
        assert_eq!(a.parent, root_id);
        assert!(sub.attrs & 0x10 != 0);
        assert_eq!(b.parent, sub.id);
        assert!(a.mtime > 0 && a.ctime > 0);
        // ID はパスから引いたものと一致する（OpenFileById の基点と同じ体系）
        let r = FileIdReader::open(&format!("{}\\", dir.components().next().unwrap().as_os_str().to_string_lossy())).unwrap();
        assert_eq!(r.id_of_path(&dir.join("a.txt").to_string_lossy()), Some(a.id));
        // 環境変数で任意のルート（ReFS など）も歩けることを確かめる
        if let Ok(extra) = std::env::var("ACCIO_WALK_PATH") {
            let mut n = 0usize;
            let mut first: Option<EnumRecord> = None;
            let rid = walk_with_ids(&extra, &|| false, &|_| {}, |r| {
                if first.is_none() && r.attrs & 0x10 == 0 {
                    first = Some(r.clone());
                }
                n += 1
            })
            .unwrap();
            eprintln!("walk {extra}: {n} entries, root id {rid:#x}");
            assert!(n > 0);
            let rr = FileIdReader::open(&extra).unwrap();
            if let Some(f) = first {
                eprintln!("first file {} id {:#x}: info={:?} path={:?}", f.name, f.id, rr.info(f.id), rr.path(f.id));
            }
        }
    }

    #[test]
    fn truncated_buffer_stops_cleanly() {
        let full = v2(1, 5, 0x20, "abc.txt");
        let mut n = 0;
        parse_usn_records(&full[..full.len() - 4], |_, _, _| n += 1);
        assert_eq!(n, 0);
    }

    /// 非昇格で動く: 一時ファイルの ID を取り、ID から開き直して同じサイズ・パスが得られる。
    #[test]
    fn file_id_reader_roundtrip_on_ntfs() {
        let dir = std::env::temp_dir().join("accio-fileid-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.txt");
        std::fs::write(&p, b"hello world").unwrap();
        let root = format!("{}\\", p.components().next().unwrap().as_os_str().to_string_lossy());
        let r = FileIdReader::open(&root).unwrap();
        // 既知ファイルの ID を取る
        let f = File::open(&p).unwrap();
        let mut bh: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        assert_ne!(unsafe { GetFileInformationByHandle(f.as_raw_handle() as _, &mut bh) }, 0);
        let id = ((bh.nFileIndexHigh as u64) << 32) | bh.nFileIndexLow as u64;
        drop(f);
        let info = r.info(id as u128).unwrap();
        assert_eq!(info.size, 11);
        assert_eq!(info.id, id);
        assert_eq!(info.attrs & 0x10, 0);
        let path = r.path(id as u128).unwrap();
        assert!(path.to_lowercase().ends_with("accio-fileid-test\\x.txt"), "{path}");
        // パスからの 128bit ID は NTFS では 64bit FRN と一致し、親フォルダの ID も取れる
        assert_eq!(r.id_of_path(&p.to_string_lossy()), Some(id as u128));
        assert!(r.id_of_path(&format!("{}\\", dir.to_string_lossy())).is_some());
        std::fs::remove_file(&p).unwrap();
        assert!(r.info(id as u128).is_none() || r.path(id as u128).is_none());
    }
}
