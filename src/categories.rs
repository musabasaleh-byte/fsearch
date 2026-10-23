//! Search-result categories (macOS "Siri & Spotlight" list) mapped onto the kinds stored in the index.

pub const CATEGORIES: [(&str, &[&str]); 12] = [
    ("Applications", &["Application"]),
    ("Documents", &["Word Document", "Text"]),
    ("PDF Documents", &["PDF Document"]),
    ("Presentations", &["Presentation"]),
    ("Spreadsheets", &["Spreadsheet"]),
    ("Images", &["Image"]),
    ("Music", &["Audio"]),
    ("Movies", &["Movie"]),
    ("Source Code", &["Source Code"]),
    ("Archives", &["Archive"]),
    ("Fonts", &["Font"]),
    ("Other", &["Document"]),
];

/// Query text that removes the hidden categories and folders, e.g. ` -kind:"Image" -under:"C:\Private"`.
pub fn exclusions(hidden_categories: &[String], private_folders: &[String]) -> String {
    let mut out = String::new();
    for (label, kinds) in CATEGORIES {
        if hidden_categories.iter().any(|h| h == label) {
            for kind in kinds { out.push_str(&format!(" -kind:\"{kind}\"")); }
        }
    }
    for folder in private_folders.iter().filter(|f| !f.trim().is_empty()) {
        out.push_str(&format!(" -under:\"{}\"", folder.replace('"', "")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn builds_exclusions() {
        let text = exclusions(&["Images".into(), "Documents".into()], &[r"C:\Private".into(), "".into()]);
        assert_eq!(text, r#" -kind:"Word Document" -kind:"Text" -kind:"Image" -under:"C:\Private""#);
        assert_eq!(exclusions(&[], &[]), "");
        assert_eq!(exclusions(&["Nonsense".into()], &[]), "");
    }
}
