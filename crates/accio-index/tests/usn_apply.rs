//! USN イベント列の適用（作成・更新・リネーム・移動・削除・親の遅延生成）を、
//! 偽の MFT レコード源で検証する。
use accio_index::{update, VolumeIndex, VolumeMeta, NONE};
use accio_ntfs::mft::{MftEntry, MftName};
use accio_ntfs::usn::reason::*;
use accio_ntfs::{RecordSource, UsnEvent};
use parking_lot::RwLock;
use std::collections::HashMap;

const DIR: u32 = 0x10;

struct Fake(HashMap<u64, MftEntry>);
impl RecordSource for Fake {
    fn read_file_record(&self, frn: u64) -> Option<MftEntry> {
        self.0.get(&frn).cloned()
    }
}

fn frn(rec: u64, seq: u16) -> u64 {
    rec | ((seq as u64) << 48)
}

fn rec(frn_: u64, parent: u64, name: &str, is_dir: bool, size: u64, mtime: i64) -> MftEntry {
    MftEntry {
        record: frn_ & 0xFFFF_FFFF_FFFF,
        seq: (frn_ >> 48) as u16,
        is_dir,
        attrs: if is_dir { DIR } else { 0x20 },
        size: Some(size),
        mtime,
        names: vec![MftName { parent: parent & 0xFFFF_FFFF_FFFF, parent_seq: (parent >> 48) as u16, name: name.into() }],
        ..Default::default()
    }
}

fn ev(frn_: u64, parent: u64, name: &str, reason: u32, attrs: u32) -> UsnEvent {
    UsnEvent { frn: frn_, parent_frn: parent, usn: 0, reason, attrs, name: name.into() }
}

fn base() -> (RwLock<VolumeIndex>, Fake) {
    let mut v = VolumeIndex::new(VolumeMeta { display_root: "T:".into(), ..Default::default() });
    v.by_recno = vec![NONE; 1000];
    v.by_recno[5] = 0;
    let root = frn(5, 5);
    let d1 = frn(100, 1);
    let f1 = frn(101, 1);
    let d1_id = v.push_raw("docs", 0, DIR, 0, 0, 0, d1);
    v.push_raw("a.txt", d1_id, 0x20, 100, 1, 1, f1);
    v.recompute_dir_sizes();
    let mut m = HashMap::new();
    m.insert(d1, rec(d1, root, "docs", true, 0, 0));
    m.insert(f1, rec(f1, d1, "a.txt", false, 100, 1));
    (RwLock::new(v), Fake(m))
}

fn paths(v: &VolumeIndex) -> Vec<String> {
    (0..v.len() as u32).filter(|&i| !v.is_deleted(i)).map(|i| format!("{} {}", v.full_path(i), v.size[i as usize])).collect()
}

#[test]
fn create_update_rename_move_delete() {
    let (vol, mut src) = base();
    let d1 = frn(100, 1);
    let f1 = frn(101, 1);
    // 作成（CLOSE 前のレコードは無視される）
    let f2 = frn(102, 1);
    src.0.insert(f2, rec(f2, d1, "b.bin", false, 5000, 7));
    let st = update::apply(&vol, &src, &[ev(f2, d1, "b.bin", FILE_CREATE, 0x20), ev(f2, d1, "b.bin", FILE_CREATE | DATA_EXTEND | CLOSE, 0x20)]);
    assert_eq!((st.created, st.ignored), (1, 1));
    {
        let v = vol.read();
        assert!(paths(&v).contains(&r"T:\docs\b.bin 5000".to_string()));
        assert_eq!(v.size[1], 5100, "フォルダサイズに加算");
        assert_eq!(v.size[0], 5100);
    }
    // サイズ更新
    src.0.insert(f1, rec(f1, d1, "a.txt", false, 300, 9));
    update::apply(&vol, &src, &[ev(f1, d1, "a.txt", DATA_EXTEND | CLOSE, 0x20)]);
    {
        let v = vol.read();
        assert_eq!(v.size[2], 300);
        assert_eq!(v.mtime(2), accio_index::volume::secs_to_filetime(accio_index::volume::filetime_to_secs(9)));
        assert_eq!(v.size[1], 5300);
    }
    // リネーム（長くなる → overflow 経路）
    src.0.insert(f1, rec(f1, d1, "a-renamed-to-a-much-longer-name.txt", false, 300, 9));
    let st = update::apply(&vol, &src, &[ev(f1, d1, "a.txt", RENAME_OLD_NAME, 0x20), ev(f1, d1, "a-renamed-to-a-much-longer-name.txt", RENAME_NEW_NAME | CLOSE, 0x20)]);
    assert_eq!(st.renamed, 1);
    assert_eq!(vol.read().name(2), "a-renamed-to-a-much-longer-name.txt");
    // 新しいフォルダへ移動（親フォルダは索引に無く、レコード源から遅延生成される）
    let d2 = frn(200, 1);
    src.0.insert(d2, rec(d2, frn(5, 5), "newdir", true, 0, 0));
    let st = update::apply(&vol, &src, &[ev(f1, d2, "a-renamed-to-a-much-longer-name.txt", RENAME_NEW_NAME | CLOSE, 0x20)]);
    assert_eq!(st.renamed, 1);
    {
        let v = vol.read();
        let p = paths(&v);
        assert!(p.contains(&r"T:\newdir\a-renamed-to-a-much-longer-name.txt 300".to_string()), "{p:?}");
        assert!(p.contains(&r"T:\newdir 300".to_string()), "{p:?}");
        assert_eq!(v.size[1], 5000, "移動元のフォルダサイズから減算");
    }
    // 削除
    let st = update::apply(&vol, &src, &[ev(f2, d1, "b.bin", FILE_DELETE | CLOSE, 0x20)]);
    assert_eq!(st.deleted, 1);
    {
        let v = vol.read();
        assert!(!paths(&v).iter().any(|p| p.contains("b.bin")));
        assert_eq!(v.size[1], 0);
        assert_eq!(v.size[0], 300);
        assert_eq!(v.live_count(), 4); // root, docs, renamed, newdir
    }
    // 名前が短くなるリネームは主アリーナ内で上書きされ、旧名ではヒットしない
    let f3 = frn(102, 2); // 同じレコード番号の再利用（seq 違い）
    src.0.insert(f3, rec(f3, d1, "zz.log", false, 1, 1));
    update::apply(&vol, &src, &[ev(f3, d1, "zz.log", FILE_CREATE | CLOSE, 0x20)]);
    src.0.insert(f3, rec(f3, d1, "z.log", false, 1, 1));
    update::apply(&vol, &src, &[ev(f3, d1, "z.log", RENAME_NEW_NAME | CLOSE, 0x20)]);
    let v = vol.read();
    assert_eq!(v.name(v.len() as u32 - 1), "z.log");
    assert!(v.lower_name(v.len() as u32 - 1) == b"z.log");
}

#[test]
fn journal_records_changes() {
    let (vol, mut src) = base();
    let d1 = frn(100, 1);
    let f1 = frn(101, 1);
    let f2 = frn(102, 1);
    src.0.insert(f2, rec(f2, d1, "new.bin", false, 10, 7));
    src.0.insert(f1, rec(f1, d1, "renamed.txt", false, 100, 1));
    let mut log: Vec<update::Change> = Vec::new();
    update::apply_with_journal(
        &vol,
        &src,
        &[
            ev(f2, d1, "new.bin", FILE_CREATE | CLOSE, 0x20),
            ev(f1, d1, "renamed.txt", RENAME_NEW_NAME | CLOSE, 0x20),
            ev(f2, d1, "new.bin", FILE_DELETE | CLOSE, 0x20),
        ],
        &mut |c| log.push(c),
    );
    let kinds: Vec<u8> = log.iter().map(|c| c.kind).collect();
    assert_eq!(kinds, vec![update::CH_FILE_CREATE, update::CH_FILE_RENAME, update::CH_FILE_DELETE]);
    assert_eq!(log[0].old_name, "new.bin");
    assert_eq!(log[0].old_path, r"T:\docs");
    assert_eq!(log[1].old_name, "a.txt");
    assert_eq!(log[1].new_name, "renamed.txt");
}

#[test]
fn compact_after_updates_roundtrips() {
    let (vol, mut src) = base();
    let f1 = frn(101, 1);
    src.0.insert(f1, rec(f1, frn(100, 1), "a-very-long-replacement-name.txt", false, 100, 1));
    update::apply(&vol, &src, &[ev(f1, frn(100, 1), "a-very-long-replacement-name.txt", RENAME_NEW_NAME | CLOSE, 0x20)]);
    let mut v = vol.write();
    v.compact();
    assert!(v.overflow.is_empty());
    assert_eq!(v.name(2), "a-very-long-replacement-name.txt");
    assert!(v.name_off.windows(2).all(|w| w[0] <= w[1]));
}
