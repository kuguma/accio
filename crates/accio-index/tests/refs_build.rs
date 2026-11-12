//! ReFS 経路: USN 列挙レコード → 合成レコード番号 → 通常のビルダーで索引を組み、
//! 合成 FRN のイベントを通常の USN 反映で処理できることを確かめる（偽のレコード源）。

use accio_index::update::{apply, Change};
use accio_index::{VolumeBuilder, VolumeMeta};
use accio_ntfs::refs::{to_mft_entries, IdMap, ROOT_RECNO};
use accio_ntfs::usn::{reason, RecordSource, UsnEvent};
use accio_ntfs::{EnumRecord, MftEntry};
use parking_lot::RwLock;
use std::collections::HashMap;

fn rec(id: u128, parent: u128, attrs: u32, name: &str) -> EnumRecord {
    EnumRecord { id, parent, attrs, name: name.into(), ..Default::default() }
}

struct Fake(HashMap<u64, MftEntry>);
impl RecordSource for Fake {
    fn read_file_record(&self, frn: u64) -> Option<MftEntry> {
        self.0.get(&frn).cloned()
    }
}

#[test]
fn builds_tree_from_enumeration_and_applies_events() {
    let root = (7u128 << 64) | 0x1234;
    let records = vec![
        rec((9u128 << 64) | 1, (8u128 << 64) | 1, 0x20, "note.txt"),
        rec((8u128 << 64) | 1, root, 0x10, "docs"),
        rec((8u128 << 64) | 2, root, 0x20, "readme.md"),
    ];
    let mut ids = IdMap::default();
    let (entries, r) = to_mft_entries(&records, &mut ids);
    assert_eq!(r, Some(root));
    let n = ids.len() as u64 + 16;
    let b = VolumeBuilder::new(VolumeMeta { display_root: "R:".into(), guid_path: "refs-test".into(), ..Default::default() }, n);
    b.push_batch(entries);
    let vol = b.finish();
    {
        let g = vol.read();
        let paths: Vec<String> = (0..g.len() as u32).map(|i| g.full_path(i)).collect();
        assert!(paths.contains(&"R:\\docs\\note.txt".to_string()), "{paths:?}");
        assert!(paths.contains(&"R:\\readme.md".to_string()));
        assert_eq!(g.entry_by_recno(ROOT_RECNO), Some(0));
    }
    // 新規作成イベント（合成 FRN）: レコード源は OpenFileById 相当の偽物
    let new_id = (9u128 << 64) | 2;
    let docs_recno = ids.get((8u128 << 64) | 1).unwrap();
    let new_recno = ids.recno_of(new_id);
    let mut fake = HashMap::new();
    fake.insert(
        new_recno,
        MftEntry {
            record: new_recno,
            attrs: 0x20,
            size: Some(42),
            mtime: 132_000_000_000_000_000,
            names: vec![accio_ntfs::mft::MftName { parent: docs_recno, parent_seq: 0, name: "new.txt".into() }],
            ..Default::default()
        },
    );
    let vol = RwLock::new(parking_lot::RwLock::into_inner(match std::sync::Arc::try_unwrap(vol) {
        Ok(v) => v,
        Err(_) => panic!("shared"),
    }));
    let ev = UsnEvent { frn: new_recno, parent_frn: docs_recno, usn: 1, reason: reason::FILE_CREATE | reason::CLOSE, attrs: 0x20, name: "new.txt".into() };
    let st = apply(&vol, &Fake(fake), &[ev]);
    assert_eq!(st.created, 1, "{st:?}");
    let g = vol.read();
    let i = (0..g.len() as u32).find(|&i| g.name(i) == "new.txt").unwrap();
    assert_eq!(g.full_path(i), "R:\\docs\\new.txt");
    assert_eq!(g.size[i as usize], 42);
    let _: Option<Change> = None;
}
