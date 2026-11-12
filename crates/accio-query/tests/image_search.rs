//! テストイメージを索引化し、検索構文の主要経路が正しい集合を返すことを確認する。
use accio_index::{builder::VolumeBuilder, Index, VolumeMeta};
use accio_ntfs::{scan_mft, FileReader, ScanOptions};
use accio_query::{search, Request, SearchOpts, Sort, SortKey};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

fn build() -> Index {
    let img = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/testfs1.ntfs");
    let r = FileReader::open_path(&img).unwrap();
    let meta = VolumeMeta { guid_path: "test".into(), display_root: "T:".into(), ..Default::default() };
    let b = VolumeBuilder::new(meta, 1024);
    scan_mft(&r, &ScanOptions { chunk_bytes: 8192, threads: 2 }, &AtomicBool::new(false), &|_, _| {}, &|v| b.push_batch(v)).unwrap();
    let vol = b.finish();
    Index::new(vec![vol])
}

fn q(idx: &Index, s: &str) -> Vec<String> {
    let req = Request { query: s.into(), opts: SearchOpts::default(), sort: Sort::default(), ..Default::default() };
    let vols = idx.snapshot();
    let r = search(&vols, &req, &|| false).unwrap();
    let g = vols[0].read();
    r.hits.iter().map(|h| g.full_path(h.id)).collect()
}

#[test]
fn end_to_end() {
    let idx = build();
    let vols = idx.snapshot();
    let g = vols[0].read();
    assert!(g.resolved);
    assert_eq!(g.live_count(), 1 + 4 + 1 + 512);
    drop(g);

    assert_eq!(q(&idx, "12345"), vec![r"T:\file-with-12345"]);
    assert_eq!(q(&idx, "FILE-WITH"), vec![r"T:\file-with-12345"]);
    assert_eq!(q(&idx, "case:FILE-WITH"), Vec::<String>::new());
    assert_eq!(q(&idx, "size:5"), vec![r"T:\file-with-12345"]);
    assert_eq!(q(&idx, "size:>400kb").len(), 2); // sparse-file とルート（フォルダサイズ）
    assert_eq!(q(&idx, "folder: many").len(), 1);
    assert_eq!(q(&idx, r"parent:T:\many_subdirs").len(), 512);
    assert_eq!(q(&idx, r"T:\many_subdirs\1").len(), 1 + 10 + 100); // 1, 10-19, 100-199 (path 部分一致)
    assert_eq!(q(&idx, "wfn:1??").len(), 100);
    assert_eq!(q(&idx, "*5 file:").len(), 1); // 末尾 5 のファイルのみ
    assert_eq!(q(&idx, r"empty: !ancestor:T:\many_subdirs").len(), 0);
    assert_eq!(q(&idx, "empty:").len(), 512);
    assert_eq!(q(&idx, r"regex:^\d{3}$").len(), 413);
    assert_eq!(q(&idx, "child:512").len(), 1);
    assert_eq!(q(&idx, "childcount:512").len(), 1);
    assert_eq!(q(&idx, "childfoldercount:>500").len(), 1);
    assert_eq!(q(&idx, "childfilecount:4").len(), 1); // ルート直下の 4 ファイル
    assert_eq!(q(&idx, "dupe:").len(), 0);
    // content: はテストイメージ上のパスを読めないので 0 件（読めない＝不一致）
    assert_eq!(q(&idx, "content:12345").len(), 0);
    assert_eq!(q(&idx, "file: content:12345").len(), 0);
    assert_eq!(q(&idx, "sizedupe:").len(), 0);
    assert_eq!(q(&idx, "a|b bytes").len(), 1);
    assert_eq!(q(&idx, "!folder: !file").len(), 0); // 全ファイル名が file を含む
    assert!(q(&idx, "dm:>2000").len() >= 5);
    // ディレクトリサイズ = 配下合計
    let vols = idx.snapshot();
    let g = vols[0].read();
    assert_eq!(g.size[0], 5 + 1000 + 500005);
    drop(g);
    // ソート
    let req = Request { query: "file".into(), opts: SearchOpts::default(), sort: Sort { key: SortKey::Size, ascending: false }, ..Default::default() };
    let r = search(&vols, &req, &|| false).unwrap();
    let g = vols[0].read();
    let sizes: Vec<u64> = r.hits.iter().map(|h| g.size[h.id as usize]).collect();
    assert!(sizes.windows(2).all(|w| w[0] >= w[1]));
}

#[test]
fn save_load_roundtrip() {
    let idx = build();
    let dir = std::env::temp_dir().join("accio-test");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("t.accio");
    {
        let vols = idx.snapshot();
        let mut g = vols[0].write();
        g.rename(1, "renamed-very-long-name-that-does-not-fit.txt");
        g.compact();
        accio_index::store::save(&g, &p).unwrap();
    }
    let v = accio_index::store::load(&p).unwrap();
    let vols = idx.snapshot();
    let g = vols[0].read();
    assert_eq!(v.len(), g.len());
    assert_eq!(v.name(1), "renamed-very-long-name-that-does-not-fit.txt");
    assert_eq!(v.arena, g.arena);
    assert_eq!(v.size, g.size);
    assert_eq!(v.live_count(), g.live_count());
}
