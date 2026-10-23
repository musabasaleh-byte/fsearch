//! Initial crawl and single-file indexing (Spotlight's mds + mdworker).
use crate::importer;
use crate::query::kind_for;
use crate::store::{FileRecord, Store};
use anyhow::Result;
use rayon::prelude::*;
use std::collections::HashSet;
use std::fs::Metadata;
use std::path::Path;
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

#[derive(Clone)]
pub struct Options {
    pub content: bool,
    pub max_content_bytes: u64,
    pub excludes: Vec<String>,
    /// Background service: pause briefly while crawling so the disk and antivirus are not hammered.
    pub nice: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            content: false,
            max_content_bytes: 2 * 1024 * 1024,
            excludes: [".git", "node_modules", "$RECYCLE.BIN", "System Volume Information", "__pycache__", ".Trash",
                "AppData", "Windows.old", "WinSxS", ".cache", ".gradle", ".npm", ".cargo", ".rustup", "site-packages"]
                .iter().map(|s| s.to_string()).collect(),
            nice: false,
        }
    }
}

impl Options {
    pub fn is_excluded(&self, path: &Path) -> bool {
        // The Start-menu folders live inside AppData and are what makes apps searchable.
        let start_menu = path.components().any(|c| c.as_os_str().eq_ignore_ascii_case("Start Menu"));
        path.components().any(|c| {
            let c = c.as_os_str().to_string_lossy();
            self.excludes.iter().any(|e| e.eq_ignore_ascii_case(&c) && !(start_menu && e == "AppData"))
        })
    }
}

#[derive(Default, Debug)]
pub struct CrawlStats {
    pub scanned: usize,
    pub indexed: usize,
    pub skipped: usize,
    pub removed: usize,
}

/// Milliseconds, so an edit within the same second that keeps the file size is still noticed.
fn millis(t: std::io::Result<std::time::SystemTime>) -> i64 {
    t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn record(path: &Path, meta: &Metadata) -> FileRecord {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    FileRecord {
        path: path.to_string_lossy().into_owned(),
        name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        kind: kind_for(&ext).to_string(),
        ext,
        size: meta.len() as i64,
        mtime: millis(meta.modified()),
        ctime: millis(meta.created()),
    }
}

struct Planned {
    rec: FileRecord,
    extract: bool,
}

/// OneDrive "files on demand" placeholders: reading one downloads it, so they are indexed by name only.
fn online_only(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        return meta.file_attributes() & (0x0040_0000 | 0x0004_0000 | 0x0000_1000) != 0;   // RECALL_ON_DATA_ACCESS | RECALL_ON_OPEN | OFFLINE
    }
    #[cfg(not(windows))]
    { let _ = meta; false }
}

/// Decides whether a file needs (re)indexing. Cheap: one indexed lookup, no file reads.
fn plan(store: &Store, path: &Path, meta: &Metadata, opts: &Options) -> Result<Option<Planned>> {
    let rec = record(path, meta);
    let extract = opts.content && !online_only(meta) && meta.len() <= opts.max_content_bytes.saturating_mul(8) && importer::find(&rec.ext).is_some();
    if let Some(state) = store.state(&rec.path)? {
        if state.size == rec.size && state.mtime == rec.mtime && (!extract || state.content_indexed) {
            return Ok(None);
        }
    }
    Ok(Some(Planned { rec, extract }))
}

/// Reads and extracts the file's text. `Some("")` means "attempted, nothing usable".
fn extract(rec: &FileRecord, opts: &Options) -> Option<String> {
    let importer = importer::find(&rec.ext)?;
    Some(importer.extract(Path::new(&rec.path), opts.max_content_bytes).unwrap_or_default())
}

pub fn index_file(store: &Store, path: &Path, opts: &Options) -> Result<bool> {
    if opts.is_excluded(path) { return Ok(false); }
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() { return Ok(false); }
    match plan(store, path, &meta, opts)? {
        Some(p) => {
            let content = if p.extract { extract(&p.rec, opts) } else { None };
            store.upsert_batch(&[(p.rec, content)])?;
            Ok(true)
        }
        None => Ok(false),
    }
}

const CHUNK: usize = 2000;

/// Extracts text for a chunk of files on all cores, then writes it in one transaction.
fn flush(store: &Store, pending: &mut Vec<Planned>, opts: &Options) -> Result<()> {
    if pending.is_empty() { return Ok(()); }
    let items: Vec<(FileRecord, Option<String>)> = std::mem::take(pending)
        .into_par_iter()
        .map(|p| { let content = if p.extract { extract(&p.rec, opts) } else { None }; (p.rec, content) })
        .collect();
    store.upsert_batch(&items)
}

pub fn crawl(store: &Store, root: &Path, opts: &Options, mut progress: impl FnMut(&CrawlStats)) -> Result<CrawlStats> {
    let mut stats = CrawlStats::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut pending: Vec<Planned> = Vec::with_capacity(CHUNK);

    let walker = WalkDir::new(root).follow_links(false).into_iter()
        .filter_entry(|e| !opts.is_excluded(e.path()) || e.path() == root);
    for entry in walker.filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() { continue; }
        let Ok(meta) = entry.metadata() else { continue };
        stats.scanned += 1;
        seen.insert(entry.path().to_string_lossy().into_owned());
        match plan(store, entry.path(), &meta, opts)? {
            Some(p) => { pending.push(p); stats.indexed += 1; }
            None => stats.skipped += 1,
        }
        if pending.len() >= CHUNK { flush(store, &mut pending, opts)?; }
        if opts.nice && stats.scanned % 400 == 0 { std::thread::sleep(std::time::Duration::from_millis(15)); }
        if stats.scanned % 1000 == 0 { progress(&stats); }
    }
    flush(store, &mut pending, opts)?;

    let stale: Vec<i64> = store.paths_under(&root.to_string_lossy())?.into_iter()
        .filter(|(_, p)| !seen.contains(p)).map(|(id, _)| id).collect();
    stats.removed = store.delete_ids(&stale)?;
    progress(&stats);
    Ok(stats)
}
