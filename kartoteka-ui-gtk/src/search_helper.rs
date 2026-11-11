//! `kartoteka-gtk --search-json QUERY…`: search the open library's index and print the hits as
//! one line of JSON, for the Porog launcher (`flatpak run --command=kartoteka-search …`).
//! No window, no keyring, no network and no writes.
//!
//! It only reads the index the app already keeps; it never builds one, because a first build
//! can take minutes. Failures are printed as `{"schema":1,"error":"…"}` with exit code 0,
//! because "no library yet" is an answer, not a crash.

use crate::config::Config;

/// Version of the JSON this prints. Bumped only for a change an older Porog cannot ignore.
const SCHEMA: u32 = 1;
/// Most hits one answer carries; Porog shows a handful.
const LIMIT: usize = 8;

pub fn run(query: &[String]) {
    println!("{}", answer(query));
}

fn answer(query: &[String]) -> String {
    match hits(query) {
        Ok(hits) => serde_json::json!({ "schema": SCHEMA, "hits": hits }).to_string(),
        Err(message) => serde_json::json!({ "schema": SCHEMA, "error": message }).to_string(),
    }
}

fn hits(query: &[String]) -> Result<Vec<serde_json::Value>, String> {
    let root = Config::load()
        .library_path
        .ok_or_else(|| "Kartoteka has no library yet.".to_string())?;
    let index_dir = root.join(".kartoteka").join("index");
    if !index_dir.exists() {
        return Err("Kartoteka has not indexed this library yet.".to_string());
    }
    let index = fond_index::SearchIndex::open(&index_dir)
        .map_err(|_| "Kartoteka's search index is out of date; open Kartoteka once.".to_string())?;
    let found = index
        .search(&query.join(" "), LIMIT)
        .map_err(|e| format!("Kartoteka could not search: {e}"))?;
    Ok(found
        .iter()
        .map(|h| {
            serde_json::json!({
                "key": h.key, "title": h.title, "author": h.author, "year": h.year,
            })
        })
        .collect())
}
