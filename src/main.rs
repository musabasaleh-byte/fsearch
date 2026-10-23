use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use fsearch::indexer::{self, Options};
use fsearch::query::Query;
use fsearch::store::Store;
use fsearch::service::{self, ensure_start_menu_roots, hotkey_label, log_path, pid_file, running_pid, sync_store_apps, LogFile};
use fsearch::watcher;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "fsearch", about = "Spotlight-style fast file search")]
struct Cli {
    /// Index database (default: %LOCALAPPDATA%\fsearch\index.db or ~/.local/share/fsearch/index.db)
    #[arg(long, env = "FSEARCH_DB")]
    db: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Crawl folders and update the index (like mdimport)
    Index {
        paths: Vec<PathBuf>,
        /// Also index text inside supported documents
        #[arg(long)]
        content: bool,
    },
    /// Search (like mdfind). Example: fsearch find 'report ext:pdf size:>1mb'
    Find {
        query: Vec<String>,
        #[arg(short = 'n', long, default_value_t = 50)]
        limit: usize,
        /// Print only the number of matches
        #[arg(long)]
        count: bool,
    },
    /// Index folders in the foreground and keep them current until Ctrl+C
    Watch {
        paths: Vec<PathBuf>,
        #[arg(long)]
        content: bool,
    },
    /// Remember folders for the background service and index them now
    Add {
        paths: Vec<PathBuf>,
        #[arg(long)]
        content: bool,
    },
    /// Forget a folder and remove its files from the index
    Remove { path: PathBuf },
    /// List the folders the background service keeps indexed
    Roots,
    /// Run the background service in this process (what the scheduled task starts)
    Daemon,
    /// Windows: start the background service at every sign-in (no admin needed)
    Install,
    /// Windows: stop and remove the background service
    Uninstall {
        /// Also delete the programs and the index
        #[arg(long)]
        purge: bool,
    },
    /// Refresh the list of Microsoft Store apps (Calculator, Settings, ...) in the index
    Apps,
    /// Show or change the launcher hotkey, e.g. `fsearch hotkey ctrl+alt+f` (default ctrl+shift+space)
    Hotkey { combo: Option<String> },
    /// Index statistics (like mdutil -s)
    Status,
    /// Show the attributes the index would store for a file (like mdls)
    Info { path: PathBuf },
}

fn root_options(roots: &[(String, bool)], path: &Path) -> Options {
    let content = roots.iter().filter(|(r, _)| path.starts_with(r)).max_by_key(|(r, _)| r.len()).map(|(_, c)| *c).unwrap_or(false);
    Options { content, nice: true, ..Options::default() }
}

fn daemon(store: &Store, db: &Path) -> Result<()> {
    fsearch::win::low_priority();
    fsearch::win::detach_console();
    let log = std::cell::RefCell::new(LogFile::open(&log_path(db))?);
    let say = |m: &str| log.borrow_mut().line(m);
    // A service that is being replaced may need a moment to exit; wait for it instead of quitting.
    let mut owner = false;
    for _ in 0..80 {
        if fsearch::win::single_instance("Local\\fsearch-daemon") { owner = true; break; }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if !owner { return Ok(()); }
    let _ = std::fs::write(pid_file(db), std::process::id().to_string());
    ensure_start_menu_roots(store);
    say("daemon started");
    #[cfg(windows)]
    {
        let wanted = store.setting("hotkey")?.unwrap_or_else(|| fsearch::win::DEFAULT_HOTKEY.to_string());
        let (combo, (mods, vk)) = match fsearch::win::parse_hotkey(&wanted) {
            Some(parsed) => (wanted, parsed),
            None => { say(&format!("hotkey '{wanted}' is not valid; using {}", fsearch::win::DEFAULT_HOTKEY)); (fsearch::win::DEFAULT_HOTKEY.to_string(), fsearch::win::parse_hotkey(fsearch::win::DEFAULT_HOTKEY).unwrap()) }
        };
        service::spawn_hotkey_listener(db.to_path_buf(), combo, mods, vk);
        // Keep the search window ready in the background so the hotkey opens it instantly.
        let gui = service::gui_exe(db);
        if gui.exists() && !fsearch::win::launcher_running() {
            if let Err(e) = fsearch::win::spawn_detached(&gui, &[std::ffi::OsStr::new("--background")]) { say(&format!("could not start the search window: {e}")); }
        }
    }
    // Let the sign-in settle first: the hotkey and tray are already up, the heavy work waits.
    // At sign-in the heavy work waits a minute; right after a rebuild (empty index) it starts at once.
    if store.stats().map(|st| st.files > 0).unwrap_or(false) { std::thread::sleep(std::time::Duration::from_secs(60)); }
    if let Some(n) = service::sync_store_apps_if_stale(store) { say(&format!("store apps indexed: {n}")); }
    loop {
        let roots = store.roots()?;
        let mut live: Vec<PathBuf> = roots.iter().map(|(p, _)| PathBuf::from(p)).filter(|p| p.is_dir()).collect();
        // Start-menu folders first: they are small and make the apps searchable straight away.
        live.sort_by_key(|p| !p.to_string_lossy().to_lowercase().contains("start menu"));
        if live.is_empty() {
            std::thread::sleep(std::time::Duration::from_secs(15));
            continue;
        }
        for root in &live {
            match indexer::crawl(store, root, &root_options(&roots, root), |_| {}) {
                Ok(s) => say(&format!("crawled {}: scanned {}, indexed {}, removed {}", root.display(), s.scanned, s.indexed, s.removed)),
                Err(e) => say(&format!("crawl {} failed: {e}", root.display())),
            }
        }
        let mut last_check = std::time::Instant::now();
        let mut last_apps = std::time::Instant::now();
        let mut stop = || {
            if last_apps.elapsed() > std::time::Duration::from_secs(6 * 3600) {
                last_apps = std::time::Instant::now();
                sync_store_apps(store);
            }
            if last_check.elapsed() < std::time::Duration::from_secs(10) { return false; }
            last_check = std::time::Instant::now();
            store.roots().map(|now| now != roots).unwrap_or(false)
        };
        let result = watcher::watch(store, &live, &|p| root_options(&roots, p), &mut stop, &mut |m| say(m));
        if let Err(e) = result {
            say(&format!("watcher stopped: {e}; retrying in 30s"));
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
    }
}

fn fmt_size(n: i64) -> String {
    let n = n as f64;
    if n >= 1e9 { format!("{:.1} GB", n / 1e9) } else if n >= 1e6 { format!("{:.1} MB", n / 1e6) }
    else if n >= 1e3 { format!("{:.0} KB", n / 1e3) } else { format!("{n:.0} B") }
}

fn canonical(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    if paths.is_empty() { bail!("give at least one folder"); }
    paths.iter().map(|p| Ok(strip_unc(std::fs::canonicalize(p)?))).collect()
}

/// Windows canonicalize returns \\?\C:\...; keep plain paths so queries and results look normal.
fn strip_unc(p: PathBuf) -> PathBuf {
    match p.to_string_lossy().strip_prefix(r"\\?\") { Some(rest) => PathBuf::from(rest), None => p }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let db = cli.db.unwrap_or_else(fsearch::store::default_db_path);
    let store = Store::open(&db)?;

    match cli.cmd {
        Cmd::Index { paths, content } => {
            let opts = Options { content, ..Options::default() };
            for root in canonical(&paths)? {
                let started = std::time::Instant::now();
                let s = indexer::crawl(&store, &root, &opts, |s| eprint!("\r{} files scanned...", s.scanned))?;
                eprintln!("\r{}: scanned {}, indexed {}, unchanged {}, removed {} in {:.1}s",
                    root.display(), s.scanned, s.indexed, s.skipped, s.removed, started.elapsed().as_secs_f64());
            }
        }
        Cmd::Find { query, limit, count } => {
            let q = Query::parse(&query.join(" "));
            if count { println!("{}", store.count(&q)?); return Ok(()); }
            let hits = store.search(&q, limit)?;
            let shown = hits.len();
            for h in hits { println!("{}\t{}\t{}", fmt_size(h.size), h.kind, h.path); }
            if shown == limit {
                eprintln!("-- showing the {shown} newest of {} matches (use -n to see more)", store.count(&q)?);
            }
        }
        Cmd::Watch { paths, content } => {
            let opts = Options { content, ..Options::default() };
            let roots = canonical(&paths)?;
            for root in &roots { indexer::crawl(&store, root, &opts, |_| {})?; }
            eprintln!("watching {} folder(s); Ctrl+C to stop", roots.len());
            watcher::watch(&store, &roots, &|_| opts.clone(), &mut || false, &mut |m| eprintln!("{m}"))?;
        }
        Cmd::Add { paths, content } => {
            let opts = Options { content, ..Options::default() };
            for root in canonical(&paths)? {
                store.add_root(&root.to_string_lossy(), content)?;
                let s = indexer::crawl(&store, &root, &opts, |s| eprint!("\r{} files scanned...", s.scanned))?;
                eprintln!("\r{}: added; scanned {}, indexed {}", root.display(), s.scanned, s.indexed);
            }
            println!("the background service (fsearch install) picks up changes within ~10 seconds");
        }
        Cmd::Remove { path } => {
            let root = strip_unc(std::fs::canonicalize(&path).unwrap_or(path)).to_string_lossy().into_owned();
            let known = store.remove_root(&root)?;
            let n = store.delete_path(&root)?;
            println!("{}; {n} files removed from the index", if known { "folder forgotten" } else { "folder was not in the list" });
        }
        Cmd::Roots => {
            for (path, content) in store.roots()? { println!("{}{}", path, if content { "  (with content)" } else { "" }); }
        }
        Cmd::Daemon => daemon(&store, &db)?,
        Cmd::Apps => println!("{} Microsoft Store apps indexed", sync_store_apps(&store)),
        Cmd::Install => for line in service::install(&db, &store)? { println!("{line}"); },
        Cmd::Hotkey { combo: None } => println!("launcher hotkey: {}", hotkey_label(&store)),
        Cmd::Hotkey { combo: Some(combo) } => {
            if fsearch::win::parse_hotkey(&combo).is_none() {
                bail!("'{combo}' is not a valid hotkey. Use modifiers (ctrl, alt, shift, win) plus a key, e.g. ctrl+alt+f, ctrl+shift+space, win+f12");
            }
            store.set_setting("hotkey", &combo.to_ascii_lowercase())?;
            if running_pid(&db).is_some() { service::restart_service(&db)?; println!("saved; service restarted"); } else { println!("saved; it takes effect when the service starts (fsearch install)"); }
        }
        Cmd::Uninstall { purge } => println!("{}", if purge { service::purge(&db)? } else { service::uninstall(&db)? }),
        Cmd::Status => {
            let s = store.stats()?;
            println!("version:      {}", fsearch::build_label());
            println!("index:        {}", db.display());
            println!("files:        {}", s.files);
            println!("with content: {}", s.with_content);
            println!("size on disk: {}", fmt_size(s.db_bytes));
            println!("folders:      {}", store.roots()?.len());
            if cfg!(windows) {
                println!("service:      {}{}", if fsearch::win::autostart_exists() { "starts at sign-in" } else { "not installed" },
                    if running_pid(&db).is_some() { ", running" } else { ", not running" });
            }
        }
        Cmd::Info { path } => {
            let path = strip_unc(std::fs::canonicalize(Path::new(&path))?);
            let meta = std::fs::metadata(&path)?;
            let tmp = Store::open_in_memory()?;
            indexer::index_file(&tmp, &path, &Options { content: true, ..Options::default() })?;
            let hit = tmp.search(&Query::parse(&format!("name:\"{}\"", path.file_name().unwrap().to_string_lossy())), 1)?;
            println!("path:  {}\nsize:  {} bytes\nkind:  {}", path.display(), meta.len(), hit.first().map(|h| h.kind.as_str()).unwrap_or("?"));
        }
    }
    Ok(())
}
