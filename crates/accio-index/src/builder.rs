//! MFT エントリ列から VolumeIndex を組み立てる。
//!
//! 入力はスキャナから順不同・並列に届く。構築中は `parent` 列に親の MFT レコード番号を仮置きし、
//! 全件到着後に EntryId へ解決する（`VolumeIndex::resolved` が false の間、パス表示は不可）。
//! 構築中でも RwLock の読み取り側からは到着済みエントリが検索できる。

use crate::volume::{VolumeIndex, VolumeMeta, ATTR_DIRECTORY, NONE};
use accio_ntfs::MftEntry;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
struct ExtInfo {
    size: Option<u64>,
    names: Vec<accio_ntfs::mft::MftName>,
}

pub struct VolumeBuilder {
    pub vol: Arc<RwLock<VolumeIndex>>,
    ext: Mutex<HashMap<u64, ExtInfo>>,
}

const ROOT_RECNO: u64 = 5;
const EXTEND_RECNO: u64 = 11;

impl VolumeBuilder {
    pub fn new(meta: VolumeMeta, mft_records: u64) -> Self {
        let mut v = VolumeIndex::new(meta);
        v.meta.mft_records = mft_records;
        v.by_recno = vec![NONE; mft_records as usize];
        v.by_recno[ROOT_RECNO as usize] = 0;
        v.resolved = false;
        Self { vol: Arc::new(RwLock::new(v)), ext: Mutex::new(HashMap::new()) }
    }

    /// スキャナからの 1 バッチ。短い書き込みロックで追記する。
    pub fn push_batch(&self, batch: Vec<MftEntry>) {
        let mut ext_local: Vec<(u64, ExtInfo)> = Vec::new();
        let mut v = self.vol.write();
        for e in batch {
            if e.base_record != 0 {
                if e.size.is_some() || !e.names.is_empty() {
                    ext_local.push((e.base_record, ExtInfo { size: e.size, names: e.names }));
                }
                continue;
            }
            if e.record < 16 && e.record != ROOT_RECNO {
                continue;
            }
            if e.record == ROOT_RECNO {
                v.attrs[0] = e.attrs | ATTR_DIRECTORY;
                v.mtime_s[0] = crate::volume::filetime_to_secs(e.mtime);
                v.ctime_s[0] = crate::volume::filetime_to_secs(e.ctime);
                v.atime_s[0] = crate::volume::filetime_to_secs(e.atime);
                continue;
            }
            let frn = e.record | ((e.seq as u64) << 48);
            for n in &e.names {
                // $Extend 配下のメタファイル（$UsnJrnl 等）だけを除外する。$RECYCLE.BIN は通常のフォルダ。
                if n.parent == EXTEND_RECNO && n.name.starts_with('$') {
                    continue;
                }
                let id = v.push_raw(&n.name, n.parent as u32, e.attrs, e.size.unwrap_or(0), e.mtime, e.ctime, frn);
                v.set_atime(id, e.atime);
            }
        }
        drop(v);
        if !ext_local.is_empty() {
            let mut m = self.ext.lock();
            for (k, info) in ext_local {
                let slot = m.entry(k).or_default();
                if info.size.is_some() {
                    slot.size = info.size;
                }
                slot.names.extend(info.names);
            }
        }
    }

    /// 全件到着後: 拡張レコードの統合、親解決、フォルダサイズ集計。
    pub fn finish(self) -> Arc<RwLock<VolumeIndex>> {
        let ext = std::mem::take(&mut *self.ext.lock());
        let mut v = self.vol.write();
        for (base, info) in ext {
            if let Some(id) = v.entry_by_recno(base) {
                if let Some(s) = info.size {
                    // 同一 FRN の全名（ハードリンク）のうち先頭のみに反映
                    v.size[id as usize] = s;
                }
            } else if !info.names.is_empty() {
                // 基底レコードに名前がなく拡張側だけにある稀なケース: 追記
                let frn = base;
                for n in info.names {
                    v.push_raw(&n.name, n.parent as u32, 0, info.size.unwrap_or(0), 0, 0, frn);
                }
            }
        }
        // 親 recno → EntryId
        let n = v.len();
        for i in 1..n {
            let p = v.parent[i] as u64;
            v.parent[i] = v.entry_by_recno(p).unwrap_or(NONE);
        }
        v.recompute_dir_sizes();
        v.resolved = true;
        drop(v);
        self.vol
    }
}
