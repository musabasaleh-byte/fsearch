//! Small Windows-only helpers for background operation. No-ops elsewhere.

/// Lower CPU priority so indexing never competes with foreground work.
pub fn low_priority() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS, PROCESS_MODE_BACKGROUND_BEGIN};
        // Background mode also lowers disk priority, so indexing never competes with what the user is doing.
        if SetPriorityClass(GetCurrentProcess(), PROCESS_MODE_BACKGROUND_BEGIN) == 0 { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS); }
    }
}

/// Close the console window when started by Task Scheduler (output goes to the log file instead).
pub fn detach_console() {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::FreeConsole();
    }
}

/// A command that never opens a console window (every console program started from a GUI program would otherwise flash one).
pub fn quiet_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    #[allow(unused_mut)]
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);   // CREATE_NO_WINDOW
    }
    cmd
}

/// True when some process currently holds the named mutex (the service holds one while it runs). No process is started.
pub fn mutex_exists(name: &str) -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
        use windows_sys::Win32::System::Threading::OpenMutexW;
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let handle = OpenMutexW(SYNCHRONIZE, 0, wide.as_ptr());
        if handle.is_null() { return false; }
        CloseHandle(handle);
        return true;
    }
    #[cfg(not(windows))]
    { let _ = name; false }
}

/// Whether a process with this id is alive (used on non-Windows, where there is no named mutex).
pub fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    { let _ = pid; false }
    #[cfg(not(windows))]
    { std::path::Path::new(&format!("/proc/{pid}")).exists() }
}

/// Ends a process immediately, without spawning taskkill.
pub fn kill_process(pid: u32) -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle.is_null() { return false; }
        let ok = TerminateProcess(handle, 1) != 0;
        CloseHandle(handle);
        return ok;
    }
    #[cfg(not(windows))]
    { let _ = pid; false }
}

/// True for the first caller; false when another process already holds `name`. The handle lives until exit.
pub fn single_instance(name: &str) -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
        use windows_sys::Win32::System::Threading::CreateMutexW;
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let handle = CreateMutexW(std::ptr::null(), 1, wide.as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS { if !handle.is_null() { CloseHandle(handle); } return false; }
        return true;
    }
    #[cfg(not(windows))]
    { let _ = name; true }
}

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() }

/// Registers `command` to run at every sign-in for the current user (no administrator rights needed).
pub fn autostart_set(command: &str) -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
        let data = wide(command);
        return RegSetKeyValueW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), wide("fsearch").as_ptr(), REG_SZ,
            data.as_ptr().cast(), (data.len() * 2) as u32) == 0;
    }
    #[cfg(not(windows))]
    { let _ = command; false }
}

pub fn autostart_remove() -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Registry::{RegDeleteKeyValueW, HKEY_CURRENT_USER};
        return RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), wide("fsearch").as_ptr()) == 0;
    }
    #[cfg(not(windows))]
    false
}

pub fn autostart_exists() -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
        return RegGetValueW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), wide("fsearch").as_ptr(), RRF_RT_REG_SZ,
            std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) == 0;
    }
    #[cfg(not(windows))]
    false
}

/// Start a program with no console window, independent of this process.
pub fn spawn_detached(program: &std::path::Path, args: &[&std::ffi::OsStr]) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000 | 0x0000_0008);   // CREATE_NO_WINDOW | DETACHED_PROCESS
    }
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().map(|_| ())
}

pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;
pub const DEFAULT_HOTKEY: &str = "ctrl+shift+space";

/// "ctrl+shift+space" -> (modifier bits, Windows virtual-key code). Needs at least one modifier.
pub fn parse_hotkey(text: &str) -> Option<(u32, u32)> {
    let mut mods = 0;
    let mut key = None;
    for part in text.split('+').map(|p| p.trim().to_ascii_lowercase()) {
        match part.as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "win" | "windows" => mods |= MOD_WIN,
            "space" => key = Some(0x20),
            k if k.len() == 1 && k.as_bytes()[0].is_ascii_alphanumeric() => key = Some(k.to_ascii_uppercase().as_bytes()[0] as u32),
            k if k.starts_with('f') && k[1..].parse::<u32>().map(|n| (1..=12).contains(&n)).unwrap_or(false) => key = Some(0x70 + k[1..].parse::<u32>().unwrap() - 1),
            _ => return None,
        }
    }
    if mods == 0 { return None; }
    key.map(|k| (mods, k))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_hotkeys() {
        assert_eq!(parse_hotkey("ctrl+shift+space"), Some((MOD_CONTROL | MOD_SHIFT, 0x20)));
        assert_eq!(parse_hotkey("Alt + F"), Some((MOD_ALT, b'F' as u32)));
        assert_eq!(parse_hotkey("win+f12"), Some((MOD_WIN, 0x7B)));
        assert_eq!(parse_hotkey("space"), None);          // needs a modifier
        assert_eq!(parse_hotkey("ctrl+"), None);          // needs a key
        assert_eq!(parse_hotkey("ctrl+f13"), None);
        assert_eq!(parse_hotkey("ctrl+enter"), None);
    }
}

/// "AppID<TAB>Name" lines -> Microsoft Store (UWP) apps. Classic apps (no '!' in the id) are covered by their Start-menu shortcuts.
pub fn parse_start_apps(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.trim_end_matches('\r').split_once('\t'))
        .filter(|(id, name)| id.contains('!') && !name.trim().is_empty())
        .map(|(id, name)| (id.trim().to_string(), name.trim().to_string()))
        .collect()
}

/// Installed Microsoft Store apps via PowerShell's Get-StartApps (empty off Windows or on failure).
pub fn store_apps() -> Vec<(String, String)> {
    #[cfg(windows)]
    {
        let script = "[Console]::OutputEncoding=[Text.Encoding]::UTF8; Get-StartApps | ForEach-Object { $_.AppID + [char]9 + $_.Name }";
        if let Ok(out) = quiet_command("powershell").args(["-NoProfile", "-NonInteractive", "-Command", script]).output() {
            return parse_start_apps(&String::from_utf8_lossy(&out.stdout));
        }
    }
    Vec::new()
}

#[cfg(test)]
mod store_app_tests {
    use super::parse_start_apps;
    #[test]
    fn keeps_only_store_apps() {
        let text = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App\tCalculator\r\n{1AC14E77}\\Chrome\\chrome.exe\tChrome\r\nbad line\r\nMicrosoft.Settings!App\t \r\n";
        assert_eq!(parse_start_apps(text), vec![("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".to_string(), "Calculator".to_string())]);
    }
}

/// Windows folder-chooser dialog (via PowerShell, so no extra dependencies). `None` when cancelled or unavailable.
pub fn pick_folder() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        let script = "Add-Type -AssemblyName System.Windows.Forms; $d=New-Object System.Windows.Forms.FolderBrowserDialog; $d.Description='Choose a folder to index'; if($d.ShowDialog() -eq 'OK'){ [Console]::OutputEncoding=[Text.Encoding]::UTF8; $d.SelectedPath }";
        let out = quiet_command("powershell").args(["-NoProfile", "-STA", "-Command", script]).output().ok()?;
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if path.is_empty() { None } else { Some(std::path::PathBuf::from(path)) }
    }
    #[cfg(not(windows))]
    None
}

/// A monitor's rectangle in physical pixels, in virtual-desktop coordinates (the primary monitor starts at 0,0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonitorRect { pub left: i32, pub top: i32, pub right: i32, pub bottom: i32 }

impl MonitorRect {
    pub fn width(&self) -> i32 { self.right - self.left }
    pub fn height(&self) -> i32 { self.bottom - self.top }
}

/// The monitor the mouse pointer is on right now (None off Windows).
pub fn cursor_monitor() -> Option<MonitorRect> {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
        use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut pt = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut pt) == 0 { return None; }
        let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 { return None; }
        let r = info.rcMonitor;
        return Some(MonitorRect { left: r.left, top: r.top, right: r.right, bottom: r.bottom });
    }
    #[cfg(not(windows))]
    None
}

/// Report real pixels (not scaled coordinates) from the Windows monitor APIs. Call before creating any window.
pub fn set_dpi_aware() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// From `reg query ... /s /v ExecutablePath` output: the registry key whose ExecutablePath ends with `exe_suffix`.
pub fn find_notify_key(reg_output: &str, exe_suffix: &str) -> Option<String> {
    let suffix = exe_suffix.to_lowercase().replace('/', "\\");
    let mut key: Option<&str> = None;
    for line in reg_output.lines() {
        let t = line.trim();
        if t.to_ascii_uppercase().starts_with("HKEY_") { key = Some(t); continue; }
        if t.starts_with("ExecutablePath") {
            let value = t.split("REG_SZ").nth(1).unwrap_or("").trim().to_lowercase().replace('/', "\\");
            if value.ends_with(&suffix) { return key.map(String::from); }
        }
    }
    None
}

/// Windows 11 hides new notification-area icons in the overflow; mark ours as "shown" (best effort).
/// The entry only exists a few seconds after the icon was first added.
pub fn promote_tray_icon(exe_suffix: &str) -> bool {
    #[cfg(windows)]
    {
        let base = r"HKCU\Control Panel\NotifyIconSettings";
        let Ok(out) = quiet_command("reg").args(["query", base, "/s", "/v", "ExecutablePath"]).output() else { return false };
        let Some(key) = find_notify_key(&String::from_utf8_lossy(&out.stdout), exe_suffix) else { return false };
        return quiet_command("reg").args(["add", &key, "/v", "IsPromoted", "/t", "REG_DWORD", "/d", "1", "/f"]).status().map(|s| s.success()).unwrap_or(false);
    }
    #[cfg(not(windows))]
    { let _ = exe_suffix; false }
}

#[cfg(test)]
mod monitor_tests {
    use super::*;
    #[test]
    fn finds_the_notify_icon_entry_for_our_exe() {
        let out = "\r\nHKEY_CURRENT_USER\\Control Panel\\NotifyIconSettings\\111\r\n    ExecutablePath    REG_SZ    C:\\Windows\\explorer.exe\r\n\r\nHKEY_CURRENT_USER\\Control Panel\\NotifyIconSettings\\222\r\n    ExecutablePath    REG_SZ    C:\\Users\\Musab Saleh\\AppData\\Local\\fsearch\\fsearch.exe\r\n";
        assert_eq!(find_notify_key(out, "fsearch\\fsearch.exe").as_deref(), Some("HKEY_CURRENT_USER\\Control Panel\\NotifyIconSettings\\222"));
        assert_eq!(find_notify_key(out, "other\\app.exe"), None);
        assert_eq!(find_notify_key("", "fsearch.exe"), None);
    }
    #[test]
    fn monitor_rect_sizes() {
        let m = MonitorRect { left: -1920, top: 0, right: 0, bottom: 1080 };
        assert_eq!((m.width(), m.height()), (1920, 1080));
    }
}

/// Copies a rectangle of the screen (physical pixels, virtual-desktop coordinates) as RGBA. Used to frost what is behind the
/// search window at the moment it opens. The picture stays in memory and is never saved or sent anywhere.
pub fn capture_screen(x: i32, y: i32, w: i32, h: i32) -> Option<(usize, usize, Vec<u8>)> {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Graphics::Gdi::{
            BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject,
            BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, SRCCOPY,
        };
        if w <= 0 || h <= 0 || w > 8192 || h > 8192 { return None; }
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() { return None; }
        let mem = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, w, h);
        let mut result = None;
        if !mem.is_null() && !bitmap.is_null() {
            let old = SelectObject(mem, bitmap);
            let copied = BitBlt(mem, 0, 0, w, h, screen, x, y, SRCCOPY | CAPTUREBLT) != 0;
            SelectObject(mem, old);                       // GetDIBits needs the bitmap to be free
            if copied {
                let mut info: BITMAPINFO = std::mem::zeroed();
                info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                info.bmiHeader.biWidth = w;
                info.bmiHeader.biHeight = -h;             // top-down rows
                info.bmiHeader.biPlanes = 1;
                info.bmiHeader.biBitCount = 32;
                info.bmiHeader.biCompression = BI_RGB;
                let mut px = vec![0u8; (w * h * 4) as usize];
                if GetDIBits(mem, bitmap, 0, h as u32, px.as_mut_ptr() as *mut _, &mut info, DIB_RGB_COLORS) != 0 {
                    for p in px.chunks_exact_mut(4) { p.swap(0, 2); p[3] = 255; }   // BGRA -> opaque RGBA
                    result = Some((w as usize, h as usize, px));
                }
            }
        }
        if !bitmap.is_null() { DeleteObject(bitmap); }
        if !mem.is_null() { DeleteDC(mem); }
        ReleaseDC(std::ptr::null_mut(), screen);
        return result;
    }
    #[cfg(not(windows))]
    { let _ = (x, y, w, h); None }
}

/// Starts a file with the "Run as administrator" verb (Windows asks for confirmation itself).
pub fn run_as_admin(path: &str) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let verb: Vec<u16> = "runas".encode_utf16().chain(Some(0)).collect();
        let file: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL);
    }
    #[cfg(not(windows))]
    { let _ = path; }
}

/// The "Open with…" chooser. Run in its own process so it stays open after the search window closes.
pub fn open_with(path: &str) {
    #[cfg(windows)]
    { let _ = std::process::Command::new("rundll32.exe").arg("shell32.dll,OpenAs_RunDLL").arg(path).spawn(); }
    #[cfg(not(windows))]
    { let _ = path; }
}

/// Puts the file itself on the clipboard (paste it in Explorer, an e-mail, a chat), like Copy in Explorer.
pub fn copy_file_to_clipboard(path: &str) {
    #[cfg(windows)]
    { let _ = quiet_command("powershell").args(["-NoProfile", "-STA", "-Command", "Set-Clipboard -LiteralPath $env:FS_PATH"]).env("FS_PATH", path).spawn(); }
    #[cfg(not(windows))]
    { let _ = path; }
}

/// Windows Terminal in `dir`.
pub fn open_terminal(dir: &str) {
    #[cfg(windows)]
    { let _ = std::process::Command::new("wt.exe").arg("-d").arg(dir).spawn(); }
    #[cfg(not(windows))]
    { let _ = dir; }
}

/// Local fixed disks ("C:\\", "D:\\"…). Empty elsewhere.
pub fn fixed_drives() -> Vec<std::path::PathBuf> {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
        let mask = GetLogicalDrives();
        (0..26u32).filter(|i| mask & (1 << i) != 0).filter_map(|i| {
            let root = format!("{}:\\", (b'A' + i as u8) as char);
            let wide: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
            if GetDriveTypeW(wide.as_ptr()) == 3 { Some(std::path::PathBuf::from(root)) } else { None }
        }).collect()
    }
    #[cfg(not(windows))]
    { Vec::new() }
}

// ---- the launcher window stays running (hidden) and is shown by signalling a named event, so opening it is instant

pub const LAUNCHER_EVENT: &str = "Local\\fsearch-launcher-show";

#[cfg(windows)]
fn wide_name(s: &str) -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() }

/// Asks a running launcher to open (or close, if it is open). False when no launcher is running.
pub fn signal_launcher() -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{OpenEventW, SetEvent};
        use windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
        let h = OpenEventW(0x0002, 0, wide_name(LAUNCHER_EVENT).as_ptr());   // EVENT_MODIFY_STATE
        if h.is_null() { return false; }
        // The caller has just received the user's keypress, so it may let the launcher take the keyboard focus at once.
        AllowSetForegroundWindow(u32::MAX);
        let ok = SetEvent(h) != 0;
        CloseHandle(h);
        ok
    }
    #[cfg(not(windows))]
    { false }
}

/// Is a launcher window already running (hidden or shown)?
pub fn launcher_running() -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::OpenEventW;
        let h = OpenEventW(0x0002, 0, wide_name(LAUNCHER_EVENT).as_ptr());
        if h.is_null() { return false; }
        CloseHandle(h);
        true
    }
    #[cfg(not(windows))]
    { false }
}

/// The launcher creates the event others signal. 0 when it could not be created.
pub fn create_launcher_event() -> usize {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Threading::CreateEventW;
        CreateEventW(std::ptr::null(), 0, 0, wide_name(LAUNCHER_EVENT).as_ptr()) as usize
    }
    #[cfg(not(windows))]
    { 0 }
}

/// Blocks until the event is signalled. False if waiting failed.
pub fn wait_launcher_event(handle: usize) -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        WaitForSingleObject(handle as _, 0xFFFF_FFFF) == 0
    }
    #[cfg(not(windows))]
    { let _ = handle; false }
}
