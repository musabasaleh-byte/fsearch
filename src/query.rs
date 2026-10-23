//! A small mdfind-like query language.
//!
//! ```text
//! report                      name OR content contains "report"
//! name:invoice ext:pdf        name contains invoice, extension pdf
//! kind:image size:>5mb        images over 5 MB
//! modified:>2026-01-01        changed after that date (also created:)
//! path:projects content:"fast search"
//! ```
use chrono::NaiveDate;
use rusqlite::types::Value;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op { Gt, Ge, Lt, Le, Eq }

impl Op {
    fn sql(self) -> &'static str {
        match self { Op::Gt => ">", Op::Ge => ">=", Op::Lt => "<", Op::Le => "<=", Op::Eq => "=" }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Query {
    pub terms: Vec<String>,
    pub names: Vec<String>,
    pub contents: Vec<String>,
    pub paths: Vec<String>,
    pub exts: Vec<String>,
    pub kinds: Vec<String>,
    pub not_kinds: Vec<String>,
    pub not_unders: Vec<String>,
    pub not_exts: Vec<String>,
    pub sizes: Vec<(Op, i64)>,
    pub modified: Vec<(Op, i64)>,
    pub created: Vec<(Op, i64)>,
}

pub fn kind_for(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "pdf" => "PDF Document",
        "doc" | "docx" | "rtf" | "odt" => "Word Document",
        "xls" | "xlsx" | "csv" | "ods" => "Spreadsheet",
        "ppt" | "pptx" | "odp" => "Presentation",
        "txt" | "md" | "markdown" | "log" => "Text",
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "heic" | "tif" | "tiff" | "svg" => "Image",
        "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" => "Audio",
        "mp4" | "mov" | "mkv" | "avi" | "webm" => "Movie",
        "zip" | "7z" | "rar" | "tar" | "gz" | "dmg" | "iso" => "Archive",
        "exe" | "msi" | "dll" | "app" | "lnk" | "uwp" => "Application",
        "ttf" | "otf" | "woff" | "woff2" => "Font",
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "h" | "cpp" | "hpp" | "cs" | "java" | "go" | "swift" | "sh" | "ps1" | "bat" | "sql" | "html" | "css" | "json" | "yaml" | "yml" | "toml" | "xml" => "Source Code",
        _ => "Document",
    }
}

fn tokenize(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in input.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() { out.push(std::mem::take(&mut cur)); }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() { out.push(cur); }
    out
}

fn split_op(v: &str) -> (Op, &str) {
    for (p, op) in [(">=", Op::Ge), ("<=", Op::Le), (">", Op::Gt), ("<", Op::Lt), ("=", Op::Eq)] {
        if let Some(rest) = v.strip_prefix(p) { return (op, rest); }
    }
    (Op::Eq, v)
}

pub fn parse_size(s: &str) -> Option<i64> {
    let s = s.trim().to_ascii_lowercase();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let mult = match unit.trim() {
        "" | "b" => 1.0,
        "k" | "kb" => 1e3,
        "m" | "mb" => 1e6,
        "g" | "gb" => 1e9,
        "t" | "tb" => 1e12,
        _ => return None,
    };
    Some((n * mult) as i64)
}

pub fn parse_date(s: &str) -> Option<i64> {
    let d = NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()?;
    Some(d.and_hms_opt(0, 0, 0)?.and_utc().timestamp())
}

impl Query {
    pub fn parse(input: &str) -> Query {
        let mut q = Query::default();
        for token in tokenize(input) {
            // -kind:Image  -under:C:\Private  -ext:tmp   (remove matches)
            if let Some(rest) = token.strip_prefix('-') {
                if let Some((key, value)) = rest.split_once(':') {
                    if !value.is_empty() {
                        match key.to_ascii_lowercase().as_str() {
                            "kind" => { q.not_kinds.push(value.to_string()); continue; }
                            "under" => { q.not_unders.push(value.to_string()); continue; }
                            "ext" => { q.not_exts.extend(value.split(',').map(|e| e.trim_start_matches('.').to_ascii_lowercase())); continue; }
                            _ => {}
                        }
                    }
                }
            }
            let Some((key, value)) = token.split_once(':') else {
                q.terms.push(token);
                continue;
            };
            let value = value.to_string();
            if value.is_empty() { q.terms.push(token); continue; }
            match key.to_ascii_lowercase().as_str() {
                "name" => q.names.push(value),
                "content" | "text" => q.contents.push(value),
                "path" | "in" => q.paths.push(value),
                "ext" => q.exts.extend(value.split(',').map(|e| e.trim_start_matches('.').to_ascii_lowercase())),
                "kind" => q.kinds.push(value.to_ascii_lowercase()),
                "size" => {
                    let (op, rest) = split_op(&value);
                    match parse_size(rest) { Some(n) => q.sizes.push((op, n)), None => q.terms.push(token.clone()) }
                }
                "modified" | "created" => {
                    let (op, rest) = split_op(&value);
                    match parse_date(rest) {
                        Some(t) if key.eq_ignore_ascii_case("modified") => q.modified.push((op, t)),
                        Some(t) => q.created.push((op, t)),
                        None => q.terms.push(token.clone()),
                    }
                }
                _ => q.terms.push(token),
            }
        }
        q
    }

    fn fts_phrase(s: &str) -> String {
        format!("\"{}\"*", s.replace('"', "\"\""))
    }

    fn escape_like(s: &str) -> String {
        s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
    }

    fn like(s: &str) -> String {
        format!("%{}%", Self::escape_like(s))
    }

    /// Name contains `t`. Fragments of 3+ characters use the trigram index; shorter ones fall back to a scan.
    fn name_condition(t: &str, args: &mut Vec<Value>) -> String {
        if t.chars().count() >= 3 {
            args.push(Value::Text(format!("\"{}\"", t.replace('"', "\"\""))));
            "f.id IN (SELECT rowid FROM name_tri WHERE name_tri MATCH ?)".to_string()
        } else {
            args.push(Value::Text(Self::like(t)));
            "f.name LIKE ? ESCAPE '\\'".to_string()
        }
    }

    fn conditions(&self) -> (String, Vec<Value>) {
        let mut wh: Vec<String> = Vec::new();
        let mut args: Vec<Value> = Vec::new();
        let text = |s: &String| Value::Text(s.clone());

        for t in &self.terms {
            let mut name_args = Vec::new();
            let name_sql = Self::name_condition(t, &mut name_args);
            if t.chars().count() >= 4 {
                wh.push(format!("({name_sql} OR f.id IN (SELECT rowid FROM content_fts WHERE content_fts MATCH ?))"));
                args.extend(name_args);
                args.push(Value::Text(Self::fts_phrase(t)));
            } else {
                // Fragments of 1-3 letters (typing in progress) match names only: as word prefixes inside documents they
                // would match thousands of unrelated files ("exc" -> except, exceed, exclude ...).
                wh.push(name_sql);
                args.extend(name_args);
            }
        }
        for n in &self.names {
            let sql = Self::name_condition(n, &mut args);
            wh.push(sql);
        }
        for c in &self.contents {
            wh.push("f.id IN (SELECT rowid FROM content_fts WHERE content_fts MATCH ?)".into());
            args.push(Value::Text(Self::fts_phrase(c)));
        }
        for p in &self.paths { wh.push("f.path LIKE ? ESCAPE '\\'".into()); args.push(Value::Text(Self::like(p))); }
        if !self.exts.is_empty() {
            wh.push(format!("f.ext IN ({})", vec!["?"; self.exts.len()].join(",")));
            args.extend(self.exts.iter().map(text));
        }
        for k in &self.kinds {
            wh.push("(f.kind LIKE ? ESCAPE '\\' OR f.ext = ?)".into());
            args.push(Value::Text(Self::like(k)));
            args.push(Value::Text(k.clone()));
        }
        for k in &self.not_kinds { wh.push("lower(f.kind) <> lower(?)".into()); args.push(Value::Text(k.clone())); }
        for u in &self.not_unders {
            // the folder itself and everything below it, but not siblings that merely share a prefix
            let base = u.trim_end_matches(['\\', '/']);
            wh.push("(f.path <> ? AND f.path NOT LIKE ? ESCAPE '\\' AND f.path NOT LIKE ? ESCAPE '\\')".into());
            args.push(Value::Text(base.to_string()));
            args.push(Value::Text(format!("{}\\\\%", Self::escape_like(base))));   // an escaped backslash, then %
            args.push(Value::Text(format!("{}/%", Self::escape_like(base))));
        }
        if !self.not_exts.is_empty() {
            wh.push(format!("f.ext NOT IN ({})", vec!["?"; self.not_exts.len()].join(",")));
            args.extend(self.not_exts.iter().map(text));
        }
        // Query dates are Unix seconds; the index stores milliseconds.
        for (col, list, scale) in [("f.size", &self.sizes, 1), ("f.mtime", &self.modified, 1000), ("f.ctime", &self.created, 1000)] {
            for (op, n) in list { wh.push(format!("{col} {} ?", op.sql())); args.push(Value::Integer(*n * scale)); }
        }
        (if wh.is_empty() { "1".to_string() } else { wh.join(" AND ") }, args)
    }

    pub fn to_sql(&self, limit: usize) -> (String, Vec<Value>) {
        let (clause, args) = self.conditions();
        (format!("SELECT f.path, f.name, f.kind, f.size, f.mtime FROM files f WHERE {clause} ORDER BY f.mtime DESC LIMIT {limit}"), args)
    }

    pub fn to_count_sql(&self) -> (String, Vec<Value>) {
        let (clause, args) = self.conditions();
        (format!("SELECT COUNT(*) FROM files f WHERE {clause}"), args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_operators_and_quotes() {
        let q = Query::parse(r#"report name:inv ext:PDF,.docx size:>5mb modified:>=2026-01-01 content:"fast search""#);
        assert_eq!(q.terms, vec!["report"]);
        assert_eq!(q.names, vec!["inv"]);
        assert_eq!(q.exts, vec!["pdf", "docx"]);
        assert_eq!(q.sizes, vec![(Op::Gt, 5_000_000)]);
        assert_eq!(q.modified, vec![(Op::Ge, 1_767_225_600)]);
        assert_eq!(q.contents, vec!["fast search"]);
    }

    #[test]
    fn parses_exclusions() {
        let q = Query::parse(r#"report -kind:"PDF Document" -under:"C:\My Docs" -ext:tmp,.bak -foo -x:"#);
        assert_eq!(q.terms, vec!["report", "-foo", "-x:"]);
        assert_eq!(q.not_kinds, vec!["PDF Document"]);
        assert_eq!(q.not_unders, vec![r"C:\My Docs"]);
        assert_eq!(q.not_exts, vec!["tmp", "bak"]);
    }

    #[test]
    fn bad_values_fall_back_to_plain_terms() {
        let q = Query::parse("size:big modified:soon http://x");
        assert_eq!(q.terms.len(), 3);
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("1.5kb"), Some(1500));
        assert_eq!(parse_size("2GB"), Some(2_000_000_000));
        assert_eq!(parse_size("12"), Some(12));
        assert_eq!(parse_size("5xb"), None);
    }
}
