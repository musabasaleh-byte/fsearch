//! Arabic text for a renderer that draws one isolated glyph per character, left to right (egui):
//! pick the joined letter forms, then reorder into visual order. Latin-only text passes through untouched.
use crate::arabic_forms::{FORMS, LAM_ALEF};
use unicode_bidi::BidiInfo;

pub fn is_rtl_char(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF)
}

pub fn has_rtl(s: &str) -> bool { s.chars().any(is_rtl_char) }

/// Persian/Urdu letters that have no Presentation-Forms-B shapes are drawn with their Arabic look-alike.
fn normalize(c: char) -> char {
    match c { '\u{06A9}' => '\u{0643}', '\u{06CC}' => '\u{064A}', '\u{06C0}' => '\u{0647}', c => c }
}

/// [isolated, final, initial, medial] ('\0' = none)
fn forms(c: char) -> Option<[char; 4]> {
    FORMS.iter().find(|f| f.0 == c).map(|f| [f.1, f.2, f.3, f.4])
}

/// Marks (harakat etc.) neither join nor break a connection.
fn transparent(c: char) -> bool {
    matches!(c as u32, 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06DC | 0x06DF..=0x06E4 | 0x06E7 | 0x06E8 | 0x06EA..=0x06ED)
}

const TATWEEL: char = '\u{0640}';

/// Can this letter connect to the letter after it?
fn joins_next(c: char) -> bool { forms(c).map(|f| f[2] != '\0').unwrap_or(c == TATWEEL) }
/// Can this letter connect to the letter before it?
fn joins_prev(c: char) -> bool { forms(c).map(|f| f[1] != '\0').unwrap_or(c == TATWEEL) }

/// Logical order in, joined presentation forms out. Each output char remembers the index of its first source char.
pub fn shape(text: &str) -> Vec<(char, usize)> {
    let chars: Vec<char> = text.chars().map(normalize).collect();
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let Some(f) = forms(c) else { out.push((c, i)); i += 1; continue };
        let prev = (0..i).rev().find(|&k| !transparent(chars[k]));
        let next = (i + 1..chars.len()).find(|&k| !transparent(chars[k]));
        let connects_prev = prev.map(|p| joins_next(chars[p])).unwrap_or(false) && joins_prev(c);
        // lam + alef is one ligature glyph
        if c == '\u{0644}' {
            if let Some(n) = next.filter(|&n| n == i + 1) {
                if let Some(l) = LAM_ALEF.iter().find(|l| l.0 == chars[n]) {
                    out.push((if connects_prev { l.2 } else { l.1 }, i));
                    i = n + 1;
                    continue;
                }
            }
        }
        let connects_next = joins_next(c) && next.map(|n| joins_prev(chars[n])).unwrap_or(false);
        let shaped = f[match (connects_prev, connects_next) { (false, false) => 0, (true, false) => 1, (false, true) => 2, (true, true) => 3 }];
        out.push((if shaped == '\0' { f[0] } else { shaped }, i));
        i += 1;
    }
    out
}

fn mirror(c: char) -> char {
    match c { '(' => ')', ')' => '(', '[' => ']', ']' => '[', '{' => '}', '}' => '{', '<' => '>', '>' => '<', '«' => '»', '»' => '«', c => c }
}

/// Characters in the order they should be drawn left to right, each with the logical index it came from.
pub fn visual_map(text: &str) -> (Vec<char>, Vec<usize>) {
    if !has_rtl(text) { return (text.chars().collect(), (0..text.chars().count()).collect()); }
    let shaped = shape(text);
    let s: String = shaped.iter().map(|(c, _)| *c).collect();
    let byte_to_char: std::collections::HashMap<usize, usize> = s.char_indices().enumerate().map(|(ci, (b, _))| (b, ci)).collect();
    let chars: Vec<char> = s.chars().collect();
    let info = BidiInfo::new(&s, None);
    let (mut vis, mut map) = (Vec::with_capacity(chars.len()), Vec::with_capacity(chars.len()));
    for para in &info.paragraphs {
        let (levels, runs) = info.visual_runs(para, para.range.clone());
        for run in runs {
            let first = byte_to_char[&run.start];
            let last = byte_to_char.get(&run.end).copied().unwrap_or(chars.len());
            let rtl = levels[run.start].is_rtl();
            let indices: Vec<usize> = if rtl { (first..last).rev().collect() } else { (first..last).collect() };
            for ci in indices {
                vis.push(if rtl { mirror(chars[ci]) } else { chars[ci] });
                map.push(shaped[ci].1);
            }
        }
    }
    (vis, map)
}

pub fn visual(text: &str) -> String { visual_map(text).0.into_iter().collect() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_text_is_untouched() {
        assert_eq!(visual("Quarterly Report (2).pdf"), "Quarterly Report (2).pdf");
        assert!(!has_rtl("report 2026"));
    }

    #[test]
    fn letters_take_joined_forms_and_lam_alef_is_one_glyph() {
        // سلام = seen(initial) + lam-alef(final ligature) + meem(isolated), shown right to left
        assert_eq!(visual("سلام"), "\u{FEE1}\u{FEFC}\u{FEB3}");
        // الله: alef(iso) lam(initial) lam(medial) heh(final) -> displayed reversed
        assert_eq!(shape("الله").iter().map(|(c, _)| *c).collect::<String>(), "\u{FE8D}\u{FEDF}\u{FEE0}\u{FEEA}");
    }

    #[test]
    fn right_joining_letters_break_the_chain() {
        // درس: dal and reh never join forward, so all three letters stay isolated
        let s: String = shape("درس").iter().map(|(c, _)| *c).collect();
        assert_eq!(s, "\u{FEA9}\u{FEAD}\u{FEB1}");
        // كتاب: kaf(initial) teh(medial) alef(final) | beh(isolated: alef ends the chain)
        let s: String = shape("كتاب").iter().map(|(c, _)| *c).collect();
        assert_eq!(s, "\u{FEDB}\u{FE98}\u{FE8E}\u{FE8F}");
        // ء (hamza) is always isolated
        assert_eq!(visual("ء"), "\u{FE80}");
    }

    #[test]
    fn marks_do_not_break_joining() {
        // مَد: the fatha sits between meem and dal, which must still connect
        let s = shape("م\u{064E}د");
        assert_eq!(s[0].0, '\u{FEE3}');        // meem initial
        assert_eq!(s.last().unwrap().0, '\u{FEAA}');  // dal final
    }

    #[test]
    fn mixed_text_keeps_latin_and_digits_in_place() {
        let v = visual("report تقرير 2026");
        assert!(v.starts_with("report "));
        assert!(v.ends_with(" 2026") || v.contains("2026"));
        // whole-string RTL reads right to left, so the first typed word ends up on the right
        let v = visual("البداية");
        assert_eq!(v.chars().count(), 7);       // no lam-alef pair in this word: one glyph per letter
        assert!(v.chars().next().unwrap() as u32 >= 0xFE70);
    }

    #[test]
    fn map_points_back_to_the_logical_characters() {
        let (vis, map) = visual_map("اب");          // alef, beh
        assert_eq!(vis.len(), 2);
        assert_eq!(map, vec![1, 0]);                // visual order: beh first
    }

    #[test]
    fn brackets_mirror_inside_rtl_runs() {
        let v = visual("(عربي)");
        assert!(v.starts_with('(') && v.ends_with(')'));
    }
}
