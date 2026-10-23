//! Opens a window in the middle of the screen the pointer is on, scaled to that screen.
//! The window is transparent and draws nothing until it has been moved, zoomed and sized (`ready()`), so there is no flash.
use crate::layout::zoom_for;
use crate::win::{self, MonitorRect};
use eframe::egui::{self, pos2, Vec2};

pub struct Placer {
    stage: u8,
    frames: u32,
    monitor: Option<MonitorRect>,
    rect_px: Option<(i32, i32, i32, i32)>,
    /// Monitor the window was parked above when it was last hidden: showing it there again needs no re-scaling.
    parked: Option<MonitorRect>,
    fast: bool,
}

impl Placer {
    pub fn new() -> Self { Self { stage: 0, frames: 0, monitor: None, rect_px: None, parked: None, fast: false } }

    /// Hides: forget where the window is and move it far above the monitor it was on (still on that monitor's scale).
    pub fn park(&mut self, ctx: &egui::Context) {
        if let Some(m) = self.monitor {
            let ppp = ctx.pixels_per_point();
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos2((m.left + 60) as f32 / ppp, (m.top - 20000) as f32 / ppp)));
            self.parked = Some(m);
        }
        self.stage = 0; self.frames = 0; self.rect_px = None; self.fast = false;
    }

    pub fn ready(&self) -> bool { self.stage >= 4 }

    /// The monitor the window was placed on.
    pub fn monitor(&self) -> Option<MonitorRect> { self.monitor }

    /// Where the window ended up, in physical pixels (x, y, width, height). Known once placed on a monitor.
    pub fn window_rect_px(&self) -> Option<(i32, i32, i32, i32)> { self.rect_px }

    /// Call at the start of every frame. `design` is the window size in points; `fraction` how much of the screen
    /// width it should cover; `anchor_y` the y offset inside the window (points) that must land on the screen's
    /// vertical centre (the search bar for the launcher, half the height for ordinary windows).
    pub fn step(&mut self, ctx: &egui::Context, design: Vec2, fraction: f32, zoom: (f32, f32), anchor_y: f32) {
        if self.ready() { return; }
        self.frames += 1;
        match self.stage {
            0 => {
                // Go to the pointer's monitor first, so the scale factor we read next is that monitor's.
                self.monitor = win::cursor_monitor();
                if self.monitor.is_some() && self.monitor == self.parked {
                    // Same screen as last time: scale and DPI are already right, go straight to placing.
                    self.fast = true;
                    self.next(2);
                    ctx.request_repaint();
                    return;
                }
                self.fast = false;
                if let Some(m) = self.monitor {
                    let ppp = ctx.pixels_per_point();
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos2((m.left + 60) as f32 / ppp, (m.top + 60) as f32 / ppp)));
                }
                self.next(1);
            }
            1 if self.frames >= 3 => {
                let native = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or(1.0).max(0.5);
                let screen_logical = match self.monitor {
                    Some(m) => m.width() as f32 / native,
                    None => ctx.input(|i| i.viewport().monitor_size).map(|s| s.x * ctx.zoom_factor()).unwrap_or(1280.0),
                };
                ctx.set_zoom_factor(zoom_for(screen_logical, fraction, design.x, zoom.0, zoom.1));
                self.next(2);
            }
            2 if self.frames >= (if self.fast { 1 } else { 2 }) => {
                let ppp = ctx.pixels_per_point();
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(design));
                let pos = match self.monitor {
                    Some(m) => {
                        let (w, h) = (design.x * ppp, design.y * ppp);
                        let x = m.left as f32 + (m.width() as f32 - w) / 2.0;
                        let y = m.top as f32 + m.height() as f32 / 2.0 - anchor_y * ppp;
                        let y = y.min(m.bottom as f32 - h - 8.0).max(m.top as f32 + 8.0);   // always fully on the screen
                        self.rect_px = Some((x.round() as i32, y.round() as i32, w.round() as i32, h.round() as i32));
                        pos2(x / ppp, y / ppp)
                    }
                    None => {
                        let m = ctx.input(|i| i.viewport().monitor_size).unwrap_or(Vec2::new(1280.0, 720.0));
                        pos2(((m.x - design.x) / 2.0).max(0.0), ((m.y - design.y) / 2.0).max(0.0))
                    }
                };
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
                if self.fast { ctx.send_viewport_cmd(egui::ViewportCommand::Focus); }   // take the keyboard as early as possible
                self.next(3);
            }
            3 if self.frames >= (if self.fast { 1 } else { 3 }) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                self.next(4);
            }
            _ => {}
        }
        if !self.ready() { ctx.request_repaint(); }
    }

    fn next(&mut self, stage: u8) { self.stage = stage; self.frames = 0; }
}
