//! Live updates (Spotlight's FSEvents). On Windows `notify` uses ReadDirectoryChangesW.
use crate::indexer::{self, Options};
use crate::store::Store;
use anyhow::Result;
use notify::{recommended_watcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::Duration;

/// Applies changes under `roots` to the index after a short quiet period.
/// Returns when `stop()` (polled every ~500 ms) says so, or when the watcher dies.
pub fn watch(
    store: &Store,
    roots: &[PathBuf],
    opts_for: &dyn Fn(&Path) -> Options,
    stop: &mut dyn FnMut() -> bool,
    log: &mut dyn FnMut(&str),
) -> Result<()> {
    let (tx, rx) = channel();
    let mut watcher = recommended_watcher(tx)?;
    for root in roots { watcher.watch(root, RecursiveMode::Recursive)?; }

    let mut pending: HashSet<PathBuf> = HashSet::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Ok(event)) => pending.extend(event.paths),
            Ok(Err(e)) => log(&format!("watch error: {e}")),
            Err(RecvTimeoutError::Timeout) => {
                for path in pending.drain() {
                    let opts = opts_for(&path);
                    if opts.is_excluded(&path) { continue; }
                    let result = if path.is_dir() {
                        indexer::crawl(store, &path, &opts, |_| {}).map(|s| format!("dir {} (+{} -{})", path.display(), s.indexed, s.removed))
                    } else if path.exists() {
                        indexer::index_file(store, &path, &opts).map(|changed| format!("{} {}", if changed { "indexed" } else { "unchanged" }, path.display()))
                    } else {
                        store.delete_path(&path.to_string_lossy()).map(|n| format!("removed {} ({} entries)", path.display(), n))
                    };
                    match result { Ok(m) => log(&m), Err(e) => log(&format!("{}: {e}", path.display())) }
                }
                if stop() { return Ok(()); }
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}
