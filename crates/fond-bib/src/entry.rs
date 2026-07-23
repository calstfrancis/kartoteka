//! Thin wrapper over Hayagriva's `Entry`/`Library` for the one-key-per-file layout.
//! `entries/<key>.yml` files are pure Hayagriva; this module parses, validates the
//! single-key invariant, and re-serializes them canonically.

use std::path::Path;

use hayagriva::Entry as HEntry;
use hayagriva::Library as HLibrary;

use crate::error::{BibError, Result};

/// One entry parsed from an `entries/<key>.yml` file.
pub struct ParsedEntry {
    /// The top-level mapping key (the citation key) as found in the file.
    pub key: String,
    pub entry: HEntry,
}

/// Parse a file that must contain exactly one Hayagriva entry.
pub fn parse_single(text: &str, path: &Path) -> Result<ParsedEntry> {
    let lib = parse_library(text, path)?;
    match lib.len() {
        1 => {
            let entry = lib.nth(0).expect("len == 1").clone();
            Ok(ParsedEntry {
                key: entry.key().to_string(),
                entry,
            })
        }
        found => Err(BibError::NotSingleEntry {
            path: path.to_path_buf(),
            found,
        }),
    }
}

/// Parse a Hayagriva YAML document that may contain one or more entries (used by `add`,
/// where the incoming snippet's keys are placeholders to be regenerated).
pub fn parse_all(text: &str, path: &Path) -> Result<Vec<HEntry>> {
    let lib = parse_library(text, path)?;
    Ok(lib.iter().cloned().collect())
}

fn parse_library(text: &str, path: &Path) -> Result<HLibrary> {
    hayagriva::io::from_yaml_str(text).map_err(|e| BibError::Yaml {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

/// First author's family name, if the entry has authors.
pub fn family_name(entry: &HEntry) -> Option<String> {
    entry
        .authors()
        .and_then(|people| people.first())
        .map(|p| p.name.clone())
}

/// Publication year, if dated.
pub fn year(entry: &HEntry) -> Option<i32> {
    entry.date().map(|d| d.year)
}

/// Title as a plain string, if present.
pub fn title_string(entry: &HEntry) -> Option<String> {
    entry.title().map(|t| t.value.to_string())
}

/// Serialize a single entry to canonical Hayagriva YAML (`<key>:` + indented fields),
/// keeping the entry's own key.
pub fn serialize_entry(entry: &HEntry) -> Result<String> {
    let mut lib = HLibrary::new();
    lib.push(entry);
    hayagriva::io::to_yaml_str(&lib).map_err(|e| BibError::Yaml {
        path: Path::new("<entry>").to_path_buf(),
        message: e.to_string(),
    })
}

/// Serialize a single entry but under a different top-level key. Used when `add` assigns a
/// freshly generated key: the serialized form's first line is `<oldkey>:`, which is the
/// only place the key appears at column 0, so swapping that line re-keys the file safely.
pub fn serialize_entry_as(entry: &HEntry, new_key: &str) -> Result<String> {
    let yaml = serialize_entry(entry)?;
    Ok(match yaml.split_once('\n') {
        Some((_first_line, rest)) => format!("{new_key}:\n{rest}"),
        None => format!("{new_key}:\n"),
    })
}
