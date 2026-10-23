use fsearch::indexer::{crawl, index_file, Options};
use fsearch::query::Query;
use fsearch::store::Store;
use std::fs;
use std::path::PathBuf;

fn tempdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fsearch-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn names(store: &Store, q: &str) -> Vec<String> {
    let mut v: Vec<String> = store.search(&Query::parse(q), 100).unwrap().into_iter().map(|h| h.name).collect();
    v.sort();
    v
}

#[test]
fn crawl_search_update_delete() {
    let root = tempdir("e2e");
    fs::write(root.join("Quarterly Report.txt"), "revenue grew in the third quarter").unwrap();
    fs::write(root.join("notes.md"), "fast search design with inverted index").unwrap();
    fs::write(root.join("photo.png"), vec![0u8; 3000]).unwrap();
    fs::create_dir_all(root.join("node_modules/x")).unwrap();
    fs::write(root.join("node_modules/x/ignored.txt"), "revenue").unwrap();
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("sub/deep.txt"), "revenue deep").unwrap();

    let store = Store::open_in_memory().unwrap();
    let opts = Options { content: true, ..Options::default() };
    let s = crawl(&store, &root, &opts, |_| {}).unwrap();
    assert_eq!(s.indexed, 4);                       // node_modules excluded

    assert_eq!(names(&store, "report"), vec!["Quarterly Report.txt"]);               // name match
    assert_eq!(names(&store, "revenue"), vec!["Quarterly Report.txt", "deep.txt"]);  // content match
    assert_eq!(names(&store, "content:\"inverted index\""), vec!["notes.md"]);
    assert_eq!(names(&store, "kind:image"), vec!["photo.png"]);
    assert_eq!(names(&store, "ext:md,png"), vec!["notes.md", "photo.png"]);
    assert_eq!(names(&store, "size:>2kb"), vec!["photo.png"]);
    assert_eq!(names(&store, "path:sub revenue"), vec!["deep.txt"]);
    assert_eq!(names(&store, "modified:>2000-01-01").len(), 4);
    assert!(names(&store, "modified:<2000-01-01").is_empty());

    // unchanged files are skipped on the next crawl
    let again = crawl(&store, &root, &opts, |_| {}).unwrap();
    assert_eq!((again.indexed, again.skipped), (0, 4));

    // content edits are re-indexed; deletions are removed
    fs::write(root.join("notes.md"), "completely different text about zebras").unwrap();
    let opts = Options { base: Some(root.clone()), ..opts.clone() };
    assert!(index_file(&store, &root.join("notes.md"), &opts).unwrap());
    assert_eq!(names(&store, "zebras"), vec!["notes.md"]);
    assert!(names(&store, "content:inverted").is_empty());

    fs::remove_file(root.join("photo.png")).unwrap();
    fs::remove_dir_all(root.join("sub")).unwrap();
    let after = crawl(&store, &root, &opts, |_| {}).unwrap();
    assert_eq!(after.removed, 2);
    assert!(names(&store, "deep").is_empty());

    // special characters must not break the query
    assert!(names(&store, "100%_done \"quote").is_empty());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn docx_content_is_searchable() {
    use std::io::Write;
    let root = tempdir("docx");
    let file = fs::File::create(root.join("contract.docx")).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file("word/document.xml", zip::write::SimpleFileOptions::default()).unwrap();
    zip.write_all(b"<w:document><w:body><w:p><w:r><w:t>indemnification clause</w:t></w:r></w:p></w:body></w:document>").unwrap();
    zip.finish().unwrap();

    let store = Store::open_in_memory().unwrap();
    crawl(&store, &root, &Options { content: true, ..Options::default() }, |_| {}).unwrap();
    assert_eq!(names(&store, "indemnification"), vec!["contract.docx"]);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn name_fragments_use_trigram_and_short_terms_fall_back() {
    let root = tempdir("tri");
    fs::write(root.join("Quarterly Report.txt"), "x").unwrap();
    fs::write(root.join("ab.txt"), "x").unwrap();
    fs::write(root.join("data_final(2).csv"), "x").unwrap();
    let store = Store::open_in_memory().unwrap();
    crawl(&store, &root, &Options::default(), |_| {}).unwrap();
    assert_eq!(names(&store, "uart"), vec!["Quarterly Report.txt"]);        // middle of a word, 3+ chars
    assert_eq!(names(&store, "QUARTERLY"), vec!["Quarterly Report.txt"]);   // case-insensitive
    assert_eq!(names(&store, "name:ab"), vec!["ab.txt"]);                   // 2 chars: LIKE fallback
    assert_eq!(names(&store, "final(2)"), vec!["data_final(2).csv"]);       // punctuation is literal
    assert_eq!(names(&store, "_final"), vec!["data_final(2).csv"]);
    assert_eq!(store.count(&Query::parse("ext:txt")).unwrap(), 2);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn roots_are_stored_and_old_v1_index_is_migrated() {
    let root = tempdir("mig");
    let db = root.join("old.db");
    {   // a database as written by the first release
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch("
            CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, name TEXT NOT NULL, ext TEXT NOT NULL,
                kind TEXT NOT NULL, size INTEGER NOT NULL, mtime INTEGER NOT NULL, ctime INTEGER NOT NULL, content_indexed INTEGER NOT NULL DEFAULT 0);
            CREATE VIRTUAL TABLE search_fts USING fts5(name, content);
            INSERT INTO files VALUES (1,'/a/Budget Plan.xlsx','Budget Plan.xlsx','xlsx','Spreadsheet',10,1,1,1);
            INSERT INTO search_fts(rowid,name,content) VALUES (1,'Budget Plan.xlsx','old text');").unwrap();
    }
    let store = Store::open(&db).unwrap();
    assert_eq!(names(&store, "dget pla"), vec!["Budget Plan.xlsx"]);   // trigram rebuilt from existing rows
    assert!(names(&store, "content:old").is_empty());                  // content is re-extracted on next --content run
    store.add_root("/a", true).unwrap();
    assert_eq!(store.roots().unwrap(), vec![("/a".to_string(), true)]);
    assert!(store.remove_root("/a").unwrap());
    drop(store);
    assert!(Store::open(&db).is_ok());                                  // reopening the migrated DB is a no-op
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn hidden_categories_and_private_folders_are_excluded() {
    let root = tempdir("priv");
    fs::create_dir_all(root.join("Private")).unwrap();
    fs::create_dir_all(root.join("PrivateStuff")).unwrap();
    fs::write(root.join("Private/secret plan.pdf"), "x").unwrap();
    fs::write(root.join("PrivateStuff/plan notes.pdf"), "x").unwrap();
    fs::write(root.join("plan.png"), "x").unwrap();
    fs::write(root.join("plan.txt"), "x").unwrap();
    let store = Store::open_in_memory().unwrap();
    crawl(&store, &root, &Options::default(), |_| {}).unwrap();
    let private = root.join("Private").to_string_lossy().into_owned();

    assert_eq!(names(&store, "plan").len(), 4);
    assert_eq!(names(&store, "plan -kind:Image"), vec!["plan notes.pdf", "plan.txt", "secret plan.pdf"]);
    assert_eq!(names(&store, "plan -kind:\"PDF Document\" -kind:image"), vec!["plan.txt"]);
    // the private folder is hidden, the sibling that only shares the prefix is not
    assert_eq!(names(&store, &format!("plan -under:\"{private}\"")), vec!["plan notes.pdf", "plan.png", "plan.txt"]);
    assert_eq!(names(&store, "plan -ext:png,txt"), vec!["plan notes.pdf", "secret plan.pdf"]);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn nested_root_files_survive_the_parent_crawl_and_the_name_index_can_be_repaired() {
    let home = tempdir("nested");
    fs::create_dir_all(home.join("AppData/Roaming/Start Menu/Programs")).unwrap();
    fs::write(home.join("AppData/Roaming/Start Menu/Programs/Excel.lnk"), "x").unwrap();
    fs::write(home.join("notes.txt"), "hello").unwrap();
    let store = Store::open_in_memory().unwrap();
    let programs = home.join("AppData/Roaming/Start Menu/Programs");
    crawl(&store, &programs, &Options::default(), |_| {}).unwrap();
    // The parent crawl does not descend into AppData; without the nested-root rule it would delete Excel.lnk.
    let parent = Options { nested_roots: vec![programs.to_string_lossy().into_owned()], ..Options::default() };
    crawl(&store, &home, &parent, |_| {}).unwrap();
    assert_eq!(names(&store, "name:excel"), vec!["Excel.lnk"]);

    // Simulate a name index that lost an entry.
    store.conn_for_tests().execute("DELETE FROM name_tri", []).unwrap();
    assert!(names(&store, "name:excel").is_empty());
    let (added, _) = store.repair_name_index().unwrap();
    assert!(added >= 1);
    assert_eq!(names(&store, "name:excel"), vec!["Excel.lnk"]);
}

#[test]
fn a_folder_that_itself_lives_under_appdata_is_still_indexed() {
    // Exclusions apply below the indexed folder only. (CI runners keep their temp folder under AppData.)
    let root = tempdir("appdata").join("AppData").join("Local").join(".hidden-parent").join("work");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::create_dir_all(root.join(".cache")).unwrap();
    fs::write(root.join("sub/report.txt"), "x").unwrap();
    fs::write(root.join(".cache/junk.txt"), "x").unwrap();
    let store = Store::open_in_memory().unwrap();
    crawl(&store, &root, &Options::default(), |_| {}).unwrap();
    assert_eq!(names(&store, "name:report"), vec!["report.txt"]);   // found although the path has AppData and a dot folder above it
    assert!(names(&store, "name:junk").is_empty());                 // but a dot folder inside it is skipped
}

#[test]
fn under_exclusion_works_with_windows_backslash_paths() {
    use fsearch::store::FileRecord;
    let store = Store::open_in_memory().unwrap();
    let rec = |path: &str, name: &str| (FileRecord { path: path.into(), name: name.into(), ext: "pdf".into(), kind: "PDF Document".into(), size: 1, mtime: 1, ctime: 1 }, None);
    store.upsert_batch(&[
        rec(r"C:\Users\me\Private\secret plan.pdf", "secret plan.pdf"),
        rec(r"C:\Users\me\Private\deep\more plan.pdf", "more plan.pdf"),
        rec(r"C:\Users\me\PrivateStuff\plan notes.pdf", "plan notes.pdf"),
    ]).unwrap();
    let q = r#"plan -under:"C:\Users\me\Private""#;
    assert_eq!(names(&store, q), vec!["plan notes.pdf"]);          // the folder and what is below it are hidden; a sibling sharing the prefix is not
    assert_eq!(names(&store, r#"plan -under:"C:\Users\me\Private\""#), vec!["plan notes.pdf"]);   // trailing separator is fine too
}
