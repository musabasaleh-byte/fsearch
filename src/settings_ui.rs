//! The "fsearch Settings" window, laid out like macOS System Settings > Siri & Spotlight.
use crate::categories::CATEGORIES;
use crate::look::{Look, Transparency};
use crate::service;
use crate::store::{default_db_path, Store};
use crate::win;
use eframe::egui::{self, Align2, Color32, FontData, FontDefinitions, FontFamily, FontId, Key, RichText, Sense, Vec2};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(10, 132, 255);
const GREEN: Color32 = Color32::from_rgb(40, 160, 80);
const RED: Color32 = Color32::from_rgb(210, 60, 50);
const DESIGN_W: f32 = 780.0;
const DESIGN_H: f32 = 580.0;

/// Segoe UI for the look, Tahoma as the Arabic fallback.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    for (key, file) in [("ui", "segoeui.ttf"), ("arabic", "tahoma.ttf"), ("arabic2", "arial.ttf"), ("symbols", "seguisym.ttf")] {
        if let Ok(bytes) = std::fs::read(Path::new(r"C:\Windows\Fonts").join(file)) {
            fonts.font_data.insert(key.to_string(), Arc::new(FontData::from_owned(bytes)));
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                let list = fonts.families.entry(family).or_default();
                if key == "ui" { list.insert(0, key.to_string()); } else { list.push(key.to_string()); }
            }
        }
    }
    ctx.set_fonts(fonts);
}

/// egui key -> the name `win::parse_hotkey` understands.
fn key_name(key: Key) -> Option<String> {
    let dbg = format!("{key:?}");
    match dbg.as_str() {
        "Space" => Some("space".into()),
        s if s.len() == 1 && s.as_bytes()[0].is_ascii_alphabetic() => Some(s.to_ascii_lowercase()),
        s if s.len() == 4 && s.starts_with("Num") && s.as_bytes()[3].is_ascii_digit() => Some(s[3..].to_string()),
        s if s.starts_with('F') && s[1..].parse::<u32>().map(|n| (1..=12).contains(&n)).unwrap_or(false) => Some(s.to_ascii_lowercase()),
        _ => None,
    }
}

pub fn pretty_hotkey(combo: &str) -> String {
    combo.split('+').map(|p| match p.trim() {
        "ctrl" => "Ctrl".to_string(), "alt" => "Alt".to_string(), "shift" => "Shift".to_string(), "win" => "Win".to_string(),
        "space" => "Space".to_string(),
        other => other.to_uppercase(),
    }).collect::<Vec<_>>().join(" + ")
}

/// Combinations other programs commonly own; we warn but still allow them.
fn conflict_hint(combo: &str) -> Option<&'static str> {
    match combo {
        "ctrl+alt+space" => Some("Claude desktop uses this by default."),
        "alt+space" => Some("Windows uses this for the window menu (PowerToys Run uses it too)."),
        "win+space" => Some("Windows uses this to switch the keyboard language."),
        "ctrl+space" => Some("Many input methods use this."),
        "ctrl+shift+esc" | "ctrl+alt+delete" => Some("Reserved by Windows."),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Page { Results, Privacy, Appearance, Shortcut, General, Folders, Index }

impl Page {
    const ALL: [Page; 7] = [Page::Results, Page::Privacy, Page::Appearance, Page::Shortcut, Page::General, Page::Folders, Page::Index];
    fn title(self) -> &'static str {
        match self { Page::Results => "Search Results", Page::Privacy => "Privacy", Page::Appearance => "Appearance", Page::Shortcut => "Keyboard Shortcut", Page::General => "General", Page::Folders => "Folders", Page::Index => "Index" }
    }
    fn icon(self) -> (Color32, &'static str) {
        match self {
            Page::Results => (Color32::from_rgb(10, 132, 255), "Q"),
            Page::Privacy => (Color32::from_rgb(88, 86, 214), "P"),
            Page::Appearance => (Color32::from_rgb(255, 69, 58), "A"),
            Page::Shortcut => (Color32::from_rgb(142, 142, 147), "K"),
            Page::General => (Color32::from_rgb(120, 120, 128), "G"),
            Page::Folders => (Color32::from_rgb(255, 159, 10), "F"),
            Page::Index => (Color32::from_rgb(52, 199, 89), "I"),
        }
    }
}

#[derive(Clone, Copy)]
enum Pick { Index, Privacy }

enum Done { Refresh, Message(String), Folder(Pick, Option<PathBuf>), Stats((i64, i64, i64)) }

struct App {
    db: PathBuf,
    store: Store,
    pending_look: Option<Instant>,
    inflight: Arc<std::sync::atomic::AtomicUsize>,
    page: Page,
    filter: String,
    roots: Vec<(String, bool)>,
    hidden: HashSet<String>,
    private_folders: Vec<String>,
    hotkey: String,
    recording: bool,
    autostart: bool,
    running: bool,
    hotkey_status: Option<String>,
    close_on_blur: bool,
    look: Look,
    max_rows: u32,
    stats: (i64, i64, i64),
    polled: Instant,
    stats_polled: Instant,
    message: String,
    confirm_rebuild: bool,
    placer: crate::placer::Placer,
    tx: Sender<Done>,
    rx: Receiver<Done>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);
        let mut visuals = egui::Visuals::light();
        for w in [&mut visuals.widgets.noninteractive, &mut visuals.widgets.inactive, &mut visuals.widgets.hovered, &mut visuals.widgets.active] {
            w.corner_radius = egui::CornerRadius::same(7);
        }
        visuals.selection.bg_fill = ACCENT;
        visuals.widgets.inactive.bg_fill = Color32::from_gray(255);
        visuals.widgets.inactive.weak_bg_fill = Color32::from_gray(255);
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, Color32::from_gray(205));
        cc.egui_ctx.set_visuals(visuals);
        let db = default_db_path();
        let store = Store::open(&db).expect("open index");
        let (tx, rx) = channel();
        let mut app = Self {
            db, store, pending_look: None, inflight: Arc::new(std::sync::atomic::AtomicUsize::new(0)), page: Page::Results, filter: String::new(), roots: vec![], hidden: HashSet::new(), private_folders: vec![],
            hotkey: String::new(), recording: false, autostart: false, running: false, hotkey_status: None,
            close_on_blur: true, look: Look::DEFAULT, max_rows: 8, stats: (0, 0, 0), polled: Instant::now(), stats_polled: Instant::now(),
            message: String::new(), confirm_rebuild: false, placer: crate::placer::Placer::new(), tx, rx,
        };
        if let Ok(name) = std::env::var("FSEARCH_SETTINGS_PAGE") {   // for documentation screenshots
            if let Some(page) = Page::ALL.into_iter().find(|p| p.title().eq_ignore_ascii_case(&name)) { app.page = page; }
        }
        app.refresh(true);
        app.refresh_stats();
        app
    }

    fn refresh(&mut self, with_service: bool) {
        if self.inflight.load(std::sync::atomic::Ordering::SeqCst) > 0 { return; }   // a save is still on its way; reading now would show the old value
        self.roots = self.store.roots().unwrap_or_default();
        self.hotkey = service::hotkey_label(&self.store);
        self.close_on_blur = self.store.setting("close_on_blur").ok().flatten().map(|v| v != "0").unwrap_or(true);
        if self.pending_look.is_none() && self.page != Page::Appearance { self.look = Look::load(&self.store); }
        self.max_rows = self.store.setting("max_rows").ok().flatten().and_then(|v| v.parse().ok()).unwrap_or(8);
        self.hidden = self.store.setting("hidden_categories").ok().flatten().unwrap_or_default().split(',').filter(|s| !s.is_empty()).map(String::from).collect();
        self.private_folders = self.store.setting("privacy_folders").ok().flatten().unwrap_or_default().lines().filter(|s| !s.is_empty()).map(String::from).collect();
        if with_service {
            self.autostart = win::autostart_exists();
            self.running = service::is_running(&self.db);
            self.hotkey_status = service::hotkey_status(&self.db);
            if std::env::var_os("FSEARCH_DOC_SHOTS").is_some() {   // documentation screenshots on a PC without the service
                self.autostart = true; self.running = true; self.hotkey_status = Some("ok".into());
            }
        }
    }

    /// Counting a big index takes a while, so it never runs on the UI thread.
    fn refresh_stats(&mut self) {
        self.stats_polled = Instant::now();
        let (db, tx) = (self.db.clone(), self.tx.clone());
        std::thread::spawn(move || {
            if let Ok(s) = Store::open(&db).and_then(|st| st.stats()) { let _ = tx.send(Done::Stats((s.files, s.with_content, s.db_bytes))); }
        });
    }

    /// Writes to the index database off the UI thread: while the service is saving a batch the database is locked for a
    /// moment, and waiting for it here would freeze the window ("Not responding").
    fn write_db(&self, job: impl FnOnce(&Store) + Send + 'static) {
        use std::sync::atomic::Ordering;
        let (db, tx, inflight) = (self.db.clone(), self.tx.clone(), self.inflight.clone());
        inflight.fetch_add(1, Ordering::SeqCst);
        std::thread::spawn(move || {
            if let Ok(store) = Store::open(&db) { job(&store); }
            inflight.fetch_sub(1, Ordering::SeqCst);
            let _ = tx.send(Done::Refresh);
        });
    }

    fn save_hidden(&mut self) {
        let mut list: Vec<&String> = self.hidden.iter().collect();
        list.sort();
        let value = list.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",");
        self.write_db(move |st| { let _ = st.set_setting("hidden_categories", &value); });
    }

    fn save_private(&mut self) {
        let value = self.private_folders.join("\n");
        self.write_db(move |st| { let _ = st.set_setting("privacy_folders", &value); });
    }

    /// Run slow work (process control, folder deletion) off the UI thread; `job` returns a message.
    fn background(&self, ctx: &egui::Context, job: impl FnOnce(&Path) -> String + Send + 'static) {
        let (db, tx, ctx) = (self.db.clone(), self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let msg = job(&db);
            let _ = tx.send(Done::Message(msg));
            let _ = tx.send(Done::Refresh);
            ctx.request_repaint();
        });
    }

    fn pick_folder(&self, ctx: &egui::Context, purpose: Pick) {
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || { let _ = tx.send(Done::Folder(purpose, win::pick_folder())); ctx.request_repaint(); });
    }

    fn set_hotkey(&mut self, ctx: &egui::Context, combo: String) {
        if win::parse_hotkey(&combo).is_none() { self.message = format!("'{combo}' is not a valid shortcut"); return; }
        { let c = combo.clone(); self.write_db(move |st| { let _ = st.set_setting("hotkey", &c); }); }
        self.hotkey = combo.clone();
        self.message = format!("Shortcut set to {}. Restarting the service…", pretty_hotkey(&combo));
        let _ = std::fs::remove_file(service::hotkey_status_path(&self.db));
        self.hotkey_status = None;
        self.background(ctx, |db| match service::restart_service(db) {
            Ok(()) => "Shortcut applied.".to_string(),
            Err(e) => format!("Saved, but the service could not be restarted: {e}"),
        });
    }

    fn record_keys(&mut self, ctx: &egui::Context) {
        let events = ctx.input(|i| i.events.clone());
        for event in events {
            let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { continue };
            if key == Key::Escape && !modifiers.any() { self.recording = false; self.message = "Cancelled.".into(); return; }
            let Some(name) = key_name(key) else { continue };
            if !(modifiers.ctrl || modifiers.alt || modifiers.shift) { self.message = "Include Ctrl, Alt or Shift in the shortcut.".into(); continue; }
            let mut parts = vec![];
            if modifiers.ctrl { parts.push("ctrl".to_string()); }
            if modifiers.alt { parts.push("alt".to_string()); }
            if modifiers.shift { parts.push("shift".to_string()); }
            parts.push(name);
            self.recording = false;
            self.set_hotkey(ctx, parts.join("+"));
            return;
        }
    }

    // ---------- drawing helpers
    fn group(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui)) {
        egui::Frame::new().fill(Color32::from_gray(255)).corner_radius(10.0).inner_margin(egui::Margin::symmetric(14, 6))
            .stroke(egui::Stroke::new(1.0_f32, Color32::from_gray(224))).show(ui, |ui| { ui.set_width(ui.available_width()); body(ui); });
    }

    fn rule(ui: &mut egui::Ui) { ui.add(egui::Separator::default().spacing(5.0)); }

    fn note(ui: &mut egui::Ui, text: &str) { ui.label(RichText::new(text).size(12.5).color(Color32::from_gray(110))); }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Search").desired_width(f32::INFINITY).margin(egui::Margin::symmetric(8, 5)));
        ui.add_space(8.0);
        let needle = self.filter.trim().to_lowercase();
        for page in Page::ALL.into_iter().filter(|p| needle.is_empty() || p.title().to_lowercase().contains(&needle)) {
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());
            let selected = self.page == page;
            if selected { ui.painter().rect_filled(rect, 8.0, ACCENT); }
            else if resp.hovered() { ui.painter().rect_filled(rect, 8.0, Color32::from_black_alpha(14)); }
            let (color, glyph) = page.icon();
            let icon = egui::Rect::from_center_size(rect.left_center() + Vec2::new(20.0, 0.0), Vec2::splat(22.0));
            ui.painter().rect_filled(icon, 6.0, color);
            ui.painter().text(icon.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(12.5), Color32::WHITE);
            ui.painter().text(rect.left_center() + Vec2::new(40.0, 0.0), Align2::LEFT_CENTER, page.title(), FontId::proportional(14.5),
                if selected { Color32::WHITE } else { Color32::from_gray(30) });
            if resp.clicked() { self.page = page; self.message.clear(); }
        }
    }

    fn page_results(&mut self, ui: &mut egui::Ui) {
        Self::note(ui, "Choose which kinds of files and apps appear when you search.");
        ui.add_space(6.0);
        let mut changed = false;
        Self::group(ui, |ui| {
            for (i, (label, _)) in CATEGORIES.iter().enumerate() {
                let mut shown = !self.hidden.contains(*label);
                if ui.checkbox(&mut shown, *label).changed() {
                    if shown { self.hidden.remove(*label); } else { self.hidden.insert(label.to_string()); }
                    changed = true;
                }
                if i + 1 < CATEGORIES.len() { Self::rule(ui); }
            }
        });
        if changed { self.save_hidden(); self.message = "Saved. The search window uses this next time it opens.".into(); }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Privacy…").clicked() { self.page = Page::Privacy; }
            if ui.button("Show everything").clicked() { self.hidden.clear(); self.save_hidden(); }
        });
    }

    fn page_privacy(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        Self::note(ui, "Results from these folders never appear in search. Their files stay in the index, only hidden from view.");
        ui.add_space(6.0);
        let mut remove: Option<usize> = None;
        Self::group(ui, |ui| {
            if self.private_folders.is_empty() { ui.label(RichText::new("No folders hidden.").color(Color32::from_gray(120))); }
            for (i, path) in self.private_folders.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(crate::bidi::visual(path));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| { if ui.button("−").clicked() { remove = Some(i); } });
                });
                if i + 1 < self.private_folders.len() { Self::rule(ui); }
            }
        });
        if let Some(i) = remove { self.private_folders.remove(i); self.save_private(); }
        ui.add_space(8.0);
        if ui.button("+  Add folder…").clicked() { self.pick_folder(ctx, Pick::Privacy); }
    }

    fn page_appearance(&mut self, ui: &mut egui::Ui) {
        Self::note(ui, "Choose how see-through the search window is. It applies the next time the search window opens.");
        ui.add_space(6.0);
        let before = self.look;
        Self::group(ui, |ui| {
            ui.radio_value(&mut self.look.mode, Transparency::Off, "Solid");
            Self::rule(ui);
            ui.radio_value(&mut self.look.mode, Transparency::Glass, "Translucent glass");
            Self::rule(ui);
            ui.radio_value(&mut self.look.mode, Transparency::Blur, "Frosted glass   (blurs what is behind the window)");
        });
        ui.add_space(8.0);
        Self::group(ui, |ui| {
            let enabled = self.look.mode != Transparency::Off;
            ui.add_enabled(enabled, egui::Slider::new(&mut self.look.level, 20..=100).suffix("%").text("Opacity"));
        });
        if self.look != before {
            if before.mode != Transparency::Blur && self.look.mode == Transparency::Blur && self.look.level > 80 { self.look.level = 55; }
            self.pending_look = Some(Instant::now());   // saved a moment after the last change, not on every slider step
            self.message = "Saved. Open the search window again to see it.".into();
        }
        ui.add_space(10.0);
        // preview: the bar over a colourful backdrop (blur itself cannot be shown here)
        let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 86.0), Sense::hover());
        let painter = ui.painter_at(r);
        painter.rect_filled(r, 10.0, Color32::from_rgb(60, 105, 215));
        for i in 0..10 {
            let w = r.width() / 10.0;
            let stripe = egui::Rect::from_min_size(r.min + Vec2::new(i as f32 * w, 0.0), Vec2::new(w * 0.5, r.height()));
            painter.rect_filled(stripe, 0.0, if i % 2 == 0 { Color32::from_rgb(255, 170, 90) } else { Color32::from_rgb(70, 205, 170) });
        }
        let pill = egui::Rect::from_center_size(r.center(), Vec2::new(r.width() * 0.72, 40.0));
        painter.rect_filled(pill, 20.0, Color32::from_rgba_unmultiplied(246, 247, 250, self.look.alpha()));
        painter.text(pill.left_center() + Vec2::new(18.0, 0.0), Align2::LEFT_CENTER, "Search files", FontId::proportional(16.0), Color32::from_gray(110));
        Self::note(ui, "Preview. Frosted glass also blurs what is behind the window, which cannot be shown here.");
        if self.look.mode == Transparency::Blur {
            ui.add_space(4.0);
            Self::note(ui, "A blurred snapshot of the screen behind the search window is taken the moment it opens. It stays in memory only and is never saved or sent anywhere.");
        }
    }

    fn page_shortcut(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        Self::note(ui, "Press this anywhere in Windows to open fsearch.");
        ui.add_space(6.0);
        Self::group(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Open fsearch with");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Reset").on_hover_text("Back to Ctrl + Shift + Space").clicked() { self.recording = false; self.set_hotkey(ctx, win::DEFAULT_HOTKEY.to_string()); }
                    let label = if self.recording { "Press the new shortcut…".to_string() } else { pretty_hotkey(&self.hotkey) };
                    let button = egui::Button::new(RichText::new(label).strong().color(if self.recording { Color32::WHITE } else { Color32::BLACK }))
                        .fill(if self.recording { ACCENT } else { Color32::from_gray(238) }).min_size(Vec2::new(190.0, 28.0));
                    if ui.add(button).clicked() { self.recording = !self.recording; self.message.clear(); }
                });
            });
        });
        ui.add_space(8.0);
        let (text, color) = match (&self.hotkey_status, self.running) {
            (_, false) => ("The background service is not running, so the shortcut is inactive (General > Start).".to_string(), Color32::from_gray(120)),
            (Some(s), _) if s.starts_with("ok") => ("● Active".to_string(), GREEN),
            (Some(s), _) if s.starts_with("taken") => ("● Another program already uses this shortcut. Choose a different one.".to_string(), RED),
            _ => ("Checking…".to_string(), Color32::from_gray(120)),
        };
        ui.label(RichText::new(text).color(color).size(13.5));
        if let Some(hint) = conflict_hint(&self.hotkey) { ui.label(RichText::new(hint).color(RED).size(12.5)); }
        Self::note(ui, "Click the shortcut, then press the keys you want (with Ctrl, Alt or Shift). Esc cancels.");
    }

    fn page_general(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        Self::group(ui, |ui| {
            let mut autostart = self.autostart;
            if ui.checkbox(&mut autostart, "Start fsearch automatically when I sign in").changed() {
                self.autostart = autostart;
                let ok = service::set_autostart(&self.db, autostart);
                self.message = match (ok, autostart) {
                    (true, true) => "Will start at sign-in.".into(),
                    (true, false) => "Will no longer start at sign-in (it keeps running until you stop it or sign out).".into(),
                    _ => "Could not change the start-up setting.".into(),
                };
                if autostart && !self.running { self.background(ctx, |db| match service::start_service(db) { Ok(()) => "Service started.".into(), Err(e) => format!("Could not start the service: {e}") }); }
            }
            Self::rule(ui);
            ui.horizontal(|ui| {
                ui.label("Background service");
                ui.label(RichText::new(if self.running { "● Running" } else { "● Stopped" }).color(if self.running { GREEN } else { RED }));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(self.running, egui::Button::new("Restart")).clicked() {
                        self.background(ctx, |db| match service::restart_service(db) { Ok(()) => "Service restarted.".into(), Err(e) => format!("Could not restart: {e}") });
                    }
                    if ui.add_enabled(self.running, egui::Button::new("Stop")).clicked() {
                        self.background(ctx, |db| { service::stop_service(db); "Service stopped.".into() });
                    }
                    if ui.add_enabled(!self.running, egui::Button::new("Start")).clicked() {
                        self.background(ctx, |db| match service::start_service(db) { Ok(()) => "Service started.".into(), Err(e) => format!("Could not start the service: {e}") });
                    }
                });
            });
            Self::rule(ui);
            ui.horizontal(|ui| {
                ui.label("Pin fsearch to the taskbar");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Show me").clicked() {
                        self.message = if service::reveal_start_menu_shortcut(&self.db) {
                            "Right-click the selected \"fsearch\" shortcut, choose \"Show more options\" then \"Pin to taskbar\". (Windows does not let programs pin themselves.)".into()
                        } else { "Could not find the fsearch shortcut. Run fsearch install first.".into() };
                    }
                });
            });
            Self::rule(ui);
            let mut blur = self.close_on_blur;
            if ui.checkbox(&mut blur, "Close the search window when I click elsewhere").changed() {
                self.close_on_blur = blur;
                self.write_db(move |st| { let _ = st.set_setting("close_on_blur", if blur { "1" } else { "0" }); });
            }
            Self::rule(ui);
            ui.horizontal(|ui| {
                ui.label("Results shown in the search window");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("rows").selected_text(self.max_rows.to_string()).show_ui(ui, |ui| {
                        for n in [5u32, 6, 8, 10] {
                            if ui.selectable_value(&mut self.max_rows, n, n.to_string()).changed() { self.write_db(move |st| { let _ = st.set_setting("max_rows", &n.to_string()); }); }
                        }
                    });
                });
            });
        });
    }

    fn page_folders(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        Self::note(ui, "fsearch keeps these folders indexed. Start-menu folders are what makes apps searchable.");
        ui.add_space(6.0);
        let mut remove: Option<String> = None;
        let mut content_change: Option<(String, bool)> = None;
        let roots = self.roots.clone();
        Self::group(ui, |ui| {
            if roots.is_empty() { ui.label(RichText::new("No folders yet.").color(Color32::from_gray(120))); }
            for (i, (path, content)) in roots.iter().enumerate() {
                ui.horizontal(|ui| {
                    let shown = if path.chars().count() > 46 { format!("…{}", path.chars().rev().take(44).collect::<Vec<_>>().into_iter().rev().collect::<String>()) } else { path.clone() };
                    ui.label(crate::bidi::visual(&shown)).on_hover_text(crate::bidi::visual(path));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Remove").clicked() { remove = Some(path.clone()); }
                        let mut c = *content;
                        if ui.checkbox(&mut c, "Search inside files").on_hover_text("Reads text, code and Word files. On OneDrive folders this can download online-only files.").changed() {
                            content_change = Some((path.clone(), c));
                        }
                    });
                });
                if i + 1 < roots.len() { Self::rule(ui); }
            }
        });
        if let Some((p, c)) = content_change { let q = p.clone(); self.write_db(move |st| { let _ = st.add_root(&q, c); }); self.message = "Saved. The service re-reads the folder shortly.".into(); }
        if let Some(p) = remove {
            { let q = p.clone(); self.write_db(move |st| { let _ = st.remove_root(&q); }); }
            self.message = format!("Removed {}. Clearing its files from the index…", crate::bidi::visual(&p));
            self.background(ctx, move |db| match Store::open(db).and_then(|s| s.delete_path(&p)) { Ok(n) => format!("Removed {n} files from the index."), Err(e) => format!("Could not clear the index: {e}") });
        }
        ui.add_space(8.0);
        if ui.button("+  Add folder…").clicked() { self.pick_folder(ctx, Pick::Index); }
    }

    fn page_index(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        Self::group(ui, |ui| {
            ui.label(format!("{} files indexed", self.stats.0));
            Self::rule(ui);
            ui.label(format!("{} searchable inside", self.stats.1));
            Self::rule(ui);
            ui.label(format!("{:.1} MB on disk", self.stats.2 as f64 / 1e6));
        });
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Refresh Store apps").clicked() {
                self.background(ctx, |db| match Store::open(db) { Ok(s) => format!("{} Microsoft Store apps indexed.", service::sync_store_apps(&s)), Err(e) => e.to_string() });
            }
            let label = if self.confirm_rebuild { "Click again to rebuild" } else { "Rebuild index…" };
            if ui.add(egui::Button::new(label).fill(if self.confirm_rebuild { Color32::from_rgb(255, 220, 215) } else { Color32::from_gray(255) })).clicked() {
                if self.confirm_rebuild {
                    self.confirm_rebuild = false;
                    self.background(ctx, |db| {
                        let was_running = service::is_running(db);
                        service::stop_service(db);
                        if let Ok(s) = Store::open(db) { let _ = s.clear_files(); let _ = s.set_setting("apps_synced_at", "0"); }
                        if was_running { let _ = service::start_service(db); }
                        "Index cleared; the service is re-indexing your folders.".into()
                    });
                } else { self.confirm_rebuild = true; }
            }
            if ui.button("Open log").clicked() { let _ = std::process::Command::new("explorer.exe").arg(service::log_path(&self.db)).spawn(); }
            if ui.button("Open data folder").clicked() { let _ = std::process::Command::new("explorer.exe").arg(service::data_dir(&self.db)).spawn(); }
        });
        ui.add_space(6.0);
        Self::note(ui, &format!("Index file: {}", self.db.display()));
        Self::note(ui, &format!("fsearch {}", crate::build_label()));
    }
}

impl eframe::App for App {
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        if self.placer.ready() { visuals.panel_fill.to_normalized_gamma_f32() } else { [0.0, 0.0, 0.0, 0.0] }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.placer.step(ctx, Vec2::new(DESIGN_W, DESIGN_H), 0.46, (0.8, 1.6), DESIGN_H / 2.0);
        if !self.placer.ready() { return; }                      // transparent until placed in the screen centre
        while let Ok(done) = self.rx.try_recv() {
            match done {
                Done::Refresh => { self.refresh(true); self.refresh_stats(); }
                Done::Stats(st) => self.stats = st,
                Done::Message(m) => self.message = m,
                Done::Folder(Pick::Index, Some(path)) => {
                    let p = path.to_string_lossy().into_owned();
                    { let q = p.clone(); self.write_db(move |st| { let _ = st.add_root(&q, false); }); }
                    self.message = format!("Added {p}. The service indexes it within a few seconds.");
                    self.refresh(false);
                }
                Done::Folder(Pick::Privacy, Some(path)) => {
                    let p = path.to_string_lossy().into_owned();
                    if !self.private_folders.contains(&p) { self.private_folders.push(p.clone()); self.save_private(); }
                    self.message = format!("{p} is now hidden from results.");
                }
                Done::Folder(_, None) => {}
            }
        }
        if let Some(t) = self.pending_look {
            if t.elapsed() > Duration::from_millis(350) { self.pending_look = None; let look = self.look; self.write_db(move |st| look.save(st)); }
            else { ctx.request_repaint_after(Duration::from_millis(400)); }
        }
        if self.polled.elapsed() > Duration::from_secs(2) { self.polled = Instant::now(); self.refresh(true); }
        if self.stats_polled.elapsed() > Duration::from_secs(15) { self.refresh_stats(); }
        ctx.request_repaint_after(Duration::from_secs(2));
        if self.recording { self.record_keys(ctx); }

        egui::SidePanel::left("sidebar").exact_width(214.0).resizable(false)
            .frame(egui::Frame::new().fill(Color32::from_rgb(233, 234, 238)).inner_margin(egui::Margin::same(12)))
            .show(ctx, |ui| self.sidebar(ui));

        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::from_rgb(246, 246, 248)).inner_margin(egui::Margin::symmetric(24, 16))).show(ctx, |ui| {
            ui.label(RichText::new(self.page.title()).size(22.0).strong());
            ui.add_space(8.0);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                match self.page {
                    Page::Results => self.page_results(ui),
                    Page::Privacy => self.page_privacy(ui, ctx),
                    Page::Appearance => self.page_appearance(ui),
                    Page::Shortcut => self.page_shortcut(ui, ctx),
                    Page::General => self.page_general(ui, ctx),
                    Page::Folders => self.page_folders(ui, ctx),
                    Page::Index => self.page_index(ui, ctx),
                }
                if !self.message.is_empty() { ui.add_space(10.0); ui.label(RichText::new(&self.message).color(ACCENT)); }
                ui.add_space(14.0);
            });
        });
    }
}

pub fn run() -> eframe::Result {
    if !win::single_instance("Local\\fsearch-settings") { return Ok(()); }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([DESIGN_W, DESIGN_H]).with_min_inner_size([520.0, 400.0])
            .with_title("fsearch Settings").with_transparent(true)
            .with_icon(egui::IconData { rgba: include_bytes!("../assets/icon-64.rgba").to_vec(), width: 64, height: 64 }).with_position([-30000.0, -30000.0]),
        ..Default::default()
    };
    eframe::run_native("fsearch Settings", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_names_match_the_hotkey_parser() {
        for (key, expected) in [(Key::Space, "space"), (Key::F, "f"), (Key::Num7, "7"), (Key::F5, "f5"), (Key::F12, "f12")] {
            let name = key_name(key).unwrap();
            assert_eq!(name, expected);
            assert!(win::parse_hotkey(&format!("ctrl+{name}")).is_some());
        }
        assert!(key_name(Key::Enter).is_none());
        assert!(key_name(Key::ArrowUp).is_none());
    }
    #[test]
    fn pretty_names() { assert_eq!(pretty_hotkey("ctrl+shift+space"), "Ctrl + Shift + Space"); assert_eq!(pretty_hotkey("alt+f"), "Alt + F"); }
}
