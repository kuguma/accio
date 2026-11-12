//! テスト用 NTFS イメージ（testdata/testfs1.ntfs: ntfs crate 由来, 512B セクタ, 2MiB）を
//! 生ボリュームと同じ経路でスキャンできることを確認する。
use accio_ntfs::{scan_mft, FileReader, MftEntry, ScanOptions};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

fn image() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/testfs1.ntfs")
}

#[test]
fn scans_test_image() {
    let r = FileReader::open_path(&image()).unwrap();
    let all = Mutex::new(Vec::<MftEntry>::new());
    let cancel = AtomicBool::new(false);
    let stats = scan_mft(&r, &ScanOptions { chunk_bytes: 4096, threads: 2 }, &cancel, &|_, _| {}, &|v| all.lock().unwrap().extend(v)).unwrap();
    let all = all.into_inner().unwrap();
    assert!(stats.total_records >= 16 + 517);
    let by_name = |n: &str| all.iter().find(|e| e.names.iter().any(|x| x.name == n)).cloned();
    let f = by_name("file-with-12345").expect("resident file");
    assert_eq!(f.size, Some(5));
    assert!(!f.is_dir);
    let f = by_name("1000-bytes-file").expect("nonresident file");
    assert_eq!(f.size, Some(1000));
    let f = by_name("sparse-file").unwrap();
    assert_eq!(f.size, Some(500005));
    let d = by_name("many_subdirs").unwrap();
    assert!(d.is_dir);
    let root = all.iter().find(|e| e.record == 5).unwrap();
    assert_eq!(root.names[0].name, ".");
    // many_subdirs 配下の 512 ディレクトリが親 FRN でつながる
    let n = all.iter().filter(|e| e.is_dir && e.names.iter().any(|x| x.parent == d.record)).count();
    assert_eq!(n, 512);
    // DOS 名は含まない（8.3 名前空間は除外）
    assert!(all.iter().all(|e| e.names.iter().all(|x| !x.name.contains('~') || x.name.len() > 12)));
    let e = by_name("empty-file").unwrap();
    assert_eq!(e.size, Some(0));
    assert!(e.mtime > 0);
}
