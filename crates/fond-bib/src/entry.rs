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

/// All authors as a single display string (`"Cone, James H., Doe, Jane"`), for indexing
/// and search. Empty if the entry has no authors.
pub fn author_names(entry: &HEntry) -> String {
    entry
        .authors()
        .map(|people| {
            people
                .iter()
                .map(|p| match &p.given_name {
                    Some(given) if !given.is_empty() => format!("{}, {given}", p.name),
                    _ => p.name.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
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

/// The subset of bibliographic fields the GUI's structured citation editor exposes. Values
/// are the human-facing strings shown in the form; `authors` is one `Family, Given` per line
/// (matching how the entry lists them). Everything else on the entry is left untouched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryFields {
    /// Hayagriva entry type, lowercased (e.g. `book`, `article`).
    pub entry_type: String,
    pub title: String,
    /// One author per line, each `Family, Given` (or just `Family`).
    pub authors: String,
    /// Publication year as free text (empty = no date).
    pub year: String,
    pub publisher: String,
    pub doi: String,
    pub isbn: String,
}

/// One author rendered as the `Family, Given` line the form shows (given name optional).
fn person_line(p: &hayagriva::types::Person) -> String {
    match &p.given_name {
        Some(given) if !given.is_empty() => format!("{}, {given}", p.name),
        _ => p.name.clone(),
    }
}

/// Read the editable fields out of an entry, for populating the structured editor.
pub fn read_fields(entry: &HEntry) -> EntryFields {
    EntryFields {
        entry_type: format!("{:?}", entry.entry_type()).to_lowercase(),
        title: title_string(entry).unwrap_or_default(),
        authors: entry
            .authors()
            .map(|people| {
                people
                    .iter()
                    .map(person_line)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),
        year: entry.date().map(|d| d.year.to_string()).unwrap_or_default(),
        publisher: entry
            .publisher()
            .and_then(|p| p.name())
            .map(|name| name.value.to_string())
            .unwrap_or_default(),
        doi: entry.doi().unwrap_or_default().to_string(),
        isbn: entry.isbn().unwrap_or_default().to_string(),
    }
}

/// Apply the structured editor's `edited` fields onto an entry's canonical `original_yaml`,
/// preserving every field the form doesn't manage. Only keys whose value actually changed
/// (vs. `current`, the fields as they were read from the same entry) are rewritten — so a
/// publisher carrying a location, or a full ISO date, survives an edit that didn't touch it.
/// Returns YAML text still keyed to the entry's own key; the caller validates it with a parse
/// round-trip before writing.
pub fn apply_fields_to_yaml(
    original_yaml: &str,
    current: &EntryFields,
    edited: &EntryFields,
) -> Result<String> {
    use serde_yaml_ng::Value;

    let mut doc: Value = serde_yaml_ng::from_str(original_yaml).map_err(|e| BibError::Yaml {
        path: Path::new("<entry>").to_path_buf(),
        message: e.to_string(),
    })?;

    // The document is a one-key mapping (`<key>: { fields }`). Grab that inner mapping.
    let inner = doc
        .as_mapping_mut()
        .and_then(|m| m.values_mut().next())
        .and_then(|v| v.as_mapping_mut())
        .ok_or_else(|| BibError::Yaml {
            path: Path::new("<entry>").to_path_buf(),
            message: "entry YAML is not a single keyed mapping".to_string(),
        })?;

    let key_of = |s: &str| Value::String(s.to_string());

    // type — always present; write the (non-empty) edited type when it changed.
    if edited.entry_type != current.entry_type && !edited.entry_type.trim().is_empty() {
        inner.insert(key_of("type"), key_of(edited.entry_type.trim()));
    }

    // Simple scalar-or-remove fields.
    if edited.title != current.title {
        set_or_remove(inner, "title", edited.title.trim());
    }
    if edited.publisher != current.publisher {
        set_or_remove(inner, "publisher", edited.publisher.trim());
    }

    // author — a YAML sequence of "Family, Given" lines, or removed if empty.
    if edited.authors != current.authors {
        let lines: Vec<Value> = edited
            .authors
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(key_of)
            .collect();
        if lines.is_empty() {
            inner.remove(key_of("author"));
        } else {
            inner.insert(key_of("author"), Value::Sequence(lines));
        }
    }

    // date — an integer year when it parses as one, else the raw string, else removed.
    if edited.year != current.year {
        let y = edited.year.trim();
        if y.is_empty() {
            inner.remove(key_of("date"));
        } else if let Ok(n) = y.parse::<i64>() {
            inner.insert(key_of("date"), Value::Number(n.into()));
        } else {
            inner.insert(key_of("date"), key_of(y));
        }
    }

    // doi / isbn live under `serial-number`. Mutate that sub-map in place, preserving any
    // other serial numbers, and drop it entirely if it ends up empty.
    if edited.doi != current.doi || edited.isbn != current.isbn {
        let sn_key = key_of("serial-number");
        let mut sn = match inner.remove(&sn_key) {
            Some(Value::Mapping(m)) => m,
            _ => serde_yaml_ng::Mapping::new(),
        };
        if edited.doi != current.doi {
            set_or_remove(&mut sn, "doi", edited.doi.trim());
        }
        if edited.isbn != current.isbn {
            set_or_remove(&mut sn, "isbn", edited.isbn.trim());
        }
        if !sn.is_empty() {
            inner.insert(sn_key, Value::Mapping(sn));
        }
    }

    serde_yaml_ng::to_string(&doc).map_err(|e| BibError::Yaml {
        path: Path::new("<entry>").to_path_buf(),
        message: e.to_string(),
    })
}

/// Set `map[key] = value` (as a string) when non-empty, else remove the key.
fn set_or_remove(map: &mut serde_yaml_ng::Mapping, key: &str, value: &str) {
    let k = serde_yaml_ng::Value::String(key.to_string());
    if value.is_empty() {
        map.remove(&k);
    } else {
        map.insert(k, serde_yaml_ng::Value::String(value.to_string()));
    }
}
