//! Background-service management shared by the command line and the settings window:
//! start/stop, start-at-sign-in, the launcher hotkey, Start-menu shortcuts.
use crate::store::Store;
use crate::win;
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

pub fn data_dir(db: &Path) -> PathBuf {
    db.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
}
pub fn pid_file(db: &Path) -> PathBuf { data_dir(db).join("daemon.pid") }
pub fn log_path(db: &Path) -> PathBuf { data_dir(db).join("daemon.log") }
pub fn hotkey_status_path(db: &Path) -> PathBuf { data_dir(db).join("hotkey.status") }
pub fn daemon_exe(db: &Path) -> PathBuf { data_dir(db).join("fsearch.exe") }
pub fn gui_exe(db: &Path) -> PathBuf { data_dir(db).join("fsearch-gui.exe") }

pub struct LogFile(std::fs::File);
impl LogFile {
    pub fn open(path: &Path) -> Result<Self> {
        if std::fs::metadata(path).map(|m| m.len() > 1_000_000).unwrap_or(false) { let _ = std::fs::remove_file(path); }
        Ok(Self(std::fs::OpenOptions::new().create(true).append(true).open(path)?))
    }
    pub fn line(&mut self, msg: &str) {
        use std::io::Write;
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(self.0, "[{secs}] {msg}");
    }
}

const MUTEX: &str = "Local\\fsearch-daemon";

/// Whether the service is running right now. Reads a Windows named mutex: no process is started, so it is instant.
pub fn is_running(db: &Path) -> bool {
    if cfg!(windows) { return win::mutex_exists(MUTEX); }
    std::fs::read_to_string(pid_file(db)).ok().and_then(|t| t.trim().parse().ok()).map(win::process_alive).unwrap_or(false)
}

/// The service's process id, if it is running.
pub fn running_pid(db: &Path) -> Option<u32> {
    if !is_running(db) { return None; }
    std::fs::read_to_string(pid_file(db)).ok()?.trim().parse().ok()
}

fn wait_until(running: bool, db: &Path, seconds: u64) -> bool {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    while std::time::Instant::now() < end {
        if is_running(db) == running { return true; }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    is_running(db) == running
}

/// Stops the service and waits until it has really exited (it may be mid-write to the index).
pub fn stop_service(db: &Path) {
    if let Some(pid) = running_pid(db) { win::kill_process(pid); }
    wait_until(false, db, 8);
}

pub fn start_service(db: &Path) -> std::io::Result<()> {
    let exe = daemon_exe(db);
    if !exe.exists() { return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "service program not installed (run: fsearch install)")); }
    if is_running(db) && !wait_until(false, db, 1) { return Ok(()); }   // already running
    win::spawn_detached(&exe, &["--db".as_ref(), db.as_os_str(), "daemon".as_ref()])
}

pub fn restart_service(db: &Path) -> std::io::Result<()> {
    stop_service(db);
    start_service(db)
}

pub fn set_autostart(db: &Path, enabled: bool) -> bool {
    if enabled {
        let command = format!("\"{}\" --db \"{}\" daemon", daemon_exe(db).display(), db.display());
        win::autostart_set(&command)
    } else {
        win::autostart_remove()
    }
}

/// Written by the service once it knows whether the hotkey could be registered: "ok <combo>" or "taken <combo>".
pub fn hotkey_status(db: &Path) -> Option<String> {
    std::fs::read_to_string(hotkey_status_path(db)).ok()
}

/// Adds both Start-menu folders once (so apps like Word are searchable). Removing them later sticks.
pub fn ensure_start_menu_roots(store: &Store) {
    if !cfg!(windows) || store.setting("start_menu_roots").ok().flatten().is_some() { return; }
    for var in ["ProgramData", "APPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            let dir = PathBuf::from(base).join(r"Microsoft\Windows\Start Menu\Programs");
            if dir.is_dir() { let _ = store.add_root(&dir.to_string_lossy(), false); }
        }
    }
    let _ = store.set_setting("start_menu_roots", "1");
}

pub fn sync_store_apps(store: &Store) -> usize {
    let apps = win::store_apps();
    if apps.is_empty() { return 0; }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    store.replace_store_apps(&apps, now).unwrap_or(0)
}

/// Store apps change rarely; asking PowerShell at every sign-in is slow and noisy for antivirus.
pub fn sync_store_apps_if_stale(store: &Store) -> Option<usize> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let last: i64 = store.setting("apps_synced_at").ok().flatten().and_then(|v| v.parse().ok()).unwrap_or(0);
    if now - last < 6 * 3600 { return None; }
    let n = sync_store_apps(store);
    if n > 0 { let _ = store.set_setting("apps_synced_at", &now.to_string()); }
    Some(n)
}

pub fn hotkey_label(store: &Store) -> String {
    store.setting("hotkey").ok().flatten().unwrap_or_else(|| win::DEFAULT_HOTKEY.to_string())
}

/// Start-menu entry so the program shows up in Windows search too. Best effort.
pub fn start_menu_shortcut(name: &str, target: &Path, args: &str, icon: &Path) {
    let Some(appdata) = std::env::var_os("APPDATA") else { return };
    let lnk = PathBuf::from(appdata).join(format!(r"Microsoft\Windows\Start Menu\Programs\{name}.lnk"));
    let script = "$s=(New-Object -ComObject WScript.Shell).CreateShortcut($env:FS_LNK);$s.TargetPath=$env:FS_TARGET;$s.Arguments=$env:FS_ARGS;$s.IconLocation=$env:FS_ICON;$s.Save()";
    let _ = win::quiet_command("powershell").args(["-NoProfile", "-Command", script])
        .env("FS_LNK", lnk).env("FS_TARGET", target).env("FS_ARGS", args).env("FS_ICON", icon)
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
}

/// Opens Explorer on the Start-menu "fsearch" shortcut (creating it if missing) with the file selected, so it can be
/// pinned to the taskbar (Windows does not let programs pin themselves).
pub fn reveal_start_menu_shortcut(db: &Path) -> bool {
    let Some(appdata) = std::env::var_os("APPDATA") else { return false };
    let lnk = PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\fsearch.lnk");
    if !lnk.exists() {
        let gui = gui_exe(db);
        if !gui.exists() { return false; }
        let icon = ico_path(db);
        let _ = std::fs::write(&icon, ICO);
        start_menu_shortcut("fsearch", &gui, "", &icon);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        return std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{}\"", lnk.display())).spawn().is_ok();
    }
    #[cfg(not(windows))]
    false
}

/// Puts a shortcut on the user's Desktop (wherever it really is, e.g. redirected to OneDrive) and remembers its path.
pub fn desktop_shortcut(db: &Path, store: &Store) -> bool {
    if !cfg!(windows) { return false; }
    let (gui, icon) = (gui_exe(db), ico_path(db));
    let script = "$p=Join-Path ([Environment]::GetFolderPath('Desktop')) 'fsearch.lnk';$s=(New-Object -ComObject WScript.Shell).CreateShortcut($p);$s.TargetPath=$env:FS_TARGET;$s.IconLocation=$env:FS_ICON;$s.Save();[Console]::OutputEncoding=[Text.Encoding]::UTF8;$p";
    let Ok(out) = win::quiet_command("powershell").args(["-NoProfile", "-Command", script]).env("FS_TARGET", gui).env("FS_ICON", icon).output() else { return false };
    let lnk = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if lnk.is_empty() || !Path::new(&lnk).exists() { return false; }
    let _ = store.set_setting("desktop_lnk", &lnk);
    true
}

fn remove_desktop_shortcut(db: &Path) {
    if let Ok(store) = Store::open(db) {
        if let Some(lnk) = store.setting("desktop_lnk").ok().flatten() { let _ = std::fs::remove_file(lnk); }
    }
}

pub fn remove_start_menu_shortcut(name: &str) {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let _ = std::fs::remove_file(PathBuf::from(appdata).join(format!(r"Microsoft\Windows\Start Menu\Programs\{name}.lnk")));
    }
}

/// Copies the programs to the data folder, registers start-at-sign-in and starts the service. Returns lines to show.
pub fn install(db: &Path, store: &Store) -> Result<Vec<String>> {
    if !cfg!(windows) { bail!("install is only available on Windows"); }
    let dir = data_dir(db);
    std::fs::create_dir_all(&dir)?;
    let target = daemon_exe(db);
    ensure_start_menu_roots(store);
    sync_store_apps(store);
    stop_service(db);
    let me = std::env::current_exe()?;
    if me != target { std::fs::copy(&me, &target)?; }
    // The launcher window travels with the service so the hotkey can start it.
    let gui_src = me.parent().map(|d| d.join("fsearch-gui.exe")).filter(|g| g.exists());
    let gui_target = gui_exe(db);
    if let Some(src) = &gui_src { if *src != gui_target { std::fs::copy(src, &gui_target)?; } }

    finish_install(db, store)
}

/// Removes the "downloaded from the internet" mark (an NTFS stream). Copying or overwriting a downloaded file keeps it,
/// and Windows then asks "The publisher could not be verified" every time the service starts at sign-in.
pub fn unblock(path: &Path) {
    #[cfg(windows)]
    { let _ = std::fs::remove_file(format!("{}:Zone.Identifier", path.display())); }
    #[cfg(not(windows))]
    { let _ = path; }
}

/// Everything after the programs are in place: start-at-sign-in, service, Start-menu entries, Apps & features entry.
pub fn finish_install(db: &Path, store: &Store) -> Result<Vec<String>> {
    let target = daemon_exe(db);
    let gui_target = gui_exe(db);
    unblock(&target);
    unblock(&gui_target);
    if !set_autostart(db, true) { bail!("could not write the start-up entry in the registry"); }
    if let Err(e) = start_service(db) { bail!("start-up entry saved, but starting the service now failed: {e}"); }
    register_uninstall(db);
    let mut lines = vec![
        "installed: starts at every sign-in; running now".to_string(),
        format!("program: {}", target.display()),
        format!("log:     {}", log_path(db).display()),
    ];
    if gui_target.exists() {
        let icon = ico_path(db);
        let _ = std::fs::write(&icon, ICO);
        start_menu_shortcut("fsearch", &gui_target, "", &icon);
        start_menu_shortcut("fsearch Settings", &gui_target, "--settings", &icon);
        lines.push(format!("launcher: press {} (or open \"fsearch\" from the Start menu). All options: open \"fsearch Settings\"", hotkey_label(store)));
    } else {
        lines.push("launcher: fsearch-gui.exe was not next to this program, so there is no hotkey window (put it beside fsearch.exe and run install again)".to_string());
    }
    Ok(lines)
}

const UNINSTALL_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\fsearch";

/// Lists fsearch under Settings > Apps so it can be removed like any program.
fn register_uninstall(db: &Path) {
    if !cfg!(windows) { return; }
    let exe = daemon_exe(db);
    let entries = [
        ("DisplayName", "fsearch".to_string()),
        ("DisplayVersion", env!("CARGO_PKG_VERSION").to_string()),
        ("Publisher", "fsearch".to_string()),
        ("DisplayIcon", ico_path(db).to_string_lossy().into_owned()),
        ("InstallLocation", data_dir(db).to_string_lossy().into_owned()),
        ("UninstallString", format!("\"{}\" uninstall --purge", exe.display())),
    ];
    for (name, value) in entries {
        let _ = win::quiet_command("reg").args(["add", UNINSTALL_KEY, "/v", name, "/d", &value, "/f"]).output();
    }
}

/// Removes everything fsearch put on the PC: service, start-up entry, shortcuts, Apps entry, then the folder with the
/// programs and the index (deleted a moment later, once this process has exited).
pub fn purge(db: &Path) -> Result<String> {
    if !cfg!(windows) { bail!("uninstall is only available on Windows"); }
    let summary = uninstall(db)?;
    let _ = win::quiet_command("reg").args(["delete", UNINSTALL_KEY, "/f"]).output();
    let dir = data_dir(db);
    let script = format!("ping -n 4 127.0.0.1 >nul & rmdir /s /q \"{}\"", dir.display());
    let _ = win::quiet_command("cmd").args(["/c", &script]).spawn();
    Ok(format!("{summary}. Removing {} …", dir.display()))
}

pub fn uninstall(db: &Path) -> Result<String> {
    if !cfg!(windows) { bail!("uninstall is only available on Windows"); }
    stop_service(db);
    let removed = win::autostart_remove();
    remove_start_menu_shortcut("fsearch");
    remove_start_menu_shortcut("fsearch Settings");
    remove_desktop_shortcut(db);
    let _ = std::fs::remove_file(pid_file(db));
    Ok(format!("{} (the index and the copied programs stay in {})",
        if removed { "service stopped and start-up entry removed" } else { "service was not installed; stopped if it was running" }, data_dir(db).display()))
}

/// Tray-icon image (PNG) and the multi-size .ico used for shortcuts and the programs' own icon.
#[cfg_attr(not(windows), allow(dead_code))]
const TRAY_PNG: &[u8] = include_bytes!("../assets/tray-32.png");
pub const ICO: &[u8] = include_bytes!("../assets/fsearch.ico");

pub fn ico_path(db: &Path) -> PathBuf { data_dir(db).join("fsearch.ico") }

#[cfg_attr(not(windows), allow(dead_code))]
fn pretty(combo: &str) -> String {
    combo.split('+').map(|p| match p.trim() {
        "ctrl" => "Ctrl".to_string(), "alt" => "Alt".to_string(), "shift" => "Shift".to_string(), "win" => "Win".to_string(),
        "space" => "Space".to_string(), o => o.to_uppercase(),
    }).collect::<Vec<_>>().join("+")
}

/// The service's windowing thread: owns the hotkey and the notification-area icon (click = open, right-click = menu).
pub fn spawn_hotkey_listener(db: PathBuf, combo: String, mods: u32, vk: u32) {
    #[cfg(windows)]
    shell::spawn(db, combo, mods, vk);
    #[cfg(not(windows))]
    let _ = (db, combo, mods, vk);
}

#[cfg(windows)]
mod shell {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, MOD_NOREPEAT};
    use windows_sys::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    const WM_TRAY: u32 = WM_APP + 1;
    const ID_OPEN: usize = 1;
    const ID_SETTINGS: usize = 2;
    const ID_QUIT: usize = 3;

    struct Shared { db: PathBuf, combo: String }
    static SHARED: OnceLock<Shared> = OnceLock::new();
    static ICON: AtomicUsize = AtomicUsize::new(0);
    static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
    static SHOW_TRAY: AtomicBool = AtomicBool::new(true);

    fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() }
    fn shared() -> &'static Shared { SHARED.get().expect("shell state") }
    fn note(msg: &str) { if let Ok(mut l) = LogFile::open(&log_path(&shared().db)) { l.line(msg); } }

    fn launch(args: &[&str]) {
        // The launcher normally stays running, hidden; opening it is then just a signal.
        if args.is_empty() && win::signal_launcher() { return; }
        let gui = gui_exe(&shared().db);
        if !gui.exists() { note("fsearch-gui.exe not found next to the service"); return; }
        if let Err(e) = std::process::Command::new(gui).args(args).spawn() { note(&format!("could not start the window: {e}")); }
    }

    unsafe fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = ICON.load(Ordering::Relaxed) as _;
        for (i, c) in format!("fsearch ({})", pretty(&shared().combo)).encode_utf16().take(127).enumerate() { nid.szTip[i] = c; }
        nid
    }

    unsafe fn add_icon(hwnd: HWND) {
        let nid = icon_data(hwnd);
        if Shell_NotifyIconW(NIM_ADD, &nid) == 0 { note("could not add the notification-area icon"); }
    }

    unsafe fn show_menu(hwnd: HWND) {
        let menu = CreatePopupMenu();
        if menu.is_null() { return; }
        AppendMenuW(menu, MF_STRING, ID_OPEN, wide(&format!("Open fsearch\t{}", pretty(&shared().combo))).as_ptr());
        AppendMenuW(menu, MF_STRING, ID_SETTINGS, wide("Settings…").as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, ID_QUIT, wide("Quit fsearch").as_ptr());
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(hwnd);                       // without this the menu would not close when you click elsewhere
        TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, std::ptr::null());
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_HOTKEY => { launch(&[]); 0 }
            WM_TRAY => {
                match (lparam as u32) & 0xFFFF {
                    WM_LBUTTONUP => launch(&[]),
                    WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                    _ => {}
                }
                0
            }
            WM_COMMAND => {
                match wparam & 0xFFFF {
                    ID_OPEN => launch(&[]),
                    ID_SETTINGS => launch(&["--settings"]),
                    ID_QUIT => {
                        let nid = icon_data(hwnd);
                        Shell_NotifyIconW(NIM_DELETE, &nid);
                        note("quit from the notification-area menu");
                        std::process::exit(0);
                    }
                    _ => {}
                }
                0
            }
            m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => { if SHOW_TRAY.load(Ordering::Relaxed) { add_icon(hwnd); } 0 }   // Explorer restarted
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    pub fn spawn(db: PathBuf, combo: String, mods: u32, vk: u32) {
        let _ = SHARED.set(Shared { db: db.clone(), combo: combo.clone() });
        std::thread::spawn(move || unsafe {
            let hinst = GetModuleHandleW(std::ptr::null());
            let class = wide("fsearchShellWindow");
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(wndproc);
            wc.hInstance = hinst;
            wc.lpszClassName = class.as_ptr();
            RegisterClassW(&wc);
            // A hidden top-level window (not message-only) so it also receives the "TaskbarCreated" broadcast.
            let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW, class.as_ptr(), wide("fsearch").as_ptr(), WS_POPUP, 0, 0, 0, 0,
                std::ptr::null_mut(), std::ptr::null_mut(), hinst, std::ptr::null());
            if hwnd.is_null() { note("could not create the service window; no hotkey and no tray icon"); return; }
            TASKBAR_CREATED.store(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()), Ordering::Relaxed);

            if RegisterHotKey(hwnd, 1, mods | MOD_NOREPEAT, vk) == 0 {
                let _ = std::fs::write(hotkey_status_path(&db), format!("taken {combo}"));
                note(&format!("hotkey {combo} is already used by another program; the shortcut is disabled (choose another in fsearch Settings)"));
            } else {
                let _ = std::fs::write(hotkey_status_path(&db), format!("ok {combo}"));
                note(&format!("hotkey {combo} registered"));
            }

            let icon = CreateIconFromResourceEx(TRAY_PNG.as_ptr(), TRAY_PNG.len() as u32, 1, 0x0003_0000, 32, 32, LR_DEFAULTCOLOR);
            let icon = if icon.is_null() { note("tray icon image rejected; using the default icon"); LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) } else { icon };
            ICON.store(icon as usize, Ordering::Relaxed);
            let show_tray = crate::store::Store::open(&db).ok().and_then(|st| st.setting("tray_icon").ok().flatten()).map(|v| v != "0").unwrap_or(true);
            SHOW_TRAY.store(show_tray, Ordering::Relaxed);
            if show_tray { add_icon(hwnd); }
            // Windows 11 puts new tray icons in the hidden overflow; ask for ours to be shown (the entry appears after a few seconds).
            let promoted = crate::store::Store::open(&db).ok().and_then(|st| st.setting("tray_promoted").ok().flatten()).is_some();
            if show_tray && !promoted { let db2 = db.clone(); std::thread::spawn(move || {
                for _ in 0..5 {
                    std::thread::sleep(std::time::Duration::from_secs(6));
                    if win::promote_tray_icon("fsearch\\fsearch.exe") {
                        if let Ok(st) = crate::store::Store::open(&db2) { let _ = st.set_setting("tray_promoted", "1"); }
                        break;
                    }
                }
            }); }

            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        });
    }
}
