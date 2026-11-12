use accio_index::{builder::VolumeBuilder, Index, VolumeMeta};
use accio_ntfs::{scan_mft, FileReader, ScanOptions};
use accio_query::{search, Request};
use std::sync::atomic::AtomicBool;
fn main() {
    let img = std::env::args().nth(1).unwrap();
    let r = FileReader::open_path(std::path::Path::new(&img)).unwrap();
    let b = VolumeBuilder::new(VolumeMeta { display_root: "T:".into(), ..Default::default() }, 1024);
    scan_mft(&r, &ScanOptions::default(), &AtomicBool::new(false), &|_, _| {}, &|v| b.push_batch(v)).unwrap();
    let idx = Index::new(vec![b.finish()]);
    let vols = idx.snapshot();
    for qs in std::env::args().skip(2) {
        let res = search(&vols, &Request { query: qs.clone(), ..Default::default() }, &|| false).unwrap();
        let g = vols[0].read();
        println!("{qs:?} -> {} hits {:?}", res.total, res.hits.iter().take(5).map(|h| format!("{} attrs={:x}", g.full_path(h.id), g.attrs[h.id as usize])).collect::<Vec<_>>());
        println!("  parsed: {:?}", accio_query::parser::parse(&qs).root);
    }
}
