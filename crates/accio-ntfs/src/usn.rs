//! USN ジャーナルの照会・読み取りと、FRN 指定での単一 MFT レコード取得。
//!
//! 読み取りはすべて非ブロッキング（BytesToWaitFor=0）でポーリングする。
//! ブロッキング読み取りはディスマウント時にハンドルをキャンセルする手間が増えるだけで利点がない。

use crate::mft::{MftEntry, MftRecordParser};
use crate::boot::BootInfo;
use anyhow::{anyhow, Result};
use std::fs::File;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use windows_sys::Win32::Foundation::{GetLastError, GENERIC_READ, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows_sys::Win32::System::Ioctl::{
    FSCTL_CREATE_USN_JOURNAL, FSCTL_GET_NTFS_FILE_RECORD, FSCTL_GET_NTFS_VOLUME_DATA, FSCTL_QUERY_USN_JOURNAL,
    FSCTL_READ_USN_JOURNAL,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

pub const ERROR_JOURNAL_DELETE_IN_PROGRESS: u32 = 1178;
pub const ERROR_JOURNAL_NOT_ACTIVE: u32 = 1179;
pub const ERROR_JOURNAL_ENTRY_DELETED: u32 = 1181;

pub mod reason {
    pub const DATA_OVERWRITE: u32 = 0x0000_0001;
    pub const DATA_EXTEND: u32 = 0x0000_0002;
    pub const DATA_TRUNCATION: u32 = 0x0000_0004;
    pub const FILE_CREATE: u32 = 0x0000_0100;
    pub const FILE_DELETE: u32 = 0x0000_0200;
    pub const RENAME_OLD_NAME: u32 = 0x0000_1000;
    pub const RENAME_NEW_NAME: u32 = 0x0000_2000;
    pub const BASIC_INFO_CHANGE: u32 = 0x0000_8000;
    pub const HARD_LINK_CHANGE: u32 = 0x0001_0000;
    pub const CLOSE: u32 = 0x8000_0000;
}

pub type UsnReason = u32;

#[derive(Debug, Clone)]
pub struct UsnEvent {
    pub frn: u64,
    pub parent_frn: u64,
    pub usn: i64,
    pub reason: UsnReason,
    pub attrs: u32,
    pub name: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct JournalState {
    pub journal_id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub max_size: u64,
}

pub struct UsnJournal {
    file: File,
    record_size: usize,
    /// 受け付ける USN レコードの最大版。NTFS は 2（検証済み）、ReFS は 3（128bit ID）
    max_major: u16,
    /// ReFS: ファイル ID ⇔ 合成レコード番号の変換と、OpenFileById によるレコード読み
    refs: Option<std::sync::Arc<crate::refs::RefsAdapter>>,
}

/// FRN から MFT レコードを引く能力。索引更新層はこれだけに依存する（テストで差し替えるため）。
pub trait RecordSource: Sync {
    fn read_file_record(&self, frn: u64) -> Option<MftEntry>;
}

impl RecordSource for UsnJournal {
    fn read_file_record(&self, frn: u64) -> Option<MftEntry> {
        match &self.refs {
            Some(a) => a.read_file_record(frn),
            None => UsnJournal::read_file_record(self, frn),
        }
    }
}

#[repr(C)]
#[derive(Default)]
struct NtfsVolumeData {
    serial: u64,
    number_sectors: i64,
    total_clusters: i64,
    free_clusters: i64,
    total_reserved: i64,
    bytes_per_sector: u32,
    bytes_per_cluster: u32,
    bytes_per_file_record_segment: u32,
    clusters_per_file_record_segment: u32,
    mft_valid_data_length: i64,
    mft_start_lcn: i64,
    mft2_start_lcn: i64,
    mft_zone_start: i64,
    mft_zone_end: i64,
}

fn ioctl(file: &File, code: u32, input: &[u8], output: &mut [u8]) -> std::result::Result<usize, u32> {
    let mut ret = 0u32;
    // SAFETY: すべてのポインタは有効なスライスに対応し、長さも渡している。
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as _,
            code,
            input.as_ptr() as _,
            input.len() as u32,
            output.as_mut_ptr() as _,
            output.len() as u32,
            &mut ret,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(unsafe { GetLastError() })
    } else {
        Ok(ret as usize)
    }
}

impl UsnJournal {
    /// `\\?\Volume{GUID}`（末尾 `\` なし）を開く。管理者権限が必要。
    pub fn open(volume: &str) -> Result<Self> {
        let wide: Vec<u16> = volume.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: NUL 終端文字列。戻り値を検査する。
        let h = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ,
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
        // SAFETY: 有効なハンドルの所有権移転。
        let file = unsafe { File::from_raw_handle(h as _) };
        let mut vd = NtfsVolumeData::default();
        let out = unsafe {
            std::slice::from_raw_parts_mut(&mut vd as *mut _ as *mut u8, std::mem::size_of::<NtfsVolumeData>())
        };
        ioctl(&file, FSCTL_GET_NTFS_VOLUME_DATA, &[], out)
            .map_err(|e| anyhow!("FSCTL_GET_NTFS_VOLUME_DATA failed: {e}"))?;
        Ok(Self { file, record_size: vd.bytes_per_file_record_segment as usize, max_major: 2, refs: None })
    }

    /// ReFS ボリューム用: MFT の情報を問い合わせず、V3（128bit ID）のレコードを受け付ける。
    /// 変換と読みは `set_refs` で与える適応層が担う。管理者権限が必要。
    pub fn open_refs(volume: &str) -> Result<Self> {
        let wide: Vec<u16> = volume.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: NUL 終端文字列。戻り値を検査する。
        let h = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ,
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
        // SAFETY: 有効なハンドルの所有権移転。
        let file = unsafe { File::from_raw_handle(h as _) };
        Ok(Self { file, record_size: 0, max_major: 3, refs: None })
    }

    pub fn set_refs(&mut self, adapter: std::sync::Arc<crate::refs::RefsAdapter>) {
        self.refs = Some(adapter);
    }
    pub fn refs(&self) -> Option<&std::sync::Arc<crate::refs::RefsAdapter>> {
        self.refs.as_ref()
    }

    pub fn query(&self) -> std::result::Result<JournalState, u32> {
        let mut out = [0u8; 80];
        ioctl(&self.file, FSCTL_QUERY_USN_JOURNAL, &[], &mut out)?;
        Ok(JournalState {
            journal_id: u64::from_le_bytes(out[0..8].try_into().unwrap()),
            first_usn: i64::from_le_bytes(out[8..16].try_into().unwrap()),
            next_usn: i64::from_le_bytes(out[16..24].try_into().unwrap()),
            max_size: u64::from_le_bytes(out[40..48].try_into().unwrap()),
        })
    }

    /// ジャーナルが無ければ作る。既にあればそのまま。
    pub fn ensure(&self, max_size: u64, alloc_delta: u64) -> Result<JournalState> {
        match self.query() {
            Ok(s) => Ok(s),
            Err(ERROR_JOURNAL_NOT_ACTIVE) => {
                let mut input = [0u8; 16];
                input[0..8].copy_from_slice(&max_size.to_le_bytes());
                input[8..16].copy_from_slice(&alloc_delta.to_le_bytes());
                ioctl(&self.file, FSCTL_CREATE_USN_JOURNAL, &input, &mut [])
                    .map_err(|e| anyhow!("FSCTL_CREATE_USN_JOURNAL failed: {e}"))?;
                self.query().map_err(|e| anyhow!("FSCTL_QUERY_USN_JOURNAL failed: {e}"))
            }
            Err(e) => Err(anyhow!("FSCTL_QUERY_USN_JOURNAL failed: {e}")),
        }
    }

    /// `start` 以降のレコードを非ブロッキングで読む。戻り値は (次に読む USN, イベント列)。
    /// Err(ERROR_JOURNAL_ENTRY_DELETED) はジャーナルのラップ（= 全再スキャンが必要）。
    pub fn read(&self, journal_id: u64, start: i64, buf: &mut Vec<u8>) -> std::result::Result<(i64, Vec<UsnEvent>), u32> {
        if buf.len() < 1 << 20 {
            buf.resize(1 << 20, 0);
        }
        // READ_USN_JOURNAL_DATA_V1（V0 と同じ並びの後ろに MinMajorVersion / MaxMajorVersion）
        let mut input = [0u8; 48];
        input[0..8].copy_from_slice(&start.to_le_bytes());
        input[8..12].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // ReasonMask
        input[12..16].copy_from_slice(&0u32.to_le_bytes()); // ReturnOnlyOnClose
        input[16..24].copy_from_slice(&0u64.to_le_bytes()); // Timeout
        input[24..32].copy_from_slice(&0u64.to_le_bytes()); // BytesToWaitFor
        input[32..40].copy_from_slice(&journal_id.to_le_bytes());
        input[40..42].copy_from_slice(&2u16.to_le_bytes());
        input[42..44].copy_from_slice(&self.max_major.to_le_bytes());
        let n = ioctl(&self.file, FSCTL_READ_USN_JOURNAL, &input, buf)?;
        if n < 8 {
            return Ok((start, Vec::new()));
        }
        let next = i64::from_le_bytes(buf[0..8].try_into().unwrap());
        let mut events = Vec::new();
        // V3（128bit ID）は下位 64bit を FRN として扱う（NTFS の 128bit ID は下位 64bit が FRN）
        crate::enumerate::parse_usn_records(&buf[8..n], |r, usn, reason| {
            let (frn, parent_frn) = match &self.refs {
                Some(a) => a.translate(r.id, r.parent),
                None => (r.id as u64, r.parent as u64),
            };
            events.push(UsnEvent { frn, parent_frn, usn, reason, attrs: r.attrs, name: r.name });
        });
        Ok((next, events))
    }

    /// FRN（シーケンス込み 64bit）の MFT レコードを 1 件読む。使用中でない/一致しなければ None。
    pub fn read_file_record(&self, frn: u64) -> Option<MftEntry> {
        // 入力はレコード番号のみ（上位 16bit のシーケンスを含めると失敗する実装がある）。
        // シーケンスは解釈後に照合する。
        let input = ((frn & 0x0000_FFFF_FFFF_FFFF) as i64).to_le_bytes();
        let mut out = vec![0u8; 12 + self.record_size];
        static MISS_LOG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let diag = |why: &str| {
            if MISS_LOG.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 10 {
                log::warn!("MFT レコード {frn:#x} を読めない: {why}");
            }
        };
        let n = match ioctl(&self.file, FSCTL_GET_NTFS_FILE_RECORD, &input, &mut out) {
            Ok(n) => n,
            Err(e) => {
                diag(&format!("ioctl error {e}"));
                return None;
            }
        };
        if n < 12 {
            diag("応答が短い");
            return None;
        }
        let got = u64::from_le_bytes(out[0..8].try_into().unwrap());
        let len = u32::from_le_bytes(out[8..12].try_into().unwrap()) as usize;
        // 要求した番号が未使用だと直近の使用中レコードが返るので、番号一致を検証する
        if got & 0x0000_FFFF_FFFF_FFFF != frn & 0x0000_FFFF_FFFF_FFFF || len < 48 || 12 + len > out.len() {
            diag(&format!("番号不一致または長さ異常 (got {got:#x}, len {len})"));
            return None;
        }
        let boot = BootInfo {
            bytes_per_sector: 512,
            bytes_per_cluster: 4096,
            mft_lcn: 0,
            bytes_per_record: self.record_size as u32,
            total_sectors: 0,
        };
        let parser = MftRecordParser::new(&boot);
        let rec = &mut out[12..12 + len];
        let Some(e) = parser.parse(rec, got & 0x0000_FFFF_FFFF_FFFF) else {
            diag("レコードを解釈できない（未使用か破損）");
            return None;
        };
        if e.seq != (frn >> 48) as u16 {
            diag(&format!("連番不一致 (record seq {}, frn seq {})", e.seq, frn >> 48));
            return None;
        }
        Some(e)
    }

    pub fn record_size(&self) -> usize {
        self.record_size
    }

    /// 受け付ける USN レコードの最大版を変える（ReFS では 3）。
    pub fn set_max_major(&mut self, v: u16) {
        self.max_major = v.clamp(2, 3);
    }
}
