//! First-run setup: pick what to index, run the first full scan (like Spotlight's initial indexing), then switch the service on.
use crate::indexer::{self, Options};
use crate::service;
use crate::settings_ui::{install_fonts, pretty_hotkey};
use crate::store::{default_db_path, Store};
use crate::win;
use eframe::egui::{self, Color32, RichText, Vec2};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(10, 132, 255);
const GREEN: Color32 = Color32::from_rgb(40, 160, 80);
const RED: Color32 = Color32::from_rgb(210, 60, 50);
const HOTKEYS: [&str; 5] = ["ctrl+shift+space", "ctrl+alt+f", "alt+space", "ctrl+alt+space", "win+shift+f"];

/// True when the program has never been set up on this PC: no `setup_done` mark and an empty index.
pub fn needed(store: &Store) -> bool {
    store.setting("setup_done").ok().flatten().is_none() && store.stats().map(|s| s.files == 0).unwrap_or(true)
}

#[derive(PartialEq)]
enum Step { Locations, Scanning, Done }

struct Choice { path: PathBuf, label: String, note: String, on: bool }

enum Msg {
    Progress { root: String, scanned: usize },
    Phase(String),
    Total { n: usize, done: bool },
    Finished(Result<Vec<String>, String>),
}

struct App {
    step: Step,
    choices: Vec<Choice>,
    content: bool,
    autostart: bool,
    desktop: bool,
    start_menu: bool,
    tray: bool,
    pin: bool,
    hotkey: usize,
    rx: Option<Receiver<Msg>>,
    root: String,
    scanned: usize,
    base: usize,
    total: usize,
    total_known: bool,
    phase: String,
    started: Instant,
    result: Option<Result<Vec<String>, String>>,
    centered: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);
        let mut visuals = egui::Visuals::light();
        visuals.selection.bg_fill = ACCENT;
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, Color32::from_gray(205));
        cc.egui_ctx.set_visuals(visuals);
        let mut choices = Vec::new();
        if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from).filter(|p| p.is_dir()) {
            choices.push(Choice { label: "Your files".into(), note: format!("{} — Documents, Desktop, Downloads, Pictures, OneDrive…", home.display()), path: home, on: true });
        }
        for drive in win::fixed_drives() {
            choices.push(Choice { label: format!("Entire drive {}", drive.display()), note: "Everything on the disk, including programs. The first scan takes longer.".into(), path: drive, on: false });
        }
        Self { step: Step::Locations, choices, content: false, autostart: true, desktop: true, start_menu: true, tray: true, pin: true, hotkey: 0, rx: None, root: String::new(), scanned: 0, base: 0, total: 0, total_known: false,
               phase: String::new(), started: Instant::now(), result: None, centered: false }
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        // A folder inside another chosen folder is already covered.
        let all: Vec<PathBuf> = self.choices.iter().filter(|c| c.on).map(|c| c.path.clone()).collect();
        let roots: Vec<PathBuf> = all.iter().filter(|p| !all.iter().any(|q| q != *p && p.starts_with(q))).cloned().collect();
        let (content, autostart, hotkey) = (self.content, self.autostart, HOTKEYS[self.hotkey].to_string());
        let (desktop, start_menu, tray) = (self.desktop, self.start_menu, self.tray);
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        self.rx = Some(rx);
        self.step = Step::Scanning;
        self.started = Instant::now();
        std::thread::spawn(move || {
            let send = |m: Msg| { let _ = tx.send(m); ctx.request_repaint(); };
            let run = || -> anyhow::Result<Vec<String>> {
                let db = default_db_path();
                let store = Store::open(&db)?;
                for r in &roots { store.add_root(&r.to_string_lossy(), content)?; }
                send(Msg::Phase("Finding apps…".into()));
                service::ensure_start_menu_roots(&store);
                service::sync_store_apps_if_stale(&store);
                let mut all = store.roots()?;
                all.sort_by_key(|(p, _)| !p.to_lowercase().contains("start menu"));   // apps first: small, and what people search for most
                // Count the files in a second thread so the bar can show a real percentage.
                let dirs: Vec<PathBuf> = all.iter().map(|(p, _)| PathBuf::from(p)).filter(|p| p.is_dir()).collect();
                let counter = tx.clone();
                let counter_ctx = ctx.clone();
                std::thread::spawn(move || {
                    let mut n = 0usize;
                    for dir in dirs {
                        let opts = Options { base: Some(dir.clone()), ..Options::default() };
                        let walker = walkdir::WalkDir::new(&dir).follow_links(false).into_iter().filter_entry(|e| !opts.is_excluded(e.path()) || e.path() == dir);
                        for e in walker.filter_map(|e| e.ok()) {
                            if e.file_type().is_file() { n += 1; }
                            if n % 4000 == 0 && n > 0 { let _ = counter.send(Msg::Total { n, done: false }); counter_ctx.request_repaint(); }
                        }
                    }
                    let _ = counter.send(Msg::Total { n, done: true });
                    counter_ctx.request_repaint();
                });
                for (path, with_content) in all {
                    let dir = PathBuf::from(&path);
                    if !dir.is_dir() { continue; }
                    send(Msg::Phase(format!("Scanning {path}")));
                    let opts = Options { content: with_content, ..Options::default() };
                    let base = path.clone();
                    indexer::crawl(&store, &dir, &opts, |s| send(Msg::Progress { root: base.clone(), scanned: s.scanned }))?;
                }
                send(Msg::Phase("Turning fsearch on…".into()));
                store.set_setting("hotkey", &hotkey)?;
                store.set_setting("tray_icon", if tray { "1" } else { "0" })?;
                store.set_setting("setup_done", "1")?;
                let lines = service::finish_install(&db, &store)?;
                if !autostart { win::autostart_remove(); }
                if !start_menu { service::remove_start_menu_shortcut("fsearch"); }
                if desktop { service::desktop_shortcut(&db, &store); }
                Ok(lines)
            };
            send(Msg::Finished(run().map_err(|e| e.to_string())));
        });
    }

    fn locations(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label(RichText::new("Choose what fsearch should search").size(22.0).strong());
        ui.add_space(4.0);
        ui.label(RichText::new("fsearch reads these places once now (a full scan), then keeps them up to date in the background.").color(Color32::from_gray(110)));
        ui.add_space(12.0);
        egui::Frame::group(ui.style()).fill(Color32::WHITE).corner_radius(10.0).inner_margin(12.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().max_height(110.0).show(ui, |ui| {
                let mut remove = None;
                for (i, c) in self.choices.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut c.on, RichText::new(crate::bidi::visual(&c.label)).strong());
                        if !c.note.is_empty() { ui.label(RichText::new(crate::bidi::visual(&c.note)).size(12.0).color(Color32::from_gray(120))); }
                        if c.note.is_empty() && ui.small_button("Remove").clicked() { remove = Some(i); }
                    });
                }
                if let Some(i) = remove { self.choices.remove(i); }
            });
        });
        ui.add_space(8.0);
        if ui.button("+  Add another folder…").clicked() {
            if let Some(p) = win::pick_folder() {
                if !self.choices.iter().any(|c| c.path == p) {
                    self.choices.push(Choice { label: p.display().to_string(), note: String::new(), path: p, on: true });
                }
            }
        }
        ui.add_space(14.0);
        ui.checkbox(&mut self.content, "Also search inside documents (text, code, Word files)");
        ui.label(RichText::new("Slower, and antivirus checks every file read, so it is off by default. Online-only OneDrive files are never downloaded.").size(12.0).color(Color32::from_gray(120)));
        ui.add_space(6.0);
        ui.checkbox(&mut self.autostart, "Start fsearch when I sign in");
        ui.add_space(10.0);
        ui.label(RichText::new("Where to put fsearch").strong());
        ui.checkbox(&mut self.desktop, "Desktop icon");
        ui.checkbox(&mut self.start_menu, "Start menu entry");
        ui.checkbox(&mut self.tray, "Icon in the system tray (next to the clock)");
        ui.checkbox(&mut self.pin, "Pin to the taskbar (I'll show you how at the end)");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Open with");
            egui::ComboBox::from_id_salt("hotkey").selected_text(pretty_hotkey(HOTKEYS[self.hotkey])).show_ui(ui, |ui| {
                for (i, h) in HOTKEYS.iter().enumerate() { ui.selectable_value(&mut self.hotkey, i, pretty_hotkey(h)); }
            });
            ui.label(RichText::new("(change it any time in Settings)").size(12.0).color(Color32::from_gray(120)));
        });
        ui.add_space(14.0);
        let any = self.choices.iter().any(|c| c.on);
        ui.horizontal(|ui| {
            if ui.add_enabled(any, egui::Button::new(RichText::new("  Start scan  ").color(Color32::WHITE).size(15.0)).fill(ACCENT).min_size(Vec2::new(0.0, 32.0))).clicked() {
                self.start_scan(ctx);
            }
            if !any { ui.label(RichText::new("Pick at least one place.").color(RED)); }
        });
    }

    fn scanning(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Indexing your files…").size(22.0).strong());
        ui.add_space(6.0);
        ui.label(RichText::new("You can keep using the PC. Search works as soon as this finishes.").color(Color32::from_gray(110)));
        ui.add_space(18.0);
        let secs = self.started.elapsed().as_secs_f64().max(0.1);
        let bar = Vec2::new(ui.available_width(), 14.0);
        let (rect, _) = ui.allocate_exact_size(bar, egui::Sense::hover());
        ui.painter().rect_filled(rect, 7.0, Color32::from_gray(225));
        if self.total_known && self.total > 0 {
            let frac = (self.scanned as f32 / self.total as f32).clamp(0.0, 0.99);
            let mut fill = rect;
            fill.set_width((rect.width() * frac).max(14.0));
            ui.painter().rect_filled(fill, 7.0, ACCENT);
        } else {
            // Total still being counted: a sliding segment shows that work is going on.
            let t = ui.input(|i| i.time) as f32;
            let w = rect.width() * 0.25;
            let x = rect.left() + (rect.width() - w) * ((t * 0.8).sin() * 0.5 + 0.5);
            ui.painter().rect_filled(egui::Rect::from_min_size(egui::Pos2::new(x, rect.top()), Vec2::new(w, rect.height())), 7.0, ACCENT);
        }
        ui.add_space(10.0);
        if self.total_known && self.total > 0 {
            let frac = (self.scanned as f64 / self.total as f64).clamp(0.0, 0.99);
            ui.label(RichText::new(format!("{:.0}%", frac * 100.0)).size(30.0).strong());
            ui.label(RichText::new(format!("{} of about {} files", self.scanned, self.total)).size(16.0));
            let eta = if frac > 0.02 { Some(secs / frac * (1.0 - frac)) } else { None };
            let rate = format!("{:.0} files per second · {:.0}s elapsed", self.scanned as f64 / secs, secs);
            ui.label(RichText::new(match eta { Some(e) => format!("{rate} · about {} left", if e >= 90.0 { format!("{:.0} min", e / 60.0) } else { format!("{e:.0}s") }), None => rate }).color(Color32::from_gray(120)));
        } else {
            ui.label(RichText::new(format!("{} files indexed", self.scanned)).size(26.0).strong());
            ui.label(RichText::new(format!("Counting files so the progress can be shown… ({} found) · {:.0}s", self.total, secs)).color(Color32::from_gray(120)));
        }
        ui.add_space(6.0);
        ui.label(RichText::new(crate::bidi::visual(&self.phase)).color(Color32::from_gray(90)));
    }

    fn done(&mut self, ui: &mut egui::Ui) {
        match self.result.clone() {
            Some(Ok(lines)) => {
                ui.label(RichText::new("✔  fsearch is ready").size(22.0).strong().color(GREEN));
                ui.add_space(8.0);
                ui.label(format!("{} files indexed in {:.0} seconds.", self.scanned, self.started.elapsed().as_secs_f64()));
                ui.add_space(6.0);
                ui.label(RichText::new(format!("Press {} anywhere to search.", pretty_hotkey(HOTKEYS[self.hotkey]))).size(18.0));
                ui.add_space(4.0);
                ui.label(RichText::new("It is also in the Start menu and the system tray, and Settings are under “fsearch Settings”.").color(Color32::from_gray(110)));
                if self.pin {
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Pin to taskbar…").clicked() { service::reveal_start_menu_shortcut(&default_db_path()); }
                        ui.label(RichText::new("Right-click the highlighted “fsearch” → Show more options → Pin to taskbar.\nWindows does not let programs pin themselves.").size(12.0).color(Color32::from_gray(110)));
                    });
                }
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(RichText::new("  Open fsearch  ").color(Color32::WHITE).size(15.0)).fill(ACCENT).min_size(Vec2::new(0.0, 32.0))).clicked() {
                        let _ = win::spawn_detached(&service::gui_exe(&default_db_path()), &[]);
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.button("Close").clicked() { ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close); }
                });
                ui.add_space(14.0);
                ui.collapsing("Details", |ui| for l in lines { ui.label(RichText::new(l).size(12.0).color(Color32::from_gray(110))); });
            }
            Some(Err(e)) => {
                ui.label(RichText::new("Setup could not finish").size(22.0).strong().color(RED));
                ui.add_space(8.0);
                ui.label(e);
                ui.add_space(14.0);
                if ui.button("Back").clicked() { self.step = Step::Locations; self.result = None; }
            }
            None => {}
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.centered {
            self.centered = true;
            if let Some(cmd) = egui::ViewportCommand::center_on_screen(ctx) { ctx.send_viewport_cmd(cmd); }
        }
        if let Some(rx) = &self.rx {
            while let Ok(m) = rx.try_recv() {
                match m {
                    Msg::Progress { root, scanned } => {
                        if root != self.root { self.base = self.scanned; self.root = root; }
                        self.scanned = self.base + scanned;
                    }
                    Msg::Phase(p) => self.phase = p,
                    Msg::Total { n, done } => { self.total = n; self.total_known = done; }
                    Msg::Finished(r) => { self.result = Some(r); self.step = Step::Done; }
                }
            }
        }
        if self.step == Step::Scanning { ctx.request_repaint_after(Duration::from_millis(40)); }
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(Color32::from_gray(246)).inner_margin(28.0)).show(ctx, |ui| {
            match self.step {
                Step::Locations => self.locations(ui, ctx),
                Step::Scanning => self.scanning(ui),
                Step::Done => self.done(ui),
            }
        });
    }
}

pub fn run() -> eframe::Result {
    if !win::single_instance("Local\\fsearch-setup") { return Ok(()); }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([660.0, 700.0]).with_resizable(false).with_title("fsearch Setup")
            .with_icon(egui::IconData { rgba: include_bytes!("../assets/icon-64.rgba").to_vec(), width: 64, height: 64 }),
        ..Default::default()
    };
    eframe::run_native("fsearch Setup", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
