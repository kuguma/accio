//! ReFS 向けの適応層。
//!
//! 索引層は「レコード番号（48bit）＋連番（16bit）」の FRN と、番号で引ける密な表を前提にしている。
//! ReFS のファイル ID は 64/128bit で疎なので、ID に合成レコード番号を割り当てて同じ経路に流す。
//! - 列挙（`enum_usn_data`）→ `to_mft_entries` で `MftEntry` 列にし、通常のビルダーで組み立てる
//! - ジャーナル（`UsnJournal::read`）は `RefsAdapter::translate` で ID を合成 FRN に直す
//! - `read_file_record` は `OpenFileById` で名前・親・サイズ・日時を取る
//!
//! 合成番号 → ID の表は索引ファイル（`refs_ids`）に保存し、起動時に復元する。

use crate::enumerate::{EnumRecord, FileIdReader};
use crate::mft::{MftEntry, MftName, FILE_ATTR_DIRECTORY};
use std::collections::HashMap;
use std::sync::Mutex;

/// ルートに割り当てる合成レコード番号（NTFS のルート MFT レコード番号に合わせる）。
pub const ROOT_RECNO: u64 = 5;
/// 通常のエントリに割り当て始める番号（NTFS のメタファイル領域を避ける）。
pub const FIRST_RECNO: u64 = 16;

/// 合成レコード番号 ⇔ ファイル ID。
#[derive(Debug, Default, Clone)]
pub struct IdMap {
    /// 番号 → ID（未使用は 0）
    ids: Vec<u128>,
    map: HashMap<u128, u64>,
}

impl IdMap {
    /// 保存形式（lo, hi の u64 列）から復元する。
    pub fn from_saved(pairs: &[u64]) -> Self {
        let ids: Vec<u128> = pairs.as_chunks::<2>().0.iter().map(|c| c[0] as u128 | ((c[1] as u128) << 64)).collect();
        let map = ids.iter().enumerate().filter(|(_, &id)| id != 0).map(|(recno, &id)| (id, recno as u64)).collect();
        Self { ids, map }
    }
    pub fn to_saved(&self) -> Vec<u64> {
        self.ids.iter().flat_map(|&id| [id as u64, (id >> 64) as u64]).collect()
    }
    pub fn set_root(&mut self, id: u128) {
        self.assign(ROOT_RECNO, id);
    }
    fn assign(&mut self, recno: u64, id: u128) {
        let r = recno as usize;
        if self.ids.len() <= r {
            self.ids.resize(r + 1, 0);
        }
        self.ids[r] = id;
        self.map.insert(id, recno);
    }
    /// ID の合成番号。未知なら新しく割り当てる。
    pub fn recno_of(&mut self, id: u128) -> u64 {
        if let Some(&r) = self.map.get(&id) {
            return r;
        }
        let r = (self.ids.len() as u64).max(FIRST_RECNO);
        self.assign(r, id);
        r
    }
    pub fn get(&self, id: u128) -> Option<u64> {
        self.map.get(&id).copied()
    }
    pub fn id_of(&self, recno: u64) -> Option<u128> {
        self.ids.get(recno as usize).copied().filter(|&id| id != 0)
    }
    /// 表の長さ（ビルダーの `mft_records` に使う）
    pub fn len(&self) -> usize {
        self.ids.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

/// 列挙結果から `MftEntry` 列を作る。ルートは `root_hint` か、「親として現れるが自身は列挙されない ID」。
/// 戻り値は (エントリ列, ルート ID)。
pub fn to_mft_entries(records: &[EnumRecord], ids: &mut IdMap) -> (Vec<MftEntry>, Option<u128>) {
    to_mft_entries_with_root(records, ids, None)
}

pub fn to_mft_entries_with_root(records: &[EnumRecord], ids: &mut IdMap, root_hint: Option<u128>) -> (Vec<MftEntry>, Option<u128>) {
    let present: std::collections::HashSet<u128> = records.iter().map(|r| r.id).collect();
    let root = root_hint.or_else(|| records.iter().map(|r| r.parent).find(|p| !present.contains(p)));
    if let Some(root) = root {
        ids.set_root(root);
    }
    let mut out = Vec::with_capacity(records.len());
    for r in records {
        if r.name.is_empty() {
            continue;
        }
        let recno = ids.recno_of(r.id);
        let parent = ids.recno_of(r.parent);
        let is_dir = r.attrs & FILE_ATTR_DIRECTORY != 0;
        out.push(MftEntry {
            record: recno,
            seq: 0,
            base_record: 0,
            is_dir,
            has_attr_list: false,
            attrs: r.attrs,
            size: Some(if is_dir { 0 } else { r.size }),
            alloc_size: None,
            mtime: r.mtime,
            ctime: r.ctime,
            atime: r.atime,
            names: vec![MftName { parent, parent_seq: 0, name: r.name.clone() }],
        });
    }
    (out, root)
}

/// ジャーナルとレコード読みの ReFS 実装。
pub struct RefsAdapter {
    reader: FileIdReader,
    pub ids: Mutex<IdMap>,
}

impl RefsAdapter {
    pub fn new(reader: FileIdReader, ids: IdMap) -> Self {
        Self { reader, ids: Mutex::new(ids) }
    }
    pub fn reader(&self) -> &FileIdReader {
        &self.reader
    }
    /// USN レコードの (ID, 親 ID) → (合成 FRN, 合成親 FRN)。未知の ID には新しい番号を割り当てる。
    pub fn translate(&self, id: u128, parent: u128) -> (u64, u64) {
        let mut m = self.ids.lock().unwrap_or_else(|e| e.into_inner());
        (m.recno_of(id), m.recno_of(parent))
    }
    /// 合成 FRN から `MftEntry` を組む（名前・親・サイズ・日時）。
    pub fn read_file_record(&self, frn: u64) -> Option<MftEntry> {
        let recno = frn & 0x0000_FFFF_FFFF_FFFF;
        let id = self.ids.lock().unwrap_or_else(|e| e.into_inner()).id_of(recno)?;
        let info = self.reader.info(id);
        let path = self.reader.path(id);
        if info.is_none() || path.is_none() {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 20 {
                log::warn!("ReFS: ID {id:#x}（合成番号 {recno}）のレコードを読めない: info={} path={:?}", info.is_some(), path);
            }
        }
        let info = info?;
        let path = path?;
        let trimmed = path.trim_end_matches('\\');
        let (parent_path, name) = match trimmed.rsplit_once('\\') {
            Some((p, n)) => (p.to_string(), n.to_string()),
            None => return None,
        };
        let parent_id = self.reader.id_of_path(&format!("{parent_path}\\"))?;
        let parent = self.ids.lock().unwrap_or_else(|e| e.into_inner()).recno_of(parent_id);
        let is_dir = info.attrs & FILE_ATTR_DIRECTORY != 0;
        Some(MftEntry {
            record: recno,
            seq: 0,
            base_record: 0,
            is_dir,
            has_attr_list: false,
            attrs: info.attrs,
            size: Some(if is_dir { 0 } else { info.size }),
            alloc_size: None,
            mtime: info.mtime,
            ctime: info.ctime,
            atime: info.atime,
            names: vec![MftName { parent, parent_seq: 0, name }],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: u128, parent: u128, attrs: u32, name: &str) -> EnumRecord {
        EnumRecord { id, parent, attrs, name: name.into(), ..Default::default() }
    }

    #[test]
    fn assigns_recnos_and_detects_root() {
        let root = 0xAAAA_u128 << 64 | 1;
        let records = vec![
            rec(100, 7, 0x20, "deep.txt"), // 親が後から来る
            rec(7, root, 0x10, "dir"),
            rec(8, root, 0x20, "top.txt"),
            rec(9, root, 0x20, ""), // 名前なしは捨てる
        ];
        let mut ids = IdMap::default();
        let (entries, r) = to_mft_entries(&records, &mut ids);
        assert_eq!(r, Some(root));
        assert_eq!(ids.get(root), Some(ROOT_RECNO));
        assert_eq!(entries.len(), 3);
        let dir = entries.iter().find(|e| e.names[0].name == "dir").unwrap();
        let deep = entries.iter().find(|e| e.names[0].name == "deep.txt").unwrap();
        assert!(dir.is_dir);
        assert_eq!(deep.names[0].parent, dir.record);
        assert_eq!(dir.names[0].parent, ROOT_RECNO);
        assert!(dir.record >= FIRST_RECNO);
        // 保存形式を往復しても同じ対応
        let again = IdMap::from_saved(&ids.to_saved());
        assert_eq!(again.get(100), ids.get(100));
        assert_eq!(again.id_of(dir.record), Some(7));
        assert_eq!(again.len(), ids.len());
    }
}
