//! 位置指定読み取りの抽象。生ボリューム（`\\?\Volume{...}`）と通常ファイル（テスト用イメージ）を同じ経路で扱う。

use anyhow::{anyhow, Result};
use std::fs::File;
use std::os::windows::fs::FileExt;
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use windows_sys::Win32::Foundation::{GetLastError, GENERIC_READ, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_NO_BUFFERING, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};

/// 任意オフセットから `buf` を満たす読み取り。複数スレッドから同時に呼べる。
pub trait PositionalRead: Send + Sync {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()>;
}

pub struct FileReader {
    file: File,
    /// 生ボリュームは `FILE_FLAG_NO_BUFFERING` で開くので読み取りはこの境界に揃える
    align: u64,
}

impl FileReader {
    pub fn open_path(path: &Path) -> Result<Self> {
        Ok(Self { file: File::open(path)?, align: 1 })
    }

    /// `\\?\Volume{GUID}` 形式（末尾の `\` なし）または `\\.\C:` を開く。管理者権限が必要。
    pub fn open_volume(volume: &str) -> Result<Self> {
        let wide: Vec<u16> = volume.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: wide は NUL 終端。戻り値は直後に検査する。
        let h = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_NO_BUFFERING,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            let e = unsafe { GetLastError() };
            return Err(anyhow!("ボリューム {volume} を開けない (Win32 error {e}; 管理者権限が必要)"));
        }
        // SAFETY: 有効なハンドルの所有権を File に移す。
        let file = unsafe { File::from_raw_handle(h as _) };
        Ok(Self { file, align: 4096 })
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let mut done = 0;
        while done < buf.len() {
            let n = self.file.seek_read(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(anyhow!("EOF (offset {offset})"));
            }
            done += n;
        }
        Ok(())
    }
}

impl PositionalRead for FileReader {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let a = self.align;
        let aligned = offset.is_multiple_of(a) && (buf.len() as u64).is_multiple_of(a) && (buf.as_ptr() as u64).is_multiple_of(a);
        if a == 1 || aligned {
            return self.read_exact_at(offset, buf);
        }
        // 非バッファ I/O: 揃った一時バッファに読んで切り出す（小さな読み取り専用の経路）
        let start = offset & !(a - 1);
        let end = (offset + buf.len() as u64 + a - 1) & !(a - 1);
        let mut tmp = AlignedBuf::new((end - start) as usize, a as usize);
        self.read_exact_at(start, tmp.as_mut())?;
        let skip = (offset - start) as usize;
        buf.copy_from_slice(&tmp.as_ref()[skip..skip + buf.len()]);
        Ok(())
    }
}

/// 境界揃えのヒープバッファ（FILE_FLAG_NO_BUFFERING の要件）。
pub struct AlignedBuf {
    ptr: *mut u8,
    len: usize,
    layout: std::alloc::Layout,
}
// SAFETY: 所有権を持つ生ポインタの単純なラッパ。
unsafe impl Send for AlignedBuf {}
unsafe impl Sync for AlignedBuf {}
impl AlignedBuf {
    pub fn new(len: usize, align: usize) -> Self {
        let layout = std::alloc::Layout::from_size_align(len.max(align), align).unwrap();
        // SAFETY: layout は非ゼロサイズ。
        let ptr = unsafe { std::alloc::alloc(layout) };
        assert!(!ptr.is_null());
        Self { ptr, len, layout }
    }
}
impl AsRef<[u8]> for AlignedBuf {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: ptr は len バイト以上の確保済み領域。
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}
impl AsMut<[u8]> for AlignedBuf {
    fn as_mut(&mut self) -> &mut [u8] {
        // SAFETY: 同上、かつ &mut self で排他。
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}
impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // SAFETY: new と同じ layout で確保したポインタ。
        unsafe { std::alloc::dealloc(self.ptr, self.layout) }
    }
}
