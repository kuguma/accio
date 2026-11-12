//! 保存済み索引（%LOCALAPPDATA%\accio\*.accio）を読み込んで検索する。昇格不要。
//! `cargo run --release -p accio-query --example query_store -- <dir> <query> [max]`
use accio_index::Index;
use accio_query::{search, Request};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let dir = std::env::args().nth(1).expect("dir");
    let query = std::env::args().nth(2).unwrap_or_default();
    let max: usize = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(20);
    let t = Instant::now();
    let mut vols = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        if e.path().extension().map(|x| x == "accio").unwrap_or(false) {
            match accio_index::store::load(&e.path()) {
                Ok(v) => {
                    eprintln!(
                        "{}: {} entries ({:.0} MB heap: arena {:.0} orig {:.0} cols {:.0} cold {:.0} by_recno {:.0} order {:.0})",
                        v.meta.display_root,
                        v.live_count(),
                        v.heap_bytes() as f64 / 1e6,
                        v.arena.len() as f64 / 1e6,
                        v.orig_arena.len() as f64 / 1e6,
                        (v.name_off.len() * 4 + v.name_len.len() * 2 + v.parent.len() * 4 + v.flags.len() * 2 + v.attrs.len() * 4 + v.orig_off.len() * 4 + v.orig_len.len() * 2) as f64 / 1e6,
                        (v.size.len() * 8 + v.mtime_s.len() * 4 + v.ctime_s.len() * 4 + v.recno.len() * 4 + v.seq.len() * 2) as f64 / 1e6,
                        v.by_recno.len() as f64 * 4.0 / 1e6,
                        v.order_name.len() as f64 * 4.0 / 1e6
                    );
                    vols.push(Arc::new(RwLock::new(v)));
                }
                Err(err) => eprintln!("{}: {err}", e.path().display()),
            }
        }
    }
    eprintln!("load: {:.2}s", t.elapsed().as_secs_f64());
    let idx = Index::new(vols);
    let vols = idx.snapshot();
    let r = search(&vols, &Request { query, ..Default::default() }, &|| false).unwrap();
    println!("{} hits ({} files, {} folders) in {:.1} ms", r.total, r.files, r.folders, r.elapsed_ms);
    for h in r.hits.iter().take(max) {
        let g = vols[h.vol as usize].read();
        println!("{:>14}  {}", g.size[h.id as usize], g.full_path(h.id));
    }
}
