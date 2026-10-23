//! Per-type content extractors (Spotlight's .mdimporter plug-ins).
use anyhow::{bail, Result};
use std::io::Read;
use std::path::Path;

pub trait Importer: Sync {
    fn accepts(&self, ext: &str) -> bool;
    fn extract(&self, path: &Path, max_bytes: u64) -> Result<String>;
}

struct PlainText;
struct Docx;

const TEXT_EXTS: &[&str] = &[
    "txt", "md", "markdown", "log", "csv", "json", "yaml", "yml", "toml", "xml", "html", "htm", "css", "ini", "cfg",
    "rs", "py", "js", "ts", "tsx", "jsx", "c", "h", "cpp", "hpp", "cs", "java", "go", "swift", "sh", "ps1", "bat", "sql",
];

impl Importer for PlainText {
    fn accepts(&self, ext: &str) -> bool { TEXT_EXTS.contains(&ext) }
    fn extract(&self, path: &Path, max_bytes: u64) -> Result<String> {
        let mut buf = Vec::new();
        std::fs::File::open(path)?.take(max_bytes).read_to_end(&mut buf)?;
        if buf.iter().take(4096).any(|&b| b == 0) { bail!("binary data"); }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }
}

impl Importer for Docx {
    fn accepts(&self, ext: &str) -> bool { ext == "docx" }
    fn extract(&self, path: &Path, max_bytes: u64) -> Result<String> {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        let mut xml = String::new();
        archive.by_name("word/document.xml")?.take(max_bytes * 4).read_to_string(&mut xml)?;
        Ok(strip_xml(&xml))
    }
}

/// Paragraph ends become newlines, tags are dropped, basic entities decoded.
fn strip_xml(xml: &str) -> String {
    let xml = xml.replace("</w:p>", "\n");
    let mut out = String::with_capacity(xml.len() / 2);
    let mut in_tag = false;
    for c in xml.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'")
}

static IMPORTERS: [&dyn Importer; 2] = [&PlainText, &Docx];

pub fn find(ext: &str) -> Option<&'static dyn Importer> {
    IMPORTERS.iter().copied().find(|i| i.accepts(ext))
}

#[cfg(test)]
mod tests {
    use super::strip_xml;
    #[test]
    fn strips_docx_xml() {
        assert_eq!(strip_xml("<w:p><w:r><w:t>Hello &amp; bye</w:t></w:r></w:p>"), "Hello & bye\n");
    }
}
