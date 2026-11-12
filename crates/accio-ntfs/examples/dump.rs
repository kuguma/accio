use accio_ntfs::{scan_mft, FileReader, ScanOptions};
use std::sync::{atomic::AtomicBool, Mutex};
fn main() {
    let p = std::env::args().nth(1).unwrap();
    let r = FileReader::open_path(std::path::Path::new(&p)).unwrap();
    let all = Mutex::new(Vec::new());
    let st = scan_mft(&r, &ScanOptions { chunk_bytes: 4096, threads: 2 }, &AtomicBool::new(false), &|_, _| {}, &|v| all.lock().unwrap().extend(v)).unwrap();
    let mut all = all.into_inner().unwrap();
    all.sort_by_key(|e| e.record);
    println!("{st:?} entries={}", all.len());
    for e in &all { println!("{} seq={} base={} dir={} size={:?} names={:?}", e.record, e.seq, e.base_record, e.is_dir, e.size, e.names.iter().map(|n| format!("{}@{}", n.name, n.parent)).collect::<Vec<_>>()); }
}
