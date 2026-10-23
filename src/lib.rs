//! Spotlight-style search engine.
//!
//! | Spotlight        | here                                  |
//! |------------------|----------------------------------------|
//! | mds_stores       | [`store`]  (SQLite + FTS5 index)       |
//! | mdworker/.mdimporter | [`importer`] (per-type extractors) |
//! | mds crawl        | [`indexer`] (initial crawl)            |
//! | FSEvents         | [`watcher`] (live updates)             |
//! | mdfind / kMDItem | [`query`]  (query language)            |
pub mod importer;
pub mod indexer;
mod arabic_forms;
pub mod bidi;
pub mod blur;
pub mod categories;
pub mod highlight;
pub mod icons;
pub mod query;
pub mod rank;
pub mod layout;
pub mod look;
#[cfg(feature = "gui")]
pub mod placer;
pub mod service;
#[cfg(feature = "gui")]
pub mod settings_ui;
#[cfg(feature = "gui")]
pub mod setup_ui;
pub mod store;
pub mod watcher;
pub mod win;

/// Which build this is: the version, plus a stamp set when it was compiled (so "which one am I running?" has an answer).
pub fn build_label() -> String {
    format!("{} · build {}", env!("CARGO_PKG_VERSION"), option_env!("FSEARCH_BUILD").unwrap_or("dev"))
}
