//! Benchmark: fill an index with N synthetic files through the real store, then time queries.
use fsearch::query::Query;
use fsearch::store::{FileRecord, Store};
use std::time::Instant;

fn main() {
    let n: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(1_000_000);
    let store = Store::open(std::path::Path::new(&std::env::args().nth(2).unwrap())).unwrap();
    let words = ["report", "invoice", "budget", "design", "meeting", "photo", "archive", "project", "contract", "summary", "draft"];
    let exts = ["txt", "md", "pdf", "png", "docx", "rs", "json"];
    let t = Instant::now();
    let mut batch = Vec::with_capacity(5000);
    let mut x: u64 = 88172645463325252;
    let mut next = || { x ^= x << 13; x ^= x >> 7; x ^= x << 17; x };
    for i in 0..n {
        let ext = exts[(next() % 7) as usize];
        let name = format!("{}_{}_{}.{}", words[(next() % 11) as usize], words[(next() % 11) as usize], i, ext);
        batch.push((FileRecord { path: format!("C:\\data\\d{}\\s{}\\{}", i % 500, i % 13, name), name, ext: ext.into(), kind: "Document".into(),
            size: (next() % 50_000_000) as i64, mtime: 1_700_000_000_000 + (next() % 90_000_000_000) as i64, ctime: 0 }, None));
        if batch.len() == 5000 { store.upsert_batch(&batch).unwrap(); batch.clear(); }
    }
    store.upsert_batch(&batch).unwrap();
    println!("inserted {n} in {:.1}s ({} MB)", t.elapsed().as_secs_f64(), store.stats().unwrap().db_bytes / 1_000_000);
    for q in ["invoice_budget", "budget", "nvoice_bud", "ud", "ext:pdf budget", "kind:document size:>40mb", "name:report_ ext:docx modified:>2023-12-01"] {
        let query = Query::parse(q);
        let t = Instant::now();
        let hits = store.search(&query, 50).unwrap();
        let first = t.elapsed();
        let t = Instant::now();
        let total = store.count(&query).unwrap();
        println!("{q:45} first-50 {:>6.0} ms | count {:>6.0} ms | {total} matches ({} shown)", first.as_secs_f64() * 1e3, t.elapsed().as_secs_f64() * 1e3, hits.len());
    }
}
