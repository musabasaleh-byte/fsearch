//! Splits a file name into plain and matched pieces, cutting long names around the first match so the
//! part you typed stays visible. Indices are in characters, so Arabic and emoji names are safe.

fn lowered(s: &str) -> Vec<char> {
    s.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect()
}

/// Character ranges (start, end) of every case-insensitive occurrence of any needle, merged.
pub fn match_ranges(text: &str, needles: &[String]) -> Vec<(usize, usize)> {
    let t = lowered(text);
    let mut ranges = Vec::new();
    for needle in needles {
        let n = lowered(needle);
        if n.is_empty() || t.len() < n.len() { continue; }
        for i in 0..=t.len() - n.len() {
            if t[i..i + n.len()] == n[..] { ranges.push((i, i + n.len())); }
        }
    }
    ranges.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r.0 <= last.1 => last.1 = last.1.max(r.1),
            _ => merged.push(r),
        }
    }
    merged
}

/// Turns per-character flags into pieces, shortening to `max_chars` around `anchor` (the first match) with "…".
fn assemble(chars: &[char], flags: &[bool], anchor: Option<usize>, max_chars: usize) -> Vec<(String, bool)> {
    let (mut start, mut end) = (0, chars.len());
    if chars.len() > max_chars {
        let room = max_chars.saturating_sub(2).max(1);          // keep space for the ellipses
        start = anchor.map(|a| a.saturating_sub(room / 3)).unwrap_or(0);
        if start + room > chars.len() { start = chars.len() - room; }
        end = start + room;
    }
    let mut out: Vec<(String, bool)> = Vec::new();
    if start > 0 { out.push(("…".into(), false)); }
    for i in start..end {
        match out.last_mut() {
            Some((s, flag)) if *flag == flags[i] && s != "…" => s.push(chars[i]),
            _ => out.push((chars[i].to_string(), flags[i])),
        }
    }
    if end < chars.len() { out.push(("…".into(), false)); }
    out
}

/// Pieces to draw, as (text, is_match). Names longer than `max_chars` are shortened with "…" so that the
/// first match is inside the visible part.
pub fn segments(text: &str, needles: &[String], max_chars: usize) -> Vec<(String, bool)> {
    let chars: Vec<char> = text.chars().collect();
    let ranges = match_ranges(text, needles);
    let flags: Vec<bool> = (0..chars.len()).map(|i| ranges.iter().any(|r| i >= r.0 && i < r.1)).collect();
    assemble(&chars, &flags, ranges.first().map(|r| r.0), max_chars)
}

/// Same, but for drawing with a renderer that cannot do Arabic: letters are joined and reordered for display
/// (see `bidi`). Matches are found in the logical text and carried over to the visual characters.
pub fn segments_visual(text: &str, needles: &[String], max_chars: usize) -> Vec<(String, bool)> {
    if !crate::bidi::has_rtl(text) { return segments(text, needles, max_chars); }
    let ranges = match_ranges(text, needles);
    let (vis, map) = crate::bidi::visual_map(text);
    let flags: Vec<bool> = map.iter().map(|&src| ranges.iter().any(|r| src >= r.0 && src < r.1)).collect();
    let anchor = flags.iter().position(|&f| f);
    assemble(&vis, &flags, anchor, max_chars)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn n(words: &[&str]) -> Vec<String> { words.iter().map(|w| w.to_lowercase()).collect() }

    #[test]
    fn short_names_are_whole_and_matches_are_case_insensitive() {
        assert_eq!(segments("My Budget.xlsx", &n(&["budget"]), 40), vec![("My ".into(), false), ("Budget".into(), true), (".xlsx".into(), false)]);
    }

    #[test]
    fn long_names_are_cut_around_the_match() {
        let name = "P-0068-PH01-D01-01D009-BIN-MAT-CIV-000005 [00] Technical Submittal for Lightweight Concrete Blocks - AWAZEL THE LEADERS (2).pdf";
        let segs = segments(name, &n(&["lightweight"]), 50);
        let joined: String = segs.iter().map(|(s, _)| s.as_str()).collect();
        assert!(joined.starts_with('…') && joined.ends_with('…'));
        assert!(joined.chars().count() <= 50);
        assert!(segs.iter().any(|(s, m)| *m && s == "Lightweight"));
    }

    #[test]
    fn no_match_keeps_the_head() {
        let segs = segments(&"x".repeat(100), &n(&["zzz"]), 20);
        assert_eq!(segs.first().unwrap().0.chars().next(), Some('x'));
        assert!(segs.last().unwrap().0 == "…");
    }

    #[test]
    fn overlapping_needles_merge_and_empty_is_ignored() {
        assert_eq!(match_ranges("archive", &n(&["arch", "chiv", ""])), vec![(0, 6)]);
    }

    #[test]
    fn arabic_names_are_shown_joined_with_the_match_marked() {
        // "تقرير الميزانية.pdf": searching الميزانية must mark exactly those (joined) glyphs
        let segs = segments_visual("تقرير الميزانية.pdf", &n(&["الميزانية"]), 60);
        let joined: String = segs.iter().map(|(s, _)| s.as_str()).collect();
        assert!(joined.contains("pdf"));
        assert!(joined.chars().all(|c| !('\u{0627}'..='\u{064A}').contains(&c)), "no unjoined base letters should remain");
        let marked: String = segs.iter().filter(|(_, m)| *m).map(|(s, _)| s.as_str()).collect();
        assert_eq!(marked.chars().count(), 9);        // الميزانية: nine letters (no lam-alef pair)
        // latin names behave exactly as before
        assert_eq!(segments_visual("My Budget.xlsx", &n(&["budget"]), 40), segments("My Budget.xlsx", &n(&["budget"]), 40));
    }

    #[test]
    fn arabic_and_mixed_scripts_do_not_panic() {
        let segs = segments("تقرير الميزانية النهائي 2026 final report.pdf", &n(&["final", "تقرير"]), 24);
        assert!(segs.iter().any(|(_, m)| *m));
    }
}
