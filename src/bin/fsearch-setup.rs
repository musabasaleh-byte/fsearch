//! The installer: one file that carries the service and the launcher, puts them in place and starts the first-run wizard.
#![cfg_attr(windows, windows_subsystem = "windows")]
use fsearch::{service, store::{default_db_path, Store}, win};
use std::path::Path;

static DAEMON: &[u8] = include_bytes!("../../payload/fsearch.exe");
static GUI: &[u8] = include_bytes!("../../payload/fsearch-gui.exe");

fn message(text: &str) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
        let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() };
        MessageBoxW(std::ptr::null_mut(), wide(text).as_ptr(), wide("fsearch Setup").as_ptr(), MB_OK | MB_ICONINFORMATION);
    }
    #[cfg(not(windows))]
    eprintln!("{text}");
}

/// A running program keeps its file locked for a moment after it is told to stop, so retry.
fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut last = Ok(());
    for _ in 0..40 {
        last = std::fs::write(path, bytes);
        if last.is_ok() { return last; }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    last
}

fn install() -> Result<bool, String> {
    let db = default_db_path();
    let dir = service::data_dir(&db);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    service::stop_service(&db);
    for exe in ["fsearch-gui.exe"] { let _ = win::quiet_command("taskkill").args(["/f", "/im", exe]).output(); }
    write(&service::daemon_exe(&db), DAEMON).map_err(|e| format!("cannot write fsearch.exe: {e}"))?;
    write(&service::gui_exe(&db), GUI).map_err(|e| format!("cannot write fsearch-gui.exe: {e}"))?;
    service::unblock(&service::daemon_exe(&db));
    service::unblock(&service::gui_exe(&db));
    let _ = std::fs::write(service::ico_path(&db), service::ICO);
    let store = Store::open(&db).map_err(|e| e.to_string())?;
    // Already set up before: this was an update, so keep the index and settings and just restart.
    if store.setting("setup_done").ok().flatten().is_some() {
        service::finish_install(&db, &store).map_err(|e| e.to_string())?;
        return Ok(true);
    }
    win::spawn_detached(&service::gui_exe(&db), &[std::ffi::OsStr::new("--setup")]).map_err(|e| format!("cannot start the setup window: {e}"))?;
    Ok(false)
}

fn main() {
    match install() {
        Ok(true) => message("fsearch was updated. Your index and settings were kept."),
        Ok(false) => {}
        Err(e) => message(&format!("Setup could not finish.\n\n{e}")),
    }
}
