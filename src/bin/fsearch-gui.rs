//! Spotlight-style launcher window: type, see results instantly, Enter to open.
#![cfg_attr(windows, windows_subsystem = "windows")]

use eframe::egui::{self, Align2, Color32, FontId, Key, Modifiers, Pos2, Rect, Sense, Stroke, Vec2};
use fsearch::categories::exclusions;
use fsearch::look::{Look, Transparency};
use fsearch::placer::Placer;
use fsearch::query::Query;
use fsearch::rank::{group_hits, rank_hits};
use std::collections::HashMap;
use fsearch::store::{default_db_path, Hit, Store};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const WIDTH: f32 = 780.0;
const ROW: f32 = 54.0;
const HEAD: f32 = 50.0;                 // height of the search row / diameter of the round buttons
const PAD: f32 = 26.0;                  // transparent margin around everything, room for the shadows
const ACCENT: Color32 = Color32::from_rgb(10, 132, 255);

#[derive(Clone, Copy, PartialEq)]
enum Chip { All, Apps, Docs, Images, Code }

impl Chip {
    const ALL: [Chip; 5] = [Chip::All, Chip::Apps, Chip::Docs, Chip::Images, Chip::Code];
    fn filter(self) -> &'static str {
        match self {
            Chip::All => "",
            Chip::Apps => " ext:exe,lnk,msi,uwp",
            Chip::Docs => " ext:pdf,doc,docx,xls,xlsx,ppt,pptx,txt,md,rtf,csv",
            Chip::Images => " kind:image",
            Chip::Code => " kind:source",
        }
    }
    fn label(self) -> &'static str {
        match self { Chip::All => "All", Chip::Apps => "Apps", Chip::Docs => "Documents", Chip::Images => "Images", Chip::Code => "Code" }
    }
}

enum Job { Search(u64, String), Opened(String) }

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// "ms word" -> "word": people say "MS Word", the shortcut is just called "Word".
fn normalize_query(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.iter().filter(|w| !w.contains(':')).count() < 2 { return text.trim().to_string(); }
    words.into_iter().filter(|w| w.contains(':') || !matches!(w.to_lowercase().as_str(), "ms" | "microsoft")).collect::<Vec<_>>().join(" ")
}

fn name_job(segments: &[(String, bool)], font: FontId, base: Color32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    for (text, matched) in segments {
        let color = if *matched { ACCENT } else { base };
        job.append(text, 0.0, egui::text::TextFormat { font_id: font.clone(), color, ..Default::default() });
    }
    job
}

/// Short kind label for the right edge of a row.
fn short_kind(kind: &str) -> &str {
    match kind { "PDF Document" => "PDF", "Word Document" => "Word", "Spreadsheet" => "Excel", "Presentation" => "Slides", "Source Code" => "Code", "Document" => "File", k => k }
}

fn is_own_icon(ext: &str) -> bool { matches!(ext, "exe" | "lnk" | "msi" | "ico" | "url") }

/// The Windows shell icon for a file, cached per type (or per file for programs/shortcuts).
fn texture_for<'a>(cache: &'a mut HashMap<String, Option<egui::TextureHandle>>, ctx: &egui::Context, path: &str, ext: &str) -> Option<&'a egui::TextureHandle> {
    if cache.len() > 300 { cache.clear(); }
    let own = is_own_icon(ext);
    let key = if own { path.to_string() } else { format!("ext:{ext}") };
    cache.entry(key.clone()).or_insert_with(|| {
        let (w, h, px) = fsearch::icons::icon_rgba(path, !own)?;
        Some(ctx.load_texture(key, egui::ColorImage::from_rgba_unmultiplied([w, h], &px), egui::TextureOptions::LINEAR))
    }).as_ref()
}

#[derive(Clone, Copy)]
enum Action { Open, RunAsAdmin, Reveal, OpenWith, CopyFile, CopyPath, CopyName, OpenTerminal }

/// The right-click menu of a result.
fn context_menu_items(ui: &mut egui::Ui, ext: &str, store_app: bool) -> Option<Action> {
    ui.set_min_width(240.0);
    let mut chosen = None;
    let mut item = |ui: &mut egui::Ui, label: &str, hint: &str, action: Action| {
        let button = egui::Button::new(label).frame(false).min_size(Vec2::new(228.0, 26.0)).shortcut_text(hint);
        if ui.add(button).clicked() { chosen = Some(action); ui.close_menu(); }
    };
    item(ui, "Open", "Enter", Action::Open);
    if !store_app {
        if matches!(ext, "exe" | "lnk" | "msi" | "bat" | "cmd" | "ps1") { item(ui, "Run as administrator", "", Action::RunAsAdmin); }
        item(ui, "Show in folder", "Ctrl+Enter", Action::Reveal);
        item(ui, "Open with…", "", Action::OpenWith);
        ui.separator();
        item(ui, "Copy file", "", Action::CopyFile);
        item(ui, "Copy path", "", Action::CopyPath);
    }
    item(ui, "Copy name", "", Action::CopyName);
    if !store_app {
        ui.separator();
        item(ui, "Open folder in Terminal", "", Action::OpenTerminal);
    }
    chosen
}

/// Runs a menu action. The window closes right away, or after a moment for copies so the clipboard is set first.
fn run_action(action: Action, path: &str, name: &str, ctx: &egui::Context, to_worker: &Sender<Job>, close_at: &mut Option<Instant>) {
    let now = Instant::now();
    match action {
        Action::Open => { launch(to_worker, path); *close_at = Some(now); }
        Action::RunAsAdmin => { fsearch::win::run_as_admin(path); *close_at = Some(now); }
        Action::Reveal => { reveal_path(path); *close_at = Some(now); }
        Action::OpenWith => { fsearch::win::open_with(path); *close_at = Some(now); }
        Action::CopyFile => { fsearch::win::copy_file_to_clipboard(path); *close_at = Some(now + Duration::from_millis(400)); }
        Action::CopyPath => { ctx.copy_text(path.to_string()); *close_at = Some(now + Duration::from_millis(300)); }
        Action::CopyName => { ctx.copy_text(name.to_string()); *close_at = Some(now + Duration::from_millis(300)); }
        Action::OpenTerminal => {
            if let Some(dir) = Path::new(path).parent() { fsearch::win::open_terminal(&dir.to_string_lossy()); }
            *close_at = Some(now);
        }
    }
}

fn launch(to_worker: &Sender<Job>, path: &str) {
    open_path(path);
    let _ = to_worker.send(Job::Opened(path.to_string()));
}

struct Results { generation: u64, hits: Vec<Hit>, total: usize, capped: bool, millis: u128 }

struct App {
    text: String,
    chip: Chip,
    selected: usize,
    hits: Vec<Hit>,
    total: usize,
    capped: bool,
    millis: u128,
    asked: u64,
    answered: u64,
    to_worker: Sender<Job>,
    icons: HashMap<String, Option<egui::TextureHandle>>,
    from_worker: Receiver<Results>,
    index_files: i64,
    index_changed_at: Option<Instant>,
    stats: Receiver<i64>,
    placer: Placer,
    close_at: Option<Instant>,
    look: Look,
    /// Blurred snapshot of what was behind the window when it opened (frosted mode only).
    frost: Option<Frost>,
    frost_tried: bool,
    glass: HashMap<String, GlassTex>,
    shown_at: Option<Instant>,
    exclusions: String,
    max_rows: usize,
    close_on_blur: bool,
    was_focused: bool,
    /// The window stays running while closed (parked off-screen) so opening it again is instant.
    hidden: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        fsearch::settings_ui::install_fonts(&cc.egui_ctx);
        let (to_worker, worker_rx) = channel::<Job>();
        let (results_tx, from_worker) = channel::<Results>();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            let Ok(store) = Store::open(&default_db_path()) else { return };
            let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_default();
            while let Ok(first) = worker_rx.recv() {
                let mut jobs = vec![first];
                while let Ok(next) = worker_rx.try_recv() { jobs.push(next); }
                let mut latest: Option<(u64, String)> = None;       // only the newest search matters
                for job in jobs {
                    match job {
                        Job::Opened(path) => { let _ = store.record_open(&path, now_ms()); }
                        Job::Search(generation, text) => latest = Some((generation, text)),
                    }
                }
                let Some((generation, text)) = latest else { continue };
                let started = Instant::now();
                let q = Query::parse(&text);
                // One or two letters cannot use the name index and match a huge part of the disk: look at fewer candidates.
                let short = q.terms.iter().chain(q.names.iter()).all(|t| t.chars().count() < 3);
                let cap = if short { 800 } else { 2500 };
                let mut hits = store.search(&q, cap).unwrap_or_default();
                let (total, capped) = (hits.len(), hits.len() >= cap);     // "N+" when capped; counting every match would scan the whole index a second time
                // Old shortcuts would otherwise fall outside the newest-first candidates, so ask for apps separately.
                if q.kinds.is_empty() {
                    if let Ok(apps) = store.search(&Query::parse(&format!("{text} kind:\"Application\"")), 40) {
                        for a in apps { if !hits.iter().any(|h| h.path == a.path) { hits.push(a); } }
                    }
                }
                let needles: Vec<String> = q.terms.iter().chain(q.names.iter()).map(|t| t.to_lowercase()).collect();
                let paths: Vec<String> = hits.iter().map(|h| h.path.clone()).collect();
                let launches = store.launches(&paths).unwrap_or_default();
                rank_hits(&mut hits, &needles, &home, &launches, now_ms());
                if q.kinds.is_empty() { group_hits(&mut hits); }
                hits.truncate(60);
                let _ = results_tx.send(Results { generation, hits, total, capped, millis: started.elapsed().as_millis() });
                ctx.request_repaint();
            }
        });

        // Another start of the program (the hotkey) signals this event instead of opening a second window.
        let event = LAUNCHER_EVENT.load(std::sync::atomic::Ordering::SeqCst);
        if event != 0 {
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                while fsearch::win::wait_launcher_event(event) {
                    if VISIBLE.load(std::sync::atomic::Ordering::SeqCst) {
                        CLOSE_REQ.store(true, std::sync::atomic::Ordering::SeqCst);        // pressing the hotkey again closes it
                    } else {
                        // Photograph the screen now, while no window of ours is on it, then ask the window to open.
                        if Look::load_default().mode == Transparency::Blur { take_snapshot(); }
                        SHOW.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    ctx.request_repaint();
                }
            });
        }

        let (stats_tx, stats) = channel::<i64>();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            let Ok(store) = Store::open(&default_db_path()) else { return };
            let mut last = -1;
            loop {
                if let Ok(s) = store.stats() {
                    if s.files != last {
                        last = s.files;
                        if stats_tx.send(s.files).is_err() { return; }
                        ctx.request_repaint();      // only when it changed: a hidden window must not wake up every second
                    }
                }
                std::thread::sleep(Duration::from_millis(700));
            }
        });

        let (max_rows, close_on_blur, exclusions) = Store::open(&default_db_path()).map(|st| {
            let hidden: Vec<String> = st.setting("hidden_categories").ok().flatten().unwrap_or_default().split(',').filter(|s| !s.is_empty()).map(String::from).collect();
            let private: Vec<String> = st.setting("privacy_folders").ok().flatten().unwrap_or_default().lines().map(String::from).collect();
            (st.setting("max_rows").ok().flatten().and_then(|v| v.parse().ok()).unwrap_or(8usize).clamp(3, 12),
             st.setting("close_on_blur").ok().flatten().map(|v| v != "0").unwrap_or(true),
             exclusions(&hidden, &private))
        }).unwrap_or((8, true, String::new()));
        let demo = std::env::var("FSEARCH_GUI_DEMO_QUERY").unwrap_or_default();   // for screenshots/tests
        let mut app = Self { text: demo, chip: Chip::All, selected: 0, hits: vec![], total: 0, capped: false, millis: 0, asked: 0, answered: 0,
               to_worker, icons: HashMap::new(), from_worker, index_files: -1, index_changed_at: None, stats, placer: Placer::new(), close_at: None, look: Look::load_default(), frost: None, frost_tried: false, glass: HashMap::new(), shown_at: None, exclusions, max_rows, close_on_blur, was_focused: false, hidden: std::env::args().any(|a| a == "--background") };
        if !app.text.is_empty() { app.search(); }
        app
    }

    fn search(&mut self) {
        self.asked += 1;
        let q = self.text.trim();
        if q.is_empty() && self.chip == Chip::All {
            self.hits.clear(); self.total = 0; self.answered = self.asked; return;
        }
        let _ = self.to_worker.send(Job::Search(self.asked, format!("{}{}{}", normalize_query(q), self.chip.filter(), self.exclusions)));
    }

    fn indexing(&self) -> bool {
        self.index_changed_at.map(|t| t.elapsed() < Duration::from_secs(3)).unwrap_or(false)
    }
}

fn open_path(path: &str) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

fn reveal_path(path: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{path}\"")).spawn();
    }
    #[cfg(not(windows))]
    if let Some(parent) = Path::new(path).parent() { let _ = std::process::Command::new("xdg-open").arg(parent).spawn(); }
}

fn age(mtime_ms: i64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let d = (now - mtime_ms / 1000).max(0);
    match d { 0..=3599 => format!("{}m", (d / 60).max(1)), 3600..=86399 => format!("{}h", d / 3600), 86400..=2591999 => format!("{}d", d / 86400), _ => format!("{}mo", d / 2592000) }
}

fn size_text(n: i64) -> String {
    let n = n as f64;
    if n >= 1e9 { format!("{:.1} GB", n / 1e9) } else if n >= 1e6 { format!("{:.1} MB", n / 1e6) } else if n >= 1e3 { format!("{:.0} KB", n / 1e3) } else { format!("{n:.0} B") }
}

fn elide_middle(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max { return s.to_string(); }
    let keep = (max - 1) / 2;
    format!("{}…{}", chars[..keep].iter().collect::<String>(), chars[chars.len() - keep..].iter().collect::<String>())
}

fn ext_color(ext: &str) -> Color32 {
    match ext {
        "pdf" => Color32::from_rgb(226, 62, 52),
        "doc" | "docx" | "rtf" => Color32::from_rgb(43, 87, 190),
        "xls" | "xlsx" | "csv" => Color32::from_rgb(30, 130, 76),
        "ppt" | "pptx" => Color32::from_rgb(214, 90, 40),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" => Color32::from_rgb(150, 80, 200),
        "exe" | "lnk" | "msi" => Color32::from_rgb(20, 130, 230),
        "zip" | "7z" | "rar" => Color32::from_rgb(190, 140, 30),
        _ => Color32::from_rgb(110, 120, 135),
    }
}

/// Magnifier, then the editable query text inside `row`. Returns true when the text changed.
/// Arabic: the edit box still receives the typing, but it is drawn invisibly and the joined, right-to-left
/// text is painted over it (egui has no Arabic shaping), with its own caret.
fn search_field(ui: &mut egui::Ui, ctx: &egui::Context, text: &mut String, row: Rect, was_focused: bool) -> bool {
    draw_chip_icon(ui.painter(), Chip::All, row.left_center() + Vec2::new(28.0, 0.0), Color32::from_gray(105));
    let area = Rect::from_min_max(row.min + Vec2::new(54.0, 3.0), row.max - Vec2::new(84.0, 3.0));
    let rtl = fsearch::bidi::has_rtl(text);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
    if rtl { child.visuals_mut().text_cursor.stroke = Stroke::NONE; }
    let edit = child.put(area, egui::TextEdit::singleline(text).id(egui::Id::new("query")).frame(false)
        .hint_text(egui::RichText::new("Search files").color(Color32::from_gray(150)))
        .font(FontId::proportional(24.0)).text_color(if rtl { Color32::TRANSPARENT } else { Color32::from_gray(25) })
        .vertical_align(egui::Align::Center));
    if !was_focused || ctx.memory(|m| m.focused().is_none()) { edit.request_focus(); }
    if rtl && !text.is_empty() {
        let galley = ui.painter().layout_no_wrap(fsearch::bidi::visual(text), FontId::proportional(24.0), Color32::from_gray(25));
        let pos = Pos2::new((area.right() - galley.size().x).max(area.left()), area.center().y - galley.size().y / 2.0);
        let (left, h) = (pos.x, galley.size().y);
        ui.painter().galley(pos, galley, Color32::from_gray(25));
        if edit.has_focus() {
            let t = ctx.input(|i| i.time);
            if (t * 1.8).fract() < 0.6 {
                ui.painter().line_segment([Pos2::new(left - 2.0, pos.y + 3.0), Pos2::new(left - 2.0, pos.y + h - 3.0)], Stroke::new(1.6_f32, ACCENT));
            }
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
    let mut changed = edit.changed();
    if !text.is_empty() {
        let c = row.right_center() - Vec2::new(58.0, 0.0);
        let r = Rect::from_center_size(c, Vec2::splat(26.0));
        let resp = ui.interact(r, ui.id().with("clear"), Sense::click());
        let fill = if resp.hovered() { 150 } else { 185 };
        ui.painter().circle_filled(c, 8.5, Color32::from_gray(fill));
        let k = 3.0;
        let st = Stroke::new(1.6_f32, Color32::WHITE);
        ui.painter().line_segment([c + Vec2::new(-k, -k), c + Vec2::new(k, k)], st);
        ui.painter().line_segment([c + Vec2::new(-k, k), c + Vec2::new(k, -k)], st);
        if resp.clicked() { text.clear(); changed = true; edit.request_focus(); }
    }
    changed
}

fn gear_button(ui: &mut egui::Ui, center: Pos2, radius: f32) -> bool {
    let r = Rect::from_center_size(center, Vec2::splat(radius * 2.4));
    let resp = ui.interact(r, ui.id().with("gear"), Sense::click());
    if resp.hovered() { ui.painter().circle_filled(center, radius * 1.3, Color32::from_black_alpha(14)); }
    draw_gear(ui.painter(), center, if resp.hovered() { Color32::from_gray(60) } else { Color32::from_gray(125) });
    resp.on_hover_text("Settings  (Ctrl+,)").clicked()
}

fn rule(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, Color32::from_black_alpha(20));
}

/// A rounded surface: the frosted snapshot of what is behind (when there is one) under a translucent tint.
/// The snapshot is drawn as a plain triangle fan over the rounded outline. (egui's textured rounded-rectangle shape left
/// thin seams across the circles.) It is inset by a pixel so its hard edge hides under the tint's smooth edge.
/// A glass surface drawn on the CPU into one bitmap: smooth edge, soft shadow, thin bright rim, frosted picture and tint.
/// egui would split such a shape into a fan of triangles, and on some graphics cards the shared edges show as hairlines
/// (diameters and chords across the round buttons). A single textured rectangle has none.
struct GlassTex { tex: egui::TextureHandle, pad: f32 }

fn rounded_sdf(px: f32, py: f32, w: f32, h: f32, r: f32) -> f32 {
    let qx = (px - w / 2.0).abs() - (w / 2.0 - r);
    let qy = (py - h / 2.0).abs() - (h / 2.0 - r);
    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r
}

fn sample_frost(frost: &Frost, x: f32, y: f32) -> [f32; 3] {
    let (sw, sh) = frost.small;
    let fx = (x / frost.screen.x * sw as f32 - 0.5).clamp(0.0, sw as f32 - 1.001);
    let fy = (y / frost.screen.y * sh as f32 - 0.5).clamp(0.0, sh as f32 - 1.001);
    let (x0, y0) = (fx as usize, fy as usize);
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let at = |x: usize, y: usize, c: usize| frost.data[(y * sw + x) * 4 + c] as f32;
    let mut out = [0.0; 3];
    for c in 0..3 {
        let top = at(x0, y0, c) * (1.0 - tx) + at(x0 + 1, y0, c) * tx;
        let bottom = at(x0, y0 + 1, c) * (1.0 - tx) + at(x0 + 1, y0 + 1, c) * tx;
        out[c] = top * (1.0 - ty) + bottom * ty;
    }
    out
}

/// `rect` is in window points; `tint` is straight (unmultiplied) RGBA.
fn make_glass(ctx: &egui::Context, name: &str, rect: Rect, radius_pts: f32, tint: [u8; 4], frost: Option<&Frost>, ppp: f32) -> GlassTex {
    let pad = (24.0 * ppp).round().max(1.0);
    let (w, h) = ((rect.width() * ppp).round(), (rect.height() * ppp).round());
    let r = (radius_pts * ppp).min(h / 2.0).min(w / 2.0);
    let (iw, ih) = ((w + 2.0 * pad) as usize, (h + 2.0 * pad) as usize);
    let sigma = 9.0 * ppp;
    let ta = tint[3] as f32 / 255.0;
    let mut px = vec![0u8; iw * ih * 4];
    for iy in 0..ih {
        for ix in 0..iw {
            let (x, y) = (ix as f32 + 0.5 - pad, iy as f32 + 0.5 - pad);          // pixels relative to the shape's top-left
            let d = rounded_sdf(x, y, w, h, r);
            let cover = (0.5 - d).clamp(0.0, 1.0);
            let shadow = 0.10 * (-0.5 * (rounded_sdf(x, y - 3.0 * ppp, w, h, r).max(0.0) / sigma).powi(2)).exp();
            let mut rgb = [tint[0] as f32, tint[1] as f32, tint[2] as f32];
            let mut alpha = ta;
            if let Some(f) = frost {
                let base = sample_frost(f, f.origin.x + rect.min.x * ppp + x, f.origin.y + rect.min.y * ppp + y);
                for c in 0..3 { rgb[c] = base[c] * (1.0 - ta) + rgb[c] * ta; }
                alpha = 1.0;                                                         // what is behind is already in the picture
            }
            let rim = if d < 0.0 { (1.0 - (-d / 1.6)).clamp(0.0, 1.0) * 0.45 } else { 0.0 };
            for c in 0..3 { rgb[c] = rgb[c] * (1.0 - rim) + 255.0 * rim; }
            let src_a = alpha * cover;
            let out_a = src_a + shadow * (1.0 - src_a);
            let i = (iy * iw + ix) * 4;
            if out_a > 0.0 {
                for c in 0..3 { px[i + c] = (rgb[c] * src_a / out_a).clamp(0.0, 255.0) as u8; }
            }
            px[i + 3] = (out_a * 255.0).round() as u8;
        }
    }
    let tex = ctx.load_texture(name, egui::ColorImage::from_rgba_unmultiplied([iw, ih], &px), egui::TextureOptions::LINEAR);
    GlassTex { tex, pad: pad / ppp }
}

fn show_glass(painter: &egui::Painter, glass: &GlassTex, rect: Rect) {
    painter.image(glass.tex.id(), rect.expand(glass.pad), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
}

/// Mean red, green, blue of an RGBA picture.
fn mean_rgb(rgba: &[u8]) -> (u32, u32, u32) {
    let n = (rgba.len() / 4).max(1) as u64;
    let sum = rgba.chunks_exact(4).fold([0u64; 3], |a, p| [a[0] + p[0] as u64, a[1] + p[1] as u64, a[2] + p[2] as u64]);
    ((sum[0] / n) as u32, (sum[1] / n) as u32, (sum[2] / n) as u32)
}

/// The frosted picture of the screen and how window points map onto it.
#[derive(Clone)]
struct Frost { tex: egui::TextureHandle, origin: Vec2, ppp: f32, screen: Vec2, small: (usize, usize), data: std::sync::Arc<Vec<u8>> }

impl Frost {
    /// Texture coordinate for a point in window coordinates.
    fn uv(&self, p: Pos2) -> Pos2 {
        Pos2::new((self.origin.x + p.x * self.ppp) / self.screen.x, (self.origin.y + p.y * self.ppp) / self.screen.y)
    }
}

struct Snapshot { monitor: fsearch::win::MonitorRect, width: usize, height: usize, rgba: Vec<u8>, mean: (u32, u32, u32), usable: bool }

static SNAPSHOT: std::sync::Mutex<Option<Snapshot>> = std::sync::Mutex::new(None);
static SHOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static CLOSE_REQ: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static VISIBLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static LAUNCHER_EVENT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Photographs the whole monitor under the pointer and frosts it. Done before any window exists, so the picture can
/// never contain our own (still empty) window, which is what made an earlier attempt come out black.
fn take_snapshot() {
    let snap = fsearch::win::cursor_monitor().and_then(|m| {
        let (w, h, rgba) = fsearch::win::capture_screen(m.left, m.top, m.width(), m.height())?;
        let usable = snapshot_is_usable(&rgba);
        let (sw, sh, small) = if usable { fsearch::blur::frosted(&rgba, w, h) } else { (0, 0, Vec::new()) };
        Some(Snapshot { monitor: m, width: sw, height: sh, rgba: small, mean: mean_rgb(&rgba), usable })
    });
    if let Ok(mut slot) = SNAPSHOT.lock() { *slot = snap; }
}

/// Turns the snapshot into a texture, mapped to where the window actually is.
fn frost_from_snapshot(ctx: &egui::Context, placer: &Placer) -> Result<Frost, String> {
    let guard = SNAPSHOT.lock().map_err(|_| "snapshot lock poisoned".to_string())?;
    let snap = guard.as_ref().ok_or("no snapshot was taken (pointer monitor unknown)")?;
    if !snap.usable { return Err(format!("the snapshot is black (mean colour {},{},{}), not used", snap.mean.0, snap.mean.1, snap.mean.2)); }
    let (x, y, _, _) = placer.window_rect_px().ok_or("the window position is unknown")?;
    if placer.monitor() != Some(snap.monitor) { return Err("the window opened on a different screen than the snapshot".to_string()); }
    let tex = ctx.load_texture("frost", egui::ColorImage::from_rgba_unmultiplied([snap.width, snap.height], &snap.rgba), egui::TextureOptions::LINEAR);
    Ok(Frost { tex, origin: Vec2::new((x - snap.monitor.left) as f32, (y - snap.monitor.top) as f32), ppp: ctx.pixels_per_point(),
               screen: Vec2::new(snap.monitor.width() as f32, snap.monitor.height() as f32),
               small: (snap.width, snap.height), data: std::sync::Arc::new(snap.rgba.clone()) })
}

/// A snapshot that is (nearly) black means the capture failed (the window system handed back its own empty window).
fn snapshot_is_usable(rgba: &[u8]) -> bool {
    let (r, g, b) = mean_rgb(rgba);
    r + g + b > 24
}

fn is_circle(rect: Rect, radius: f32) -> bool {
    (rect.width() - rect.height()).abs() < 0.5 && radius * 2.0 >= rect.width() - 0.5
}

/// A soft shadow made of a few stacked translucent shapes. Circles use circle shapes: egui fills a rounded rectangle whose
/// radius is half its size with long thin triangles, whose shared edges showed as hairlines under translucent colours.
fn soft_shadow_with(painter: &egui::Painter, rect: Rect, radius: f32, allow_circle: bool) {
    const LAYERS: usize = 12;
    for i in 0..LAYERS {
        let grow = i as f32 * 2.2;
        let color = Color32::from_black_alpha((5.0 - i as f32 * 0.4).max(1.0) as u8);
        if allow_circle && is_circle(rect, radius) {
            painter.circle_filled(rect.center() + Vec2::new(0.0, 4.0), radius + grow, color);
        } else {
            painter.rect_filled(rect.translate(Vec2::new(0.0, 4.0)).expand(grow), radius + grow, color);
        }
    }
}

fn soft_shadow(painter: &egui::Painter, rect: Rect, radius: f32) { soft_shadow_with(painter, rect, radius, true); }

fn fill_with(painter: &egui::Painter, rect: Rect, radius: f32, color: Color32, allow_circle: bool) {
    if allow_circle && is_circle(rect, radius) { painter.circle_filled(rect.center(), radius, color); }
    else { painter.rect_filled(rect, radius, color); }
}

fn paint_glass(painter: &egui::Painter, rect: Rect, radius: f32, tint: Color32, frost: Option<&Frost>) {
    paint_glass_with(painter, rect, radius, tint, frost, true);
}

fn paint_glass_with(painter: &egui::Painter, rect: Rect, radius: f32, tint: Color32, frost: Option<&Frost>, allow_circle: bool) {
    if let Some(frost) = frost {
        let inner = rect.shrink(0.8);
        let r = (radius - 0.8).clamp(0.0, inner.width().min(inner.height()) / 2.0);
        let uv = |p: Pos2| frost.uv(p);
        let mut mesh = egui::Mesh::with_texture(frost.tex.id());
        let vertex = |p: Pos2| egui::epaint::Vertex { pos: p, uv: uv(p), color: Color32::WHITE };
        mesh.vertices.push(vertex(inner.center()));
        const STEPS: usize = 12;
        let corners = [
            (Pos2::new(inner.right() - r, inner.top() + r), -90.0_f32),
            (Pos2::new(inner.right() - r, inner.bottom() - r), 0.0),
            (Pos2::new(inner.left() + r, inner.bottom() - r), 90.0),
            (Pos2::new(inner.left() + r, inner.top() + r), 180.0),
        ];
        for (centre, start) in corners {
            for i in 0..=STEPS {
                let a = (start + 90.0 * i as f32 / STEPS as f32).to_radians();
                mesh.vertices.push(vertex(Pos2::new(centre.x + r * a.cos(), centre.y + r * a.sin())));
            }
        }
        let n = mesh.vertices.len() as u32 - 1;                              // perimeter points
        for i in 1..=n { mesh.indices.extend_from_slice(&[0, i, if i == n { 1 } else { i + 1 }]); }
        painter.add(egui::Shape::mesh(mesh));
    }
    fill_with(painter, rect, radius, tint, allow_circle);
}

fn draw_gear(p: &egui::Painter, c: Pos2, col: Color32) {
    let s = Stroke::new(1.8_f32, col);
    p.circle_stroke(c, 6.0, s);
    p.circle_stroke(c, 2.0, s);
    for k in 0..8 {
        let a = k as f32 * std::f32::consts::FRAC_PI_4;
        let d = Vec2::new(a.cos(), a.sin());
        p.line_segment([c + d * 8.0, c + d * 10.5], Stroke::new(2.4_f32, col));
    }
}

/// The settings window is the same program started with --settings.
/// Asks the launcher to close (hide) at the end of this frame.
fn request_close(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("close_request"), true));
    ctx.request_repaint();
}

fn open_settings() {
    if let Ok(me) = std::env::current_exe() { let _ = std::process::Command::new(me).arg("--settings").spawn(); }
}

/// Line icons in the SF Symbols manner: one weight, round ends, drawn on a 20-point grid around `c`.
fn draw_chip_icon(p: &egui::Painter, chip: Chip, c: Pos2, col: Color32) {
    let w = 1.7_f32;
    let s = Stroke::new(w, col);
    let line = |a: (f32, f32), b: (f32, f32)| {
        p.line_segment([c + Vec2::new(a.0, a.1), c + Vec2::new(b.0, b.1)], s);
        p.circle_filled(c + Vec2::new(a.0, a.1), w / 2.0, col);
        p.circle_filled(c + Vec2::new(b.0, b.1), w / 2.0, col);
    };
    match chip {
        // magnifyingglass
        Chip::All => { p.circle_stroke(c + Vec2::new(-1.5, -1.5), 6.5, s); line((3.4, 3.4), (8.0, 8.0)); }
        // square.grid.2x2
        Chip::Apps => for (dx, dy) in [(-8.0, -8.0), (1.0, -8.0), (-8.0, 1.0), (1.0, 1.0)] {
            p.rect_stroke(Rect::from_min_size(c + Vec2::new(dx, dy), Vec2::splat(7.0)), 2.2, s, egui::StrokeKind::Inside);
        },
        // doc.text
        Chip::Docs => {
            let pts = [(-6.5, -9.0), (2.5, -9.0), (6.5, -5.0), (6.5, 9.0), (-6.5, 9.0), (-6.5, -9.0)];
            for w2 in pts.windows(2) { line(w2[0], w2[1]); }
            line((2.5, -9.0), (2.5, -5.0)); line((2.5, -5.0), (6.5, -5.0));
            line((-3.0, 0.0), (3.0, 0.0)); line((-3.0, 3.5), (3.0, 3.5));
        }
        // photo
        Chip::Images => {
            p.rect_stroke(Rect::from_center_size(c, Vec2::new(18.0, 14.0)), 3.0, s, egui::StrokeKind::Inside);
            p.circle_stroke(c + Vec2::new(-3.0, -2.0), 1.8, s);
            line((-8.0, 4.5), (-3.5, 1.0)); line((-3.5, 1.0), (0.5, 4.0)); line((0.5, 4.0), (3.0, 2.0)); line((3.0, 2.0), (8.0, 6.0));
        }
        // chevron.left.forwardslash.chevron.right
        Chip::Code => {
            line((-4.5, -5.5), (-9.0, 0.0)); line((-9.0, 0.0), (-4.5, 5.5));
            line((4.5, -5.5), (9.0, 0.0)); line((9.0, 0.0), (4.5, 5.5));
            line((1.8, -7.0), (-1.8, 7.0));
        }
    }
}

/// A Finder-style document icon: white page with a folded corner and a coloured type label.
fn draw_file_icon(p: &egui::Painter, r: Rect, ext: &str, col: Color32) {
    let page = Rect::from_center_size(r.center(), Vec2::new(r.width() * 0.76, r.height()));
    let fold = page.width() * 0.34;
    let (l, t, rt, b) = (page.left(), page.top(), page.right(), page.bottom());
    let outline = [Pos2::new(l, t), Pos2::new(rt - fold, t), Pos2::new(rt, t + fold), Pos2::new(rt, b), Pos2::new(l, b)];
    let shadow: Vec<Pos2> = outline.iter().map(|q| *q + Vec2::new(0.0, 1.2)).collect();
    p.add(egui::Shape::convex_polygon(shadow, Color32::from_black_alpha(30), Stroke::NONE));
    p.add(egui::Shape::convex_polygon(outline.to_vec(), Color32::WHITE, Stroke::new(0.8_f32, Color32::from_gray(190))));
    p.add(egui::Shape::convex_polygon(
        vec![Pos2::new(rt - fold, t), Pos2::new(rt - fold, t + fold), Pos2::new(rt, t + fold)],
        Color32::from_gray(228), Stroke::new(0.8_f32, Color32::from_gray(190))));
    let label = Rect::from_min_max(Pos2::new(l - 3.0, b - page.height() * 0.42), Pos2::new(rt + 3.0, b - 2.5));
    p.rect_filled(label, 2.5, col);
    p.text(label.center(), Align2::CENTER_CENTER, ext.chars().take(4).collect::<String>().to_uppercase(), FontId::proportional(10.0), Color32::WHITE);
}

impl App {
    /// Closing = parking the window out of sight and clearing what was typed. The process, fonts and database stay warm.
    fn hide(&mut self, ctx: &egui::Context) {
        VISIBLE.store(false, std::sync::atomic::Ordering::SeqCst);
        self.hidden = true;
        self.text.clear();
        self.chip = Chip::All;
        self.hits.clear();
        self.total = 0;
        self.selected = 0;
        self.asked += 1;
        self.answered = self.asked;
        self.close_at = None;
        self.was_focused = false;
        self.shown_at = None;
        self.frost = None;
        self.frost_tried = false;
        self.glass.clear();
        self.placer.park(ctx);
    }

    /// Opening: re-read the settings (they may have changed in the Settings window) and place the window.
    fn begin_show(&mut self, ctx: &egui::Context) {
        if let Ok(st) = Store::open(&default_db_path()) {
            let hidden: Vec<String> = st.setting("hidden_categories").ok().flatten().unwrap_or_default().split(',').filter(|s| !s.is_empty()).map(String::from).collect();
            let private: Vec<String> = st.setting("privacy_folders").ok().flatten().unwrap_or_default().lines().map(String::from).collect();
            self.max_rows = st.setting("max_rows").ok().flatten().and_then(|v| v.parse().ok()).unwrap_or(8usize).clamp(3, 12);
            self.close_on_blur = st.setting("close_on_blur").ok().flatten().map(|v| v != "0").unwrap_or(true);
            self.exclusions = exclusions(&hidden, &private);
            self.look = Look::load(&st);
        }
        self.hidden = false;
        VISIBLE.store(true, std::sync::atomic::Ordering::SeqCst);
        ctx.request_repaint();
    }

    /// The window never changes size (resizing a transparent window shows a half-drawn frame), so it is
    /// as tall as the panel can ever get; the unused part is transparent.
    fn window_height(&self) -> f32 {
        2.0 * PAD + 24.0 + HEAD + 16.0 + 44.0 + 42.0 + self.max_rows as f32 * ROW + 52.0
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] { [0.0, 0.0, 0.0, 0.0] }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.hidden {
            if SHOW.swap(false, std::sync::atomic::Ordering::SeqCst) { self.begin_show(ctx); } else { return; }
        } else if !VISIBLE.load(std::sync::atomic::Ordering::SeqCst) && self.placer.ready() {
            VISIBLE.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let design = Vec2::new(WIDTH, self.window_height());
        self.placer.step(ctx, design, 0.36, (0.7, 1.4), PAD + HEAD / 2.0);
        if !self.placer.ready() { return; }                      // stay transparent until placed
        if self.shown_at.is_none() { self.shown_at = Some(Instant::now()); }
        if self.look.mode == Transparency::Blur && !self.frost_tried {
            self.frost_tried = true;
            self.frost = frost_from_snapshot(ctx, &self.placer).ok();
        }
        // Close like Spotlight when the window loses focus (only once it has really been shown and focused).
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        if focused { self.was_focused = true; }
        else if self.was_focused && self.close_on_blur && !ctx.memory(|m| m.any_popup_open()) && self.shown_at.map(|t| t.elapsed() > Duration::from_millis(700)).unwrap_or(false) {
            request_close(ctx);
        }

        while let Ok(files) = self.stats.try_recv() {
            if self.index_files >= 0 && files != self.index_files { self.index_changed_at = Some(Instant::now()); }
            self.index_files = files;
        }
        let mut refresh = false;
        while let Ok(r) = self.from_worker.try_recv() {
            if r.generation >= self.answered { self.answered = r.generation; self.hits = r.hits; self.total = r.total; self.capped = r.capped; self.millis = r.millis; self.selected = 0; }
        }
        if self.indexing() { ctx.request_repaint_after(Duration::from_millis(100)); }

        // Keyboard
        let (down, up, enter, ctrl_enter, esc) = ctx.input_mut(|i| (
            i.consume_key(Modifiers::NONE, Key::ArrowDown), i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::Enter), i.consume_key(Modifiers::COMMAND, Key::Enter),
            i.consume_key(Modifiers::NONE, Key::Escape)));
        for (n, key) in [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5].into_iter().enumerate() {
            if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, key)) && self.chip != Chip::ALL[n] { self.chip = Chip::ALL[n]; refresh = true; }
        }
        if esc { request_close(ctx); }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Comma)) { open_settings(); request_close(ctx); }
        if !self.hits.is_empty() {
            if down { self.selected = (self.selected + 1).min(self.hits.len() - 1); }
            if up { self.selected = self.selected.saturating_sub(1); }
            if let Some(h) = self.hits.get(self.selected) {
                if ctrl_enter { if !h.path.starts_with("shell:") { reveal_path(&h.path); } } else if enter { launch(&self.to_worker, &h.path); request_close(ctx); }
            }
        }

        let needles: Vec<String> = normalize_query(&self.text).split_whitespace()
            .filter(|w| !w.contains(':')).map(|w| w.trim_matches('"').to_lowercase()).collect();
        let rows = self.hits.len().min(self.max_rows);
        let empty_msg = !self.text.trim().is_empty() && self.hits.is_empty() && self.answered == self.asked;
        // Like Spotlight: just a search pill and round buttons until you type, then one panel with the results.
        let expanded = !self.text.trim().is_empty() || self.chip != Chip::All || self.indexing() || self.index_files == 0 || rows > 0;
        let height = if !expanded { HEAD + 2.0 * PAD + 12.0 } else {
            let mut h = 2.0 * PAD + 24.0 + HEAD + 16.0 + 44.0;       // margins, search row, rule, filter pills
            if self.indexing() { h += 42.0; }
            h += if rows > 0 { rows as f32 * ROW + 52.0 } else { 70.0 };
            h
        };

        let alpha = self.look.alpha();
        let panel_color = Color32::from_rgba_unmultiplied(244, 245, 249, alpha);
        let frost = self.frost.clone();
        let mut active = Rect::NOTHING;     // what is actually drawn; the rest of the window is transparent
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
            let area = ui.max_rect().shrink(PAD);
            if !expanded {
                // ---- idle: a floating pill and four round buttons, each with its own shadow
                active = Rect::from_min_size(area.min, Vec2::new(area.width(), HEAD));
                let bubbles_w = 4.0 * (HEAD + 10.0);
                let pill = Rect::from_min_size(area.min, Vec2::new(area.width() - bubbles_w, HEAD));
                let ppp = ctx.pixels_per_point();
                let glass = self.glass.entry("pill".to_string())
                    .or_insert_with(|| make_glass(ctx, "pill", pill, HEAD / 2.0, [246, 247, 250, alpha], frost.as_ref(), ppp));
                show_glass(ui.painter(), glass, pill);
                if search_field(ui, ctx, &mut self.text, pill, self.was_focused) { refresh = true; }
                if gear_button(ui, pill.right_center() - Vec2::new(26.0, 0.0), 14.0) { open_settings(); request_close(ctx); }
                for (i, chip) in Chip::ALL.into_iter().skip(1).enumerate() {
                    let center = Pos2::new(pill.right() + 10.0 + HEAD / 2.0 + i as f32 * (HEAD + 10.0), pill.center().y);
                    let r = Rect::from_center_size(center, Vec2::splat(HEAD));
                    let resp = ui.interact(r, ui.id().with(("bubble", i)), Sense::click());
                    let hovered = resp.hovered();
                    let key = format!("bubble{i}-{hovered}");
                    let tint = if hovered { [255, 255, 255, alpha.saturating_add(12)] } else { [246, 247, 250, alpha] };
                    let glass = self.glass.entry(key.clone()).or_insert_with(|| make_glass(ctx, &key, r, HEAD / 2.0, tint, frost.as_ref(), ppp));
                    show_glass(ui.painter(), glass, r);
                    draw_chip_icon(ui.painter(), chip, center, Color32::from_gray(85));
                    if resp.on_hover_text(format!("{}  (Ctrl+{})", chip.label(), i + 2)).clicked() { self.chip = chip; refresh = true; }
                }
            } else {
                // ---- expanded: one rounded panel, search row on top, thin rules between sections
                let panel = Rect::from_min_size(area.min, Vec2::new(area.width(), height - 2.0 * PAD));
                active = panel;
                soft_shadow(ui.painter(), panel, 28.0);
                paint_glass(ui.painter(), panel, 28.0, panel_color, frost.as_ref());
                ui.painter().rect_stroke(panel, 28.0, Stroke::new(1.0_f32, Color32::from_white_alpha(230)), egui::StrokeKind::Inside);
                let inner = panel.shrink2(Vec2::new(18.0, 12.0));
                let head = Rect::from_min_size(inner.min, Vec2::new(inner.width(), HEAD));
                if search_field(ui, ctx, &mut self.text, head, self.was_focused) { refresh = true; }
                if gear_button(ui, head.right_center() - Vec2::new(14.0, 0.0), 14.0) { open_settings(); request_close(ctx); }
                let mut content = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(Pos2::new(inner.left(), head.bottom() + 4.0), inner.max)));
                let ui = &mut content;
                ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
                rule(ui);

                if self.indexing() {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("Indexing…  {} files", self.index_files)).size(13.0).color(Color32::from_gray(90)));
                        let t = ctx.input(|i| i.time) as f32;
                        let (bar, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 8.0), Sense::hover());
                        ui.painter().rect_filled(bar, 4.0, Color32::from_black_alpha(18));
                        let w = bar.width() * 0.25; let x = bar.left() + ((t * 0.7).fract()) * (bar.width() - w);
                        ui.painter().rect_filled(Rect::from_min_size(Pos2::new(x, bar.top()), Vec2::new(w, 8.0)), 4.0, ACCENT);
                    });
                    ui.add_space(2.0);
                    rule(ui);
                }

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    for chip in Chip::ALL {
                        let on = self.chip == chip;
                        let label = egui::RichText::new(chip.label()).size(13.0).color(if on { Color32::WHITE } else { Color32::from_gray(70) });
                        let pill = egui::Button::new(label).fill(if on { ACCENT } else { Color32::from_black_alpha(10) })
                            .stroke(Stroke::NONE).corner_radius(14.0).min_size(Vec2::new(0.0, 28.0));
                        if ui.add(pill).clicked() { self.chip = if on { Chip::All } else { chip }; refresh = true; }
                    }
                });
                ui.add_space(4.0);

                if rows > 0 {
                    for (i, h) in self.hits.iter().take(self.max_rows).enumerate() {
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW), Sense::click());
                        if i == self.selected { ui.painter().rect_filled(r.shrink2(Vec2::new(0.0, 1.0)), 12.0, Color32::from_rgba_unmultiplied(10, 132, 255, 40)); }
                        else if resp.hovered() { ui.painter().rect_filled(r.shrink2(Vec2::new(0.0, 1.0)), 12.0, Color32::from_black_alpha(10)); }
                        let ext = Path::new(&h.name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                        let icon = Rect::from_center_size(r.left_center() + Vec2::new(28.0, 0.0), Vec2::splat(34.0));
                        let is_store_app = ext == "uwp";
                        let tex = if is_store_app { None } else { texture_for(&mut self.icons, ctx, &h.path, &ext) };
                        if let Some(tex) = tex {
                            ui.painter().image(tex.id(), icon, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                        } else if is_store_app {
                            ui.painter().rect_filled(icon, 10.0, ACCENT);
                            ui.painter().text(icon.center(), Align2::CENTER_CENTER, h.name.chars().next().unwrap_or('?').to_uppercase().to_string(), FontId::proportional(17.0), Color32::WHITE);
                        } else {
                            draw_file_icon(ui.painter(), icon, &ext, ext_color(&ext));
                        }
                        let is_app = ext == "lnk" || is_store_app;
                        let shown = if ext == "lnk" { h.name[..h.name.len() - 4].to_string() } else { h.name.clone() };
                        let job = name_job(&fsearch::highlight::segments_visual(&shown, &needles, 64), FontId::proportional(16.0), Color32::from_gray(25));
                        let galley = ui.painter().layout_job(job);
                        ui.painter().galley(r.left_top() + Vec2::new(56.0, 8.0), galley, Color32::from_gray(25));
                        let folder = if is_store_app { "Microsoft Store app".to_string() } else { Path::new(&h.path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default() };
                        ui.painter().text(r.left_top() + Vec2::new(56.0, 31.0), Align2::LEFT_TOP, fsearch::bidi::visual(&elide_middle(&folder, 78)), FontId::proportional(12.0), Color32::from_gray(120));
                        let right = if is_app { "Application".to_string() } else { format!("{}  ·  {}  ·  {}", short_kind(&h.kind), size_text(h.size), age(h.mtime)) };
                        ui.painter().text(r.right_center() - Vec2::new(12.0, 0.0), Align2::RIGHT_CENTER, right, FontId::proportional(12.5), Color32::from_gray(110));
                        if resp.clicked() { self.selected = i; launch(&self.to_worker, &h.path); request_close(ctx); }
                        if resp.secondary_clicked() { self.selected = i; }
                        let mut chosen = None;
                        resp.context_menu(|ui| { chosen = context_menu_items(ui, &ext, is_store_app); });
                        if let Some(action) = chosen { run_action(action, &h.path, &shown, ctx, &self.to_worker, &mut self.close_at); }
                    }
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(format!("{}{} result{}  ·  {} ms  ·  ↑↓ select   ⏎ open   Ctrl+⏎ show in folder   Ctrl+, settings   Esc close",
                        self.total, if self.capped { "+" } else { "" }, if self.total == 1 { "" } else { "s" }, self.millis)).size(12.0).color(Color32::from_gray(120)));
                } else if empty_msg {
                    ui.add_space(14.0);
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new("No results").size(16.0).color(Color32::from_gray(110)));
                        if self.indexing() { ui.label(egui::RichText::new("Still indexing — more files appear as they are found").size(12.5).color(Color32::from_gray(130))); }
                    });
                } else if self.index_files == 0 {
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("Nothing is indexed yet. In PowerShell run:  fsearch add \"C:\\Users\\you\\Documents\"").size(13.0).color(Color32::from_gray(100)));
                } else if self.text.trim().is_empty() {
                    ui.add_space(10.0);
                    ui.vertical_centered(|ui| ui.label(egui::RichText::new(if self.chip == Chip::All { "Type to search".to_string() } else { format!("No {} yet — type to search", self.chip.label().to_lowercase()) }).size(14.0).color(Color32::from_gray(110))));
                }
            }
        });
        // A click on the transparent part counts as "clicking elsewhere".
        if let Some(t) = self.close_at {
            if Instant::now() >= t { request_close(ctx); } else { ctx.request_repaint_after(Duration::from_millis(50)); }
        }
        if self.close_on_blur && self.placer.ready() && !ctx.memory(|m| m.any_popup_open()) && ctx.input(|i| i.pointer.any_pressed()) {
            if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                if !active.expand(6.0).contains(p) { request_close(ctx); }
            }
        }
        if refresh { self.search(); }
        if CLOSE_REQ.swap(false, std::sync::atomic::Ordering::SeqCst) || ctx.data(|d| d.get_temp::<bool>(egui::Id::new("close_request"))).unwrap_or(false) {
            ctx.data_mut(|d| d.remove::<bool>(egui::Id::new("close_request")));
            self.hide(ctx);
        }
    }
}

#[cfg(windows)]
fn already_running() -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "Local\\fsearch-gui".encode_utf16().chain(Some(0)).collect();
    unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()); GetLastError() == ERROR_ALREADY_EXISTS }
}
#[cfg(not(windows))]
fn already_running() -> bool { false }

#[derive(Clone, Copy, PartialEq)]
enum FrostKind { None, Fan }

struct Variant { label: &'static str, shadow: bool, frost: FrostKind, tint: bool, solid: bool, circles: bool }

const VARIANTS: [Variant; 6] = [
    Variant { label: "A  OLD: shadow + frost + tint, rect shapes", shadow: true, frost: FrostKind::Fan, tint: true, solid: false, circles: false },
    Variant { label: "B  OLD: shadow + tint, rect shapes, no frost", shadow: true, frost: FrostKind::None, tint: true, solid: false, circles: false },
    Variant { label: "C  OLD: frost + tint, rect shapes, no shadow", shadow: false, frost: FrostKind::Fan, tint: true, solid: false, circles: false },
    Variant { label: "D  solid white, rect shapes (reference)", shadow: false, frost: FrostKind::None, tint: true, solid: true, circles: false },
    Variant { label: "E  NEW: shadow + frost + tint, circle shapes", shadow: true, frost: FrostKind::Fan, tint: true, solid: false, circles: true },
    Variant { label: "F  NEW: shadow + tint, circle shapes, no frost", shadow: true, frost: FrostKind::None, tint: true, solid: false, circles: true },
];

/// `fsearch-gui --diagnose`: the idle bar drawn seven ways in one window, to see which way shows hairlines.
struct Diagnose { placer: Placer, frost: Option<Frost>, tried: bool, report: String }

impl eframe::App for Diagnose {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] { [0.0, 0.0, 0.0, 0.0] }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let design = Vec2::new(WIDTH, 6.0 * 78.0 + 2.0 * PAD + 22.0);
        self.placer.step(ctx, design, 0.36, (0.7, 1.4), design.y / 2.0);
        if !self.placer.ready() { return; }
        if ctx.input(|i| i.key_pressed(Key::Escape)) { ctx.send_viewport_cmd(egui::ViewportCommand::Close); }
        if !self.tried {
            self.tried = true;
            let mean = SNAPSHOT.lock().unwrap().as_ref().map(|snap| snap.mean);
            self.report = match mean {
                None => "snapshot: none was taken (pointer monitor unknown)".to_string(),
                Some((r, g, b)) => {
                    match frost_from_snapshot(ctx, &self.placer) {
                        Ok(f) => { self.frost = Some(f); format!("snapshot of the whole screen, taken before the window opened.  mean colour {r},{g},{b}  OK") }
                        Err(e) => format!("snapshot: {e}"),
                    }
                }
            };
        }
        let frost = self.frost.clone();
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
            let area = ui.max_rect().shrink(PAD);
            let note = Rect::from_min_size(area.min, Vec2::new(area.width(), 16.0));
            ui.painter().rect_filled(note, 4.0, Color32::from_white_alpha(225));
            ui.painter().text(note.left_center() + Vec2::new(5.0, 0.0), Align2::LEFT_CENTER, &self.report, FontId::proportional(11.0), Color32::BLACK);
            for (k, v) in VARIANTS.iter().enumerate() {
                let top = area.top() + 22.0 + k as f32 * 78.0 + 18.0;
                let label = Rect::from_min_size(Pos2::new(area.left(), top - 17.0), Vec2::new(330.0, 16.0));
                ui.painter().rect_filled(label, 4.0, Color32::from_white_alpha(225));
                ui.painter().text(label.left_center() + Vec2::new(5.0, 0.0), Align2::LEFT_CENTER, v.label, FontId::proportional(11.0), Color32::BLACK);
                let pill = Rect::from_min_size(Pos2::new(area.left(), top), Vec2::new(area.width() - 4.0 * (HEAD + 10.0), HEAD));
                let mut shapes = vec![pill];
                for i in 0..4 { shapes.push(Rect::from_center_size(Pos2::new(pill.right() + 10.0 + HEAD / 2.0 + i as f32 * (HEAD + 10.0), pill.center().y), Vec2::splat(HEAD))); }
                for r in shapes {
                    if v.shadow { soft_shadow_with(ui.painter(), r, HEAD / 2.0, v.circles); }
                    if !v.solid {
                        match (v.frost, frost.as_ref()) {
                            (FrostKind::Fan, Some(f)) => paint_glass_with(ui.painter(), r, HEAD / 2.0, Color32::TRANSPARENT, Some(f), v.circles),
                            _ => {}
                        }
                    }
                    let tint = if v.solid { Color32::from_rgb(246, 247, 250) } else { Color32::from_rgba_unmultiplied(246, 247, 250, 150) };
                    if v.tint { fill_with(ui.painter(), r, HEAD / 2.0, tint, v.circles); }
                }
            }
        });
    }
}

fn run_diagnose() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([WIDTH, 560.0]).with_decorations(false).with_transparent(true)
            .with_always_on_top().with_taskbar(false).with_resizable(false).with_title("fsearch diagnose").with_position([-30000.0, -30000.0]),
        ..Default::default()
    };
    eframe::run_native("fsearch diagnose", options, Box::new(|cc| { fsearch::settings_ui::install_fonts(&cc.egui_ctx); Ok(Box::new(Diagnose { placer: Placer::new(), frost: None, tried: false, report: String::new() })) }))
}

fn window_icon() -> egui::IconData {
    egui::IconData { rgba: include_bytes!("../../assets/icon-64.rgba").to_vec(), width: 64, height: 64 }
}

fn main() -> eframe::Result {
    fsearch::win::set_dpi_aware();
    if std::env::args().any(|a| a == "--settings") { return fsearch::settings_ui::run(); }
    if std::env::args().any(|a| a == "--setup") { return fsearch::setup_ui::run(); }
    if let Ok(store) = fsearch::store::Store::open(&fsearch::store::default_db_path()) {
        if fsearch::setup_ui::needed(&store) { drop(store); return fsearch::setup_ui::run(); }
    }
    if std::env::args().any(|a| a == "--diagnose") { take_snapshot(); return run_diagnose(); }
    let background = std::env::args().any(|a| a == "--background");
    if already_running() {
        // A launcher is already running (hidden): make it open, then leave. Retry briefly in case it is still starting.
        if !background { for _ in 0..10 { if fsearch::win::signal_launcher() { break; } std::thread::sleep(Duration::from_millis(100)); } }
        return Ok(());
    }
    LAUNCHER_EVENT.store(fsearch::win::create_launcher_event(), std::sync::atomic::Ordering::SeqCst);
    if !background {
        if Look::load_default().mode == Transparency::Blur { take_snapshot(); }
        VISIBLE.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([WIDTH, 116.0]).with_decorations(false).with_transparent(true)
            .with_always_on_top().with_taskbar(false).with_resizable(false).with_title("fsearch").with_icon(window_icon()).with_position([-30000.0, -30000.0]),
        ..Default::default()
    };
    eframe::run_native("fsearch", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

#[cfg(test)]
mod tests {
    use super::normalize_query;
    #[test]
    fn drops_ms_and_microsoft_only_when_other_words_remain() {
        assert_eq!(normalize_query("ms word"), "word");
        assert_eq!(normalize_query("Microsoft  Excel ext:lnk"), "Excel ext:lnk");
        assert_eq!(normalize_query("ms"), "ms");
        assert_eq!(normalize_query("ms microsoft"), "");   // nothing left: searches everything, never errors
    }
}
