//! USN ジャーナルのイベントを索引へ適用する。
//!
//! CLOSE ビットの立ったレコードだけを処理する（理由ビットはクローズまで累積するため、
//! 1 回のクローズで作成・リネーム・データ変更をまとめて反映できる）。
//! サイズ・日付は FSCTL_GET_NTFS_FILE_RECORD で該当 MFT レコードだけを読む（ファイルは開かない →
//! クラウドプレースホルダーのハイドレーションを誘発しない）。

use crate::volume::{VolumeIndex, ATTR_DIRECTORY, NONE};
use accio_ntfs::usn::reason;
use accio_ntfs::{RecordSource, UsnEvent};
use parking_lot::RwLock;

const RECNO_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// 索引ジャーナルの 1 件（Everything 1.5 の Index Journal 相当）。
#[derive(Debug, Clone)]
pub struct Change {
    /// 1 FOLDER_CREATE 2 FOLDER_DELETE 3 FOLDER_RENAME 4 FOLDER_MOVE 5 FOLDER_MODIFY 6 FILE_CREATE 7 FILE_DELETE 8 FILE_RENAME 9 FILE_MOVE 10 FILE_MODIFY
    pub kind: u8,
    /// 変更前の親パスと名前
    pub old_path: String,
    pub old_name: String,
    /// リネーム/移動後の親パスと名前
    pub new_path: String,
    pub new_name: String,
    pub size: u64,
    pub mtime: i64,
    pub ctime: i64,
    pub atime: i64,
    pub attrs: u32,
}

pub const CH_FOLDER_CREATE: u8 = 1;
pub const CH_FOLDER_DELETE: u8 = 2;
pub const CH_FOLDER_RENAME: u8 = 3;
pub const CH_FOLDER_MOVE: u8 = 4;
pub const CH_FOLDER_MODIFY: u8 = 5;
pub const CH_FILE_CREATE: u8 = 6;
pub const CH_FILE_DELETE: u8 = 7;
pub const CH_FILE_RENAME: u8 = 8;
pub const CH_FILE_MOVE: u8 = 9;
pub const CH_FILE_MODIFY: u8 = 10;

fn change_of(v: &VolumeIndex, i: u32, kind_file: u8, kind_dir: u8) -> Change {
    let is_dir = v.is_dir(i);
    Change {
        kind: if is_dir { kind_dir } else { kind_file },
        old_path: v.parent_path(i),
        old_name: v.name(i).to_string(),
        new_path: String::new(),
        new_name: String::new(),
        size: if is_dir { u64::MAX } else { v.size[i as usize] },
        mtime: v.mtime(i),
        ctime: v.ctime(i),
        atime: v.atime(i),
        attrs: v.attrs[i as usize],
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ApplyStats {
    pub created: u32,
    pub deleted: u32,
    pub renamed: u32,
    pub updated: u32,
    pub ignored: u32,
    /// MFT レコードの再読み取りに失敗した回数（サイズ・日付が欠ける）
    pub record_miss: u32,
}

fn name_eq_ci(v: &VolumeIndex, i: u32, name: &str) -> bool {
    v.lower_name(i) == crate::volume::lowercase(name).as_bytes()
}

/// `frn` の親ディレクトリエントリを得る。索引になければ MFT から辿って作る（深さ上限あり）。
fn ensure_dir(v: &mut VolumeIndex, j: &dyn RecordSource, frn: u64, depth: u32) -> Option<u32> {
    if frn & RECNO_MASK == 5 {
        return Some(0);
    }
    if let Some(id) = v.entry_by_recno(frn & RECNO_MASK) {
        if v.frn(id) == frn && v.is_dir(id) {
            return Some(id);
        }
    }
    if depth > 64 {
        return None;
    }
    let rec = j.read_file_record(frn)?;
    if !rec.is_dir {
        return None;
    }
    let n = rec.names.first()?;
    let pfrn = n.parent | ((n.parent_seq as u64) << 48);
    let parent = ensure_dir(v, j, pfrn, depth + 1)?;
    let id = v.push_raw(&n.name, parent, rec.attrs | ATTR_DIRECTORY, 0, rec.mtime, rec.ctime, frn);
    v.set_atime(id, rec.atime);
    Some(id)
}

/// 同一 FRN・同名のエントリを探す。先頭エントリが別名（ハードリンク）なら全走査する。
fn find_entry(v: &VolumeIndex, frn: u64, name: &str) -> Option<u32> {
    let first = v.entry_by_recno(frn & RECNO_MASK)?;
    if v.frn(first) != frn {
        return None;
    }
    if name_eq_ci(v, first, name) {
        return Some(first);
    }
    v.entries_by_recno(frn & RECNO_MASK).into_iter().find(|&i| v.frn(i) == frn && !v.is_deleted(i) && name_eq_ci(v, i, name))
}

pub fn apply(vol: &RwLock<VolumeIndex>, j: &dyn RecordSource, events: &[UsnEvent]) -> ApplyStats {
    apply_with_journal(vol, j, events, &mut |_| {})
}

/// `apply` に加えて、反映した変更を `sink` へ流す（索引ジャーナル用）。
pub fn apply_with_journal(vol: &RwLock<VolumeIndex>, j: &dyn RecordSource, events: &[UsnEvent], sink: &mut dyn FnMut(Change)) -> ApplyStats {
    let mut st = ApplyStats::default();
    if events.is_empty() {
        return st;
    }
    let mut v = vol.write();
    for e in events {
        if e.reason & reason::CLOSE == 0 {
            st.ignored += 1;
            continue;
        }
        let r = e.reason;
        if r & reason::FILE_DELETE != 0 {
            // 削除: 名前一致のエントリを消す。見つからなければ無視。
            let target = find_entry(&v, e.frn, &e.name).or_else(|| {
                // リネーム→削除が同一クローズに畳まれた場合は旧名で載っている
                let first = v.entry_by_recno(e.frn & RECNO_MASK)?;
                (v.frn(first) == e.frn).then_some(first)
            });
            if let Some(i) = target {
                sink(change_of(&v, i, CH_FILE_DELETE, CH_FOLDER_DELETE));
                let sz = v.size[i as usize] as i64;
                v.adjust_ancestors(i, -sz);
                v.mark_deleted(i);
                st.deleted += 1;
            } else {
                st.ignored += 1;
            }
            continue;
        }

        let existing_same_name = find_entry(&v, e.frn, &e.name);
        let existing_any = v.entry_by_recno(e.frn & RECNO_MASK).filter(|&i| v.frn(i) == e.frn);

        if let Some(i) = existing_same_name {
            // 既存エントリ: 親の移動 / データ・属性更新
            let parent = match ensure_dir(&mut v, j, e.parent_frn, 0) {
                Some(p) => p,
                None => {
                    st.ignored += 1;
                    continue;
                }
            };
            if v.parent[i as usize] != parent {
                let mut c = change_of(&v, i, CH_FILE_MOVE, CH_FOLDER_MOVE);
                let sz = v.size[i as usize] as i64;
                v.adjust_ancestors(i, -sz);
                v.parent[i as usize] = parent;
                if v.is_dir(i) {
                    v.dir_epoch += 1;
                }
                v.adjust_ancestors(i, sz);
                c.new_path = v.parent_path(i);
                c.new_name = v.name(i).to_string();
                sink(c);
                st.renamed += 1;
            }
            if r & (reason::DATA_OVERWRITE | reason::DATA_EXTEND | reason::DATA_TRUNCATION | reason::BASIC_INFO_CHANGE) != 0 {
                let rec = j.read_file_record(e.frn);
                if rec.is_none() {
                    st.record_miss += 1;
                }
                if let Some(rec) = rec {
                    if !rec.is_dir {
                        let new = rec.size.unwrap_or(v.size[i as usize]);
                        let delta = new as i64 - v.size[i as usize] as i64;
                        v.size[i as usize] = new;
                        v.adjust_ancestors(i, delta);
                    }
                    v.set_mtime(i, rec.mtime);
                    v.attrs[i as usize] = rec.attrs | (v.attrs[i as usize] & ATTR_DIRECTORY);
                }
                sink(change_of(&v, i, CH_FILE_MODIFY, CH_FOLDER_MODIFY));
                st.updated += 1;
            }
            continue;
        }

        if r & reason::RENAME_NEW_NAME != 0 {
            if let Some(i) = existing_any {
                // 同一 FRN で名前が違う: リネーム（ハードリンクの別名作成は HARD_LINK_CHANGE で来る）
                if r & reason::HARD_LINK_CHANGE == 0 {
                    let parent = match ensure_dir(&mut v, j, e.parent_frn, 0) {
                        Some(p) => p,
                        None => {
                            st.ignored += 1;
                            continue;
                        }
                    };
                    let moved = v.parent[i as usize] != parent;
                    let mut c = change_of(&v, i, if moved { CH_FILE_MOVE } else { CH_FILE_RENAME }, if moved { CH_FOLDER_MOVE } else { CH_FOLDER_RENAME });
                    v.rename(i, &e.name);
                    if moved {
                        let sz = v.size[i as usize] as i64;
                        v.adjust_ancestors(i, -sz);
                        v.parent[i as usize] = parent;
                        if v.is_dir(i) {
                            v.dir_epoch += 1;
                        }
                        v.adjust_ancestors(i, sz);
                    }
                    c.new_path = v.parent_path(i);
                    c.new_name = v.name(i).to_string();
                    sink(c);
                    st.renamed += 1;
                    continue;
                }
            }
        }

        // 新規（作成、またはハードリンクの新しい名前）
        if e.parent_frn & RECNO_MASK == 11 && e.name.starts_with('$') {
            st.ignored += 1;
            continue;
        }
        let parent = match ensure_dir(&mut v, j, e.parent_frn, 0) {
            Some(p) => p,
            None => {
                st.ignored += 1;
                continue;
            }
        };
        let rec = j.read_file_record(e.frn);
        if rec.is_none() {
            st.record_miss += 1;
        }
        let (attrs, size, mtime, ctime, atime) = match &rec {
            Some(rec) => (rec.attrs, rec.size.unwrap_or(0), rec.mtime, rec.ctime, rec.atime),
            None => (e.attrs, 0, 0, 0, 0),
        };
        if rec.is_none() && e.attrs == 0 {
            st.ignored += 1;
            continue;
        }
        let is_dir = attrs & ATTR_DIRECTORY != 0 || e.attrs & ATTR_DIRECTORY != 0;
        let attrs = if is_dir { attrs | ATTR_DIRECTORY } else { attrs };
        let id = v.push_raw(&e.name, parent, attrs, if is_dir { 0 } else { size }, mtime, ctime, e.frn);
        v.set_atime(id, atime);
        if !is_dir {
            v.adjust_ancestors(id, size as i64);
        }
        sink(change_of(&v, id, CH_FILE_CREATE, CH_FOLDER_CREATE));
        st.created += 1;
    }
    let _ = NONE;
    st
}
