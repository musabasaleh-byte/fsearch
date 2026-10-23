//! Result ranking: how well the name matches, where the file lives, how recent it is, and how often
//! you open it from the launcher. Pure functions, so the behaviour is covered by unit tests.
use crate::store::Hit;
use std::collections::HashMap;

const DAY_MS: f64 = 86_400_000.0;

/// How well `name` matches one typed fragment (0 = it only matched inside the file's text).
fn name_score(name: &str, needle: &str) -> f64 {
    if needle.is_empty() { return 0.0; }
    let lower = name.to_lowercase();
    let stem = lower.rsplit_once('.').map(|(s, _)| s).unwrap_or(&lower);
    if stem == needle { return 100.0; }
    if lower.starts_with(needle) { return 70.0; }
    // A match that begins a word ("_arch", " arch", "-arch", or a camelCase hump) beats one inside a word.
    let chars: Vec<char> = name.chars().collect();
    let lower_chars: Vec<char> = lower.chars().collect();
    let n: Vec<char> = needle.chars().collect();
    let mut inside = false;
    for i in 0..lower_chars.len().saturating_sub(n.len() - 1) {
        if lower_chars[i..i + n.len()] != n[..] { continue; }
        let boundary = i == 0
            || !chars[i - 1].is_alphanumeric()
            || (chars[i - 1].is_lowercase() && chars[i].is_uppercase());
        if boundary { return 45.0; }
        inside = true;
    }
    if inside { 15.0 } else { 0.0 }
}

fn location_score(path: &str, home: &str) -> f64 {
    let p = path.to_lowercase().replace('/', "\\");
    // Start-menu shortcuts are the apps; they outrank ordinary files (and sit under ProgramData, which is otherwise noise).
    if p.contains("\\start menu\\programs") || p.starts_with("shell:appsfolder") { return 40.0; }
    const NOISE: [&str; 10] = ["\\program files", "\\windows\\", "\\appdata\\", "\\node_modules\\", "\\.git\\", "\\programdata\\", "\\$recycle.bin", "\\.cache\\", "\\site-packages\\", "\\.tmp\\"];
    if NOISE.iter().any(|n| p.contains(n)) { return -60.0; }
    // Hidden developer folders (.codex, .vscode, .claude, ...) are tool storage, not the user's files.
    if p.split('\\').skip(1).any(|c| c.len() > 1 && c.starts_with('.')) { return -50.0; }
    let h = home.to_lowercase().replace('/', "\\");
    if h.is_empty() || !p.starts_with(&h) { return 0.0; }
    // The folders people actually keep their work in.
    let personal = ["\\desktop\\", "\\documents\\", "\\downloads\\", "\\pictures\\", "\\onedrive"];
    if personal.iter().any(|d| p[h.len()..].contains(d)) { 18.0 } else { 8.0 }
}

fn type_score(name: &str) -> f64 {
    match name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).as_deref() {
        Some("pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "dwg" | "png" | "jpg" | "jpeg") => 5.0,
        Some("dll" | "sys" | "lib" | "pyc" | "tmp" | "log" | "pdb" | "obj" | "etl") => -15.0,
        Some("py" | "js" | "json" | "map" | "cfg" | "ini" | "toml" | "yml" | "yaml" | "rs" | "ts") => -8.0,
        _ => 0.0,
    }
}

/// True when the file matched only through its text, not through its name.
pub fn content_only(hit: &Hit, needles: &[String]) -> bool {
    !needles.is_empty() && needles.iter().all(|n| name_score(&hit.name, n) == 0.0)
}

/// `launch` is (times opened from the launcher, last opened at, in ms).
pub fn score(hit: &Hit, needles: &[String], home: &str, launch: Option<(i64, i64)>, now_ms: i64) -> f64 {
    let mut total = 0.0;
    if !needles.is_empty() {
        let parts: Vec<f64> = needles.iter().map(|n| name_score(&hit.name, n)).collect();
        total += parts.iter().sum::<f64>() / parts.len() as f64;
        if parts.iter().all(|s| *s >= 45.0) { total += 10.0; }
        if parts.iter().all(|s| *s == 0.0) { total -= 30.0; }     // found only inside the text: below name matches
    }
    let age_days = ((now_ms - hit.mtime) as f64 / DAY_MS).max(0.0);
    total += 20.0 * 0.5f64.powf(age_days / 30.0);
    total += location_score(&hit.path, home) + type_score(&hit.name);
    if let Some((count, last)) = launch {
        let since = ((now_ms - last) as f64 / DAY_MS).max(0.0);
        total += (12.0 * (1.0 + count as f64).ln()).min(40.0) + 15.0 * 0.5f64.powf(since / 14.0);
    }
    total
}

/// Spotlight-style grouping order: apps, then documents, images, media, other files, code.
pub fn group_rank(kind: &str) -> u8 {
    match kind {
        "Application" => 0,
        "Word Document" | "PDF Document" | "Presentation" | "Spreadsheet" | "Text" => 1,
        "Image" => 2,
        "Movie" | "Audio" => 3,
        "Source Code" => 5,
        _ => 4,
    }
}

/// Keeps the best match first, then lists the rest grouped by kind (each group still best-first); files that matched
/// only through their text come after all the files whose names match.
pub fn group_hits(hits: &mut [Hit], needles: &[String]) {
    if hits.len() > 2 { hits[1..].sort_by_key(|h| (content_only(h, needles), group_rank(&h.kind))); }
}

/// Sorts best-first. `needles` are the typed name fragments (lower-case), `launches` maps path -> (count, last).
pub fn rank_hits(hits: &mut [Hit], needles: &[String], home: &str, launches: &HashMap<String, (i64, i64)>, now_ms: i64) {
    let mut scored: Vec<(f64, usize)> = hits.iter().enumerate()
        .map(|(i, h)| (score(h, needles, home, launches.get(&h.path).copied(), now_ms), i)).collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1)));
    let order: Vec<Hit> = scored.iter().map(|(_, i)| hits[*i].clone()).collect();
    hits.clone_from_slice(&order);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(path: &str, age_days: f64) -> Hit {
        let now = 1_800_000_000_000i64;
        Hit { path: path.into(), name: path.rsplit(['\\', '/']).next().unwrap().into(), kind: "x".into(), size: 1, mtime: now - (age_days * DAY_MS) as i64 }
    }
    const NOW: i64 = 1_800_000_000_000;
    const HOME: &str = r"C:\Users\me";

    fn order(mut hits: Vec<Hit>, needle: &str, launches: HashMap<String, (i64, i64)>) -> Vec<String> {
        rank_hits(&mut hits, &[needle.to_string()], HOME, &launches, NOW);
        hits.into_iter().map(|h| h.name).collect()
    }

    #[test]
    fn word_start_beats_inside_a_word_even_if_older() {
        let hits = vec![hit(r"C:\Users\me\Desktop\fsearch.exe", 0.1), hit(r"C:\Users\me\Downloads\Create_a_sophisticated_archite.mp4", 60.0)];
        assert_eq!(order(hits, "arch", HashMap::new())[0], "Create_a_sophisticated_archite.mp4");
    }

    #[test]
    fn exact_then_prefix_then_word_then_inside() {
        let hits = vec![hit(r"C:\Users\me\a\fsearch.txt", 1.0), hit(r"C:\Users\me\a\my_arch_notes.txt", 1.0), hit(r"C:\Users\me\a\archive.txt", 1.0), hit(r"C:\Users\me\a\arch.txt", 1.0)];
        assert_eq!(order(hits, "arch", HashMap::new()), ["arch.txt", "archive.txt", "my_arch_notes.txt", "fsearch.txt"]);
    }

    #[test]
    fn camel_case_humps_count_as_word_starts() {
        assert_eq!(name_score("MyArchPlan.docx", "arch"), 45.0);
        assert_eq!(name_score("research.docx", "arch"), 15.0);
    }

    #[test]
    fn system_folders_sink_below_your_files() {
        let hits = vec![hit(r"C:\Program Files\Git\usr\bin\arch.exe", 1.0), hit(r"C:\Users\me\Documents\arch plan.pdf", 90.0)];
        assert_eq!(order(hits, "arch", HashMap::new())[0], "arch plan.pdf");
    }

    #[test]
    fn files_you_open_often_rise() {
        let a = hit(r"C:\Users\me\a\report_a.pdf", 30.0);
        let b = hit(r"C:\Users\me\a\report_b.pdf", 30.0);
        let mut launches = HashMap::new();
        launches.insert(b.path.clone(), (6, NOW - 2 * DAY_MS as i64));
        assert_eq!(order(vec![a, b], "report", launches)[0], "report_b.pdf");
    }

    #[test]
    fn start_menu_apps_beat_documents_with_the_same_name() {
        let hits = vec![hit(r"C:\Users\me\Documents\word notes.docx", 1.0), hit(r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Word.lnk", 200.0)];
        assert_eq!(order(hits, "word", HashMap::new())[0], "Word.lnk");
    }

    #[test]
    fn content_only_matches_still_rank_by_recency_and_location() {
        let hits = vec![hit(r"C:\Users\me\old.txt", 400.0), hit(r"C:\Users\me\new.txt", 1.0)];
        assert_eq!(order(hits, "zebra", HashMap::new())[0], "new.txt");
    }

    #[test]
    fn app_and_personal_files_beat_tool_storage_and_scripts() {
        let hits = vec![
            hit(r"C:\Users\me\.codex\tmp\plugins\excel_utils.py", 0.2),
            hit(r"C:\Users\me\.cache\runtime\site-packages\pandas\excel.py", 0.5),
            hit(r"C:\Users\me\Downloads\excel_import.xlsx", 90.0),
            hit(r"C:\Users\me\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Excel.lnk", 400.0),
        ];
        let names = order(hits, "excel", HashMap::new());
        assert_eq!(names[0], "Excel.lnk");
        assert_eq!(names[1], "excel_import.xlsx");
    }

    #[test]
    fn groups_keep_the_top_hit_then_order_by_kind() {
        let mk = |name: &str, kind: &str| Hit { path: format!(r"C:\x\{name}"), name: name.into(), kind: kind.into(), size: 1, mtime: 0 };
        let mut hits = vec![mk("a.png", "Image"), mk("b.py", "Source Code"), mk("c.pdf", "PDF Document"), mk("D.lnk", "Application")];
        group_hits(&mut hits, &[]);
        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["a.png", "D.lnk", "c.pdf", "b.py"]);
    }

    #[test]
    fn files_matching_only_through_their_text_come_after_name_matches() {
        let mk = |name: &str, kind: &str| Hit { path: format!(r"C:\Users\me\Documents\{name}"), name: name.into(), kind: kind.into(), size: 1, mtime: NOW - DAY_MS as i64 };
        let needles = vec!["excel".to_string()];
        let mut hits = vec![mk("Quarterly plan.docx", "Word Document"), mk("Notes.txt", "Text"), mk("excel_import.csv", "Spreadsheet"), mk("budget excel.png", "Image")];
        rank_hits(&mut hits, &needles, HOME, &HashMap::new(), NOW);
        group_hits(&mut hits, &needles);
        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert!(names[..2].contains(&"excel_import.csv") && names[..2].contains(&"budget excel.png"), "{names:?}");
    }
}
