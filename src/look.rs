//! How see-through the search window is. Stored in the index database so the settings window and the launcher agree.
use crate::store::Store;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transparency {
    /// Fully opaque.
    Off,
    /// Translucent fill: what is behind shows through, sharp.
    Glass,
    /// Translucent fill over a blurred copy of what is behind (Windows acrylic).
    Blur,
}

impl Transparency {
    pub fn as_str(self) -> &'static str {
        match self { Transparency::Off => "off", Transparency::Glass => "glass", Transparency::Blur => "blur" }
    }
    pub fn parse(s: &str) -> Self {
        match s.trim() { "off" => Transparency::Off, "blur" => Transparency::Blur, _ => Transparency::Glass }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Look {
    pub mode: Transparency,
    /// Opacity of the panel in percent (100 = solid).
    pub level: u8,
}

impl Look {
    pub const DEFAULT: Look = Look { mode: Transparency::Blur, level: 80 };

    pub fn load(store: &Store) -> Look {
        let mode = store.setting("transparency_mode").ok().flatten().map(|s| Transparency::parse(&s)).unwrap_or(Look::DEFAULT.mode);
        let level = store.setting("transparency_level").ok().flatten().and_then(|s| s.parse::<u8>().ok()).unwrap_or(Look::DEFAULT.level);
        Look { mode, level: level.clamp(20, 100) }
    }

    /// Reads the saved look from the default database (the built-in default when it cannot be opened).
    pub fn load_default() -> Look {
        Store::open(&crate::store::default_db_path()).map(|s| Look::load(&s)).unwrap_or(Look::DEFAULT)
    }

    pub fn save(&self, store: &Store) {
        let _ = store.set_setting("transparency_mode", self.mode.as_str());
        let _ = store.set_setting("transparency_level", &self.level.to_string());
    }

    /// Alpha (0-255) for the panel and search bar fill. Text always stays readable: glass never goes below 55 %.
    pub fn alpha(&self) -> u8 {
        let percent = match self.mode {
            Transparency::Off => 100,
            Transparency::Glass => self.level.clamp(45, 100),
            Transparency::Blur => self.level.clamp(25, 100),
        };
        // A touch more see-through than the slider says (except solid), which reads as natural frosted glass.
        let percent = if self.mode == Transparency::Off { 100.0 } else { percent as f32 * 0.88 };
        (percent * 255.0 / 100.0) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_round_trip_and_unknown_values_mean_glass() {
        for m in [Transparency::Off, Transparency::Glass, Transparency::Blur] { assert_eq!(Transparency::parse(m.as_str()), m); }
        assert_eq!(Transparency::parse("???"), Transparency::Glass);
    }

    #[test]
    fn alpha_follows_the_level_within_safe_limits() {
        assert_eq!(Look { mode: Transparency::Off, level: 40 }.alpha(), 255);                 // solid ignores the slider
        assert_eq!(Look { mode: Transparency::Glass, level: 100 }.alpha(), 224);
        assert_eq!(Look { mode: Transparency::Glass, level: 10 }.alpha(), 100);               // never too see-through to read
        assert_eq!(Look { mode: Transparency::Blur, level: 60 }.alpha(), 134);
        assert_eq!(Look { mode: Transparency::Blur, level: 10 }.alpha(), 56);
    }

    #[test]
    fn stored_in_the_database() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(Look::load(&store), Look::DEFAULT);
        let look = Look { mode: Transparency::Blur, level: 55 };
        look.save(&store);
        assert_eq!(Look::load(&store), look);
        store.set_setting("transparency_level", "9999").unwrap();
        assert_eq!(Look::load(&store).level, Look::DEFAULT.level);   // not a valid number: back to the default
        store.set_setting("transparency_level", "5").unwrap();
        assert_eq!(Look::load(&store).level, 20);                    // valid but out of range: clamped
    }
}
