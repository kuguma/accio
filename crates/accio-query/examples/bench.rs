//! 合成データで検索・ソート・永続化の所要時間を測る。`cargo run --release -p accio-query --example bench -- 15000000`
use accio_index::{Index, VolumeIndex, VolumeMeta};
use accio_query::{search, Request, Sort, SortKey};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1_000_000);
    let words = ["report", "photo", "Backup", "node_modules", "src", "main", "index", "Cargo", "README", "Program", "Windows", "system32", "user", "Documents", "データ", "画像", "設計書", "2024", "final", "draft", "v2", "lib", "test", "build", "cache"];
    let exts = ["txt", "rs", "js", "png", "jpg", "dll", "exe", "h", "cpp", "json", "md", "log", "", "pdf", "zip"];
    let t0 = Instant::now();
    let mut v = VolumeIndex::new(VolumeMeta { display_root: "C:".into(), ..Default::default() });
    v.by_recno = vec![u32::MAX; n + 16];
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let mut dirs: Vec<u32> = vec![0];
    for i in 0..n {
        let r = next();
        let parent = dirs[(r as usize) % dirs.len()];
        let w1 = words[(r >> 8) as usize % words.len()];
        let w2 = words[(r >> 16) as usize % words.len()];
        let is_dir = (r >> 24) % 10 == 0;
        let name = if is_dir { format!("{w1}_{w2}{}", (r >> 32) % 1000) } else { format!("{w1}-{w2}{}.{}", (r >> 32) % 100000, exts[(r >> 40) as usize % exts.len()]) };
        let attrs = if is_dir { 0x10 } else { 0x20 };
        let size = if is_dir { 0 } else { (r >> 20) % (10 << 20) };
        let id = v.push_raw(&name, parent, attrs, size, 133_000_000_000_000_000 + (r % 100_000_000_000) as i64, 0, (i + 16) as u64);
        if is_dir && dirs.len() < 1_500_000 {
            dirs.push(id);
        }
    }
    v.recompute_dir_sizes();
    println!("build {} entries: {:.2}s, heap {:.0} MB, arena {:.0} MB", n, t0.elapsed().as_secs_f64(), v.heap_bytes() as f64 / 1e6, v.arena.len() as f64 / 1e6);
    let dir = std::env::temp_dir().join("accio-bench");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("bench.accio");
    let t = Instant::now();
    accio_index::store::save(&v, &p).unwrap();
    println!("save: {:.2}s, file {:.0} MB", t.elapsed().as_secs_f64(), std::fs::metadata(&p).unwrap().len() as f64 / 1e6);
    let t = Instant::now();
    let v2 = accio_index::store::load(&p).unwrap();
    println!("load: {:.2}s ({} entries)", t.elapsed().as_secs_f64(), v2.len());
    drop(v2);
    let t = Instant::now();
    v.order_name = v.compute_name_order();
    v.order_covers = v.len();
    println!("name order: {:.2}s", t.elapsed().as_secs_f64());
    let idx = Index::new(vec![Arc::new(RwLock::new(v))]);
    for (q, sort) in [
        ("r", SortKey::Name), ("re", SortKey::Name), ("rep", SortKey::Name), ("repo", SortKey::Name), ("report", SortKey::Name),
        ("report photo", SortKey::Name), ("ext:png", SortKey::Name), ("size:>5mb", SortKey::Size), ("設計", SortKey::Name),
        ("path:backup", SortKey::Name), ("*.rs", SortKey::Name), (r"regex:^rep.*\d{4}", SortKey::Name), ("zzzz", SortKey::Name),
        ("folder: size:>1gb", SortKey::Size), ("dm:2024", SortKey::DateModified), ("report !photo ext:txt;md", SortKey::Path),
        // 件数上限なしソートの最悪ケース: 全件をサイズ/日時/パス順
        ("", SortKey::Size), ("", SortKey::DateModified), ("", SortKey::Path), ("r", SortKey::Path), ("", SortKey::Extension),
    ] {
        let req = Request { query: q.into(), sort: Sort { key: sort, ascending: false }, ..Default::default() };
        let mut best = f64::MAX;
        let mut total = 0;
        for _ in 0..3 {
            let r = search(&idx.snapshot(), &req, &|| false).unwrap();
            best = best.min(r.elapsed_ms);
            total = r.total;
        }
        println!("{q:28} {sort:?}: {total:>9} hits  {best:8.1} ms");
    }
}
