//! フォルダ索引（非 NTFS / ネットワーク / 任意のフォルダ向け）。
//!
//! ディレクトリ走査で VolumeIndex を組み立てる。リパースポイント（シンボリックリンク、
//! ジャンクション、マウントポイント）は降りない（循環防止）。アクセス拒否はそのサブツリーだけ諦める。
//! 変更追従は呼び出し側（ReadDirectoryChangesW → 再走査）。

use crate::volume::{VolumeIndex, VolumeMeta, ATTR_DIRECTORY};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// `root`（`D:\data` や `\\server\share\dir`）配下を走査する。`progress(entries)` を適宜呼ぶ。
pub fn scan_folder(root: &str, cancel: &AtomicBool, progress: &dyn Fn(usize)) -> std::io::Result<VolumeIndex> {
    let root_display = root.trim_end_matches('\\').to_string();
    let meta = VolumeMeta { guid_path: format!("folder:{root_display}"), display_root: root_display.clone(), ..Default::default() };
    let mut v = VolumeIndex::new(meta);
    v.resolved = true;
    let root_path = extended(&root_display);
    if !root_path.is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("{root_display} はフォルダではない")));
    }
    let mut stack: Vec<(PathBuf, u32)> = vec![(root_path, 0)];
    let mut next_frn: u64 = 16;
    let mut since_report = 0usize;
    while let Some((dir, parent)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            let ft = e.file_type().ok();
            let name = e.file_name().to_string_lossy().into_owned();
            let attrs = md.file_attributes();
            let is_dir = md.is_dir();
            let is_reparse = ft.map(|t| t.is_symlink()).unwrap_or(false) || attrs & 0x400 != 0;
            let size = if is_dir { 0 } else { md.len() };
            let mtime = md.last_write_time() as i64;
            let ctime = md.creation_time() as i64;
            let id = v.push_raw(&name, parent, attrs | if is_dir { ATTR_DIRECTORY } else { 0 }, size, mtime, ctime, next_frn);
            v.set_atime(id, md.last_access_time() as i64);
            next_frn += 1;
            if is_dir && !is_reparse {
                stack.push((e.path(), id));
            }
            since_report += 1;
            if since_report >= 20_000 {
                since_report = 0;
                progress(v.len());
            }
        }
    }
    v.recompute_dir_sizes();
    progress(v.len());
    Ok(v)
}

/// MAX_PATH を超えるために `\\?\` プレフィックスを付ける（UNC は `\\?\UNC\`）。
fn extended(p: &str) -> PathBuf {
    if p.starts_with(r"\\?\") {
        PathBuf::from(p)
    } else if let Some(rest) = p.strip_prefix(r"\\") {
        PathBuf::from(format!(r"\\?\UNC\{rest}"))
    } else {
        PathBuf::from(format!(r"\\?\{p}"))
    }
}

pub fn display_to_path(p: &str) -> &Path {
    Path::new(p)
}
