use crate::query::Query;
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct FileRecord {
    pub path: String,
    pub name: String,
    pub ext: String,
    pub kind: String,
    pub size: i64,
    pub mtime: i64,
    pub ctime: i64,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub size: i64,
    pub mtime: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct FileState {
    pub size: i64,
    pub mtime: i64,
    pub content_indexed: bool,
}

pub struct Stats {
    pub files: i64,
    pub with_content: i64,
    pub db_bytes: i64,
}

/// `%LOCALAPPDATA%\fsearch\index.db` on Windows, `~/.local/share/fsearch/index.db` elsewhere.
pub fn default_db_path() -> std::path::PathBuf {
    use std::path::PathBuf;
    if let Some(p) = std::env::var_os("FSEARCH_DB") { return PathBuf::from(p); }
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    base.unwrap_or_else(|| PathBuf::from(".")).join("fsearch").join("index.db")
}

pub struct Store {
    conn: Connection,
}

const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS files (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    ext TEXT NOT NULL,
    kind TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    ctime INTEGER NOT NULL,
    content_indexed INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS files_ext ON files(ext);
CREATE INDEX IF NOT EXISTS files_mtime ON files(mtime);
CREATE INDEX IF NOT EXISTS files_kind ON files(kind);
CREATE TABLE IF NOT EXISTS roots (
    path TEXT PRIMARY KEY,
    content INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS launches (
    path TEXT PRIMARY KEY,
    count INTEGER NOT NULL DEFAULT 0,
    last INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- Substring search over file names: any 3+ character fragment, case-insensitive, no table scan.
CREATE VIRTUAL TABLE IF NOT EXISTS name_tri USING fts5(name, tokenize = 'trigram');
CREATE VIRTUAL TABLE IF NOT EXISTS content_fts USING fts5(content, tokenize = 'unicode61 remove_diacritics 2');
";

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    #[doc(hidden)]
    pub fn conn_for_tests(&self) -> &Connection { &self.conn }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.pragma_update(None, "cache_size", -65536)?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < SCHEMA_VERSION {
            // v1 kept names and content in one FTS table and scanned names with LIKE.
            conn.execute_batch("DROP TABLE IF EXISTS search_fts; DROP INDEX IF EXISTS files_name;")?;
        }
        conn.execute_batch(SCHEMA)?;
        if version < SCHEMA_VERSION {
            conn.execute_batch(
                "DELETE FROM name_tri;
                 INSERT INTO name_tri(rowid, name) SELECT id, name FROM files;
                 UPDATE files SET content_indexed = 0;",
            )?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self { conn })
    }

    pub fn state(&self, path: &str) -> Result<Option<FileState>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT size, mtime, content_indexed FROM files WHERE path = ?1")?;
        let mut rows = stmt.query(params![path])?;
        Ok(match rows.next()? {
            Some(r) => Some(FileState { size: r.get(0)?, mtime: r.get(1)?, content_indexed: r.get::<_, i64>(2)? != 0 }),
            None => None,
        })
    }

    /// Insert or update many files atomically. `content` is `Some` when an importer was attempted.
    pub fn upsert_batch(&self, items: &[(FileRecord, Option<String>)]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut find = tx.prepare_cached("SELECT id FROM files WHERE path = ?1")?;
            let mut insert = tx.prepare_cached(
                "INSERT INTO files(path,name,ext,kind,size,mtime,ctime,content_indexed) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)")?;
            let mut update = tx.prepare_cached(
                "UPDATE files SET name=?2, ext=?3, kind=?4, size=?5, mtime=?6, ctime=?7, content_indexed=?8 WHERE id=?1")?;
            let mut del_name = tx.prepare_cached("DELETE FROM name_tri WHERE rowid = ?1")?;
            let mut del_content = tx.prepare_cached("DELETE FROM content_fts WHERE rowid = ?1")?;
            let mut add_name = tx.prepare_cached("INSERT INTO name_tri(rowid, name) VALUES (?1, ?2)")?;
            let mut add_content = tx.prepare_cached("INSERT INTO content_fts(rowid, content) VALUES (?1, ?2)")?;

            for (rec, content) in items {
                let attempted = content.is_some() as i64;
                let id = match find.query_row(params![rec.path], |r| r.get::<_, i64>(0)).optional()? {
                    Some(id) => {
                        update.execute(params![id, rec.name, rec.ext, rec.kind, rec.size, rec.mtime, rec.ctime, attempted])?;
                        del_name.execute(params![id])?;
                        del_content.execute(params![id])?;
                        id
                    }
                    None => {
                        insert.execute(params![rec.path, rec.name, rec.ext, rec.kind, rec.size, rec.mtime, rec.ctime, attempted])?;
                        tx.last_insert_rowid()
                    }
                };
                add_name.execute(params![id, rec.name])?;
                if let Some(text) = content.as_deref().filter(|t| !t.is_empty()) {
                    add_content.execute(params![id, text])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Removes one path and, if it was a directory, everything under it.
    pub fn delete_path(&self, path: &str) -> Result<usize> {
        let prefix = format!("{}{}%", escape_like(path.trim_end_matches(std::path::MAIN_SEPARATOR)), escape_like(&std::path::MAIN_SEPARATOR.to_string()));
        let ids: Vec<i64> = {
            let mut stmt = self.conn.prepare("SELECT id FROM files WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\'")?;
            let rows = stmt.query_map(params![path, prefix], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        self.delete_ids(&ids)
    }

    pub fn paths_under(&self, root: &str) -> Result<Vec<(i64, String)>> {
        let prefix = format!("{}{}%", escape_like(root.trim_end_matches(std::path::MAIN_SEPARATOR)), escape_like(&std::path::MAIN_SEPARATOR.to_string()));
        let mut stmt = self.conn.prepare("SELECT id, path FROM files WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\'")?;
        let rows = stmt.query_map(params![root, prefix], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn delete_ids(&self, ids: &[i64]) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        for id in ids {
            tx.execute("DELETE FROM name_tri WHERE rowid = ?1", params![id])?;
            tx.execute("DELETE FROM content_fts WHERE rowid = ?1", params![id])?;
            tx.execute("DELETE FROM files WHERE id = ?1", params![id])?;
        }
        tx.commit()?;
        Ok(ids.len())
    }

    pub fn search(&self, query: &Query, limit: usize) -> Result<Vec<Hit>> {
        let (sql, args) = query.to_sql(limit);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), |r| {
            Ok(Hit { path: r.get(0)?, name: r.get(1)?, kind: r.get(2)?, size: r.get(3)?, mtime: r.get(4)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Number of files matching `query` (ignores any display limit).
    pub fn count(&self, query: &Query) -> Result<usize> {
        let (sql, args) = query.to_count_sql();
        let n: i64 = self.conn.query_row(&sql, rusqlite::params_from_iter(args.iter()), |r| r.get(0))?;
        Ok(n as usize)
    }

    /// Remember that the user opened `path` from the launcher (feeds ranking).
    pub fn record_open(&self, path: &str, now_ms: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO launches(path, count, last) VALUES (?1, 1, ?2) ON CONFLICT(path) DO UPDATE SET count = count + 1, last = ?2",
            params![path, now_ms],
        )?;
        Ok(())
    }

    /// path -> (times opened, last opened ms) for the given paths.
    pub fn launches(&self, paths: &[String]) -> Result<std::collections::HashMap<String, (i64, i64)>> {
        let mut out = std::collections::HashMap::new();
        for chunk in paths.chunks(400) {
            let marks = vec!["?"; chunk.len()].join(",");
            let mut stmt = self.conn.prepare(&format!("SELECT path, count, last FROM launches WHERE path IN ({marks})"))?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))))?;
            for row in rows { let (p, v) = row?; out.insert(p, v); }
        }
        Ok(out)
    }

    /// Replaces the Microsoft Store app entries (path "shell:AppsFolder\\<AppID>") with the given (AppID, name) list.
    pub fn replace_store_apps(&self, apps: &[(String, String)], now_ms: i64) -> Result<usize> {
        let old: Vec<i64> = {
            let mut stmt = self.conn.prepare("SELECT id FROM files WHERE ext = 'uwp'")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        self.delete_ids(&old)?;
        let items: Vec<(FileRecord, Option<String>)> = apps.iter().map(|(id, name)| (FileRecord {
            path: format!("shell:AppsFolder\\{id}"), name: name.clone(), ext: "uwp".into(), kind: "Application".into(),
            size: 0, mtime: now_ms, ctime: now_ms }, None)).collect();
        self.upsert_batch(&items)?;
        Ok(items.len())
    }

    /// The name index (trigram) must have exactly one entry per file. If a file is missing from it, searching by name
    /// cannot find it. Adds the missing entries and removes leftovers; returns (added, removed).
    pub fn repair_name_index(&self) -> Result<(usize, usize)> {
        let added = self.conn.execute(
            "INSERT INTO name_tri(rowid, name) SELECT id, name FROM files WHERE id NOT IN (SELECT rowid FROM name_tri)", [])?;
        let removed = self.conn.execute("DELETE FROM name_tri WHERE rowid NOT IN (SELECT id FROM files)", [])?;
        Ok((added, removed))
    }

    /// Forget every indexed file (folders, settings and launch history stay). Used by "Rebuild index".
    pub fn clear_files(&self) -> Result<()> {
        self.conn.execute_batch("DELETE FROM name_tri; DELETE FROM content_fts; DELETE FROM files;")?;
        Ok(())
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute("INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2", params![key, value])?;
        Ok(())
    }

    pub fn add_root(&self, path: &str, content: bool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO roots(path, content) VALUES (?1, ?2) ON CONFLICT(path) DO UPDATE SET content = ?2",
            params![path, content as i64],
        )?;
        Ok(())
    }

    pub fn remove_root(&self, path: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM roots WHERE path = ?1", params![path])? > 0)
    }

    pub fn roots(&self) -> Result<Vec<(String, bool)>> {
        let mut stmt = self.conn.prepare("SELECT path, content FROM roots ORDER BY path")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn stats(&self) -> Result<Stats> {
        let files = self.conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
        let with_content = self.conn.query_row("SELECT COUNT(*) FROM files WHERE content_indexed = 1", [], |r| r.get(0))?;
        let pages: i64 = self.conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let page_size: i64 = self.conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        Ok(Stats { files, with_content, db_bytes: pages * page_size })
    }
}
