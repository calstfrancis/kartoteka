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

/// Which role a book's own author(s) play when embedded as a book-part's `parent:` block
/// (see `book_part_yaml`). The common case for a multi-contributor anthology is `Editor` —
/// whoever the book is catalogued under functions as the volume's editor once individual
/// chapters get their own entries; `Author` keeps them as the parent's author instead, for
/// a single- or co-authored book being split into named chapters/sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentRole {
    Editor,
    Author,
}

/// `source`'s own fields as a YAML mapping suitable for embedding as another entry's
/// `parent:` block, with `author` renamed to `editor` when `role` is `Editor`.
fn parent_block(source: &HEntry, role: ParentRole) -> Result<serde_yaml_ng::Value> {
    use serde_yaml_ng::Value;

    let text = serialize_entry(source)?;
    let doc: Value = serde_yaml_ng::from_str(&text).map_err(|e| BibError::Yaml {
        path: Path::new("<entry>").to_path_buf(),
        message: e.to_string(),
    })?;
    let mut inner = doc
        .as_mapping()
        .and_then(|m| m.values().next())
        .cloned()
        .unwrap_or_else(|| Value::Mapping(Default::default()));
    if role == ParentRole::Editor {
        if let Some(map) = inner.as_mapping_mut() {
            if let Some(author) = map.remove(Value::String("author".to_string())) {
                map.insert(Value::String("editor".to_string()), author);
            }
        }
    }
    Ok(inner)
}

/// Build a new "book part" (chapter/section) entry's YAML from an existing book/anthology
/// `source`: `source`'s own fields become the new entry's `parent:` block (see
/// `parent_block`), so the part cites correctly ("In: Editor (Ed.), Book Title…") without
/// duplicating data the source entry already owns — unlike copy-and-edit workflows (e.g.
/// Zotero's "Create Book Section From Item"), where the two entries start drifting apart the
/// moment either is edited afterward; see `refresh_book_part_parent` for pulling the source's
/// latest fields back in later. `part_type` is normally `"chapter"`. Returns YAML under a
/// placeholder key; the caller re-keys it (`Library::add_from_yaml` already generates a
/// fresh key from title/author when it sees one).
pub fn book_part_yaml(
    source: &HEntry,
    role: ParentRole,
    part_type: &str,
    title: &str,
    authors: &str,
    pages: &str,
) -> Result<String> {
    use serde_yaml_ng::Value;

    let mut fields = serde_yaml_ng::Mapping::new();
    fields.insert(
        Value::String("type".to_string()),
        Value::String(part_type.to_string()),
    );
    if !title.trim().is_empty() {
        fields.insert(
            Value::String("title".to_string()),
            Value::String(title.trim().to_string()),
        );
    }
    let names: Vec<Value> = authors
        .split(['\n', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Value::String(s.to_string()))
        .collect();
    if !names.is_empty() {
        fields.insert(Value::String("author".to_string()), Value::Sequence(names));
    }
    if !pages.trim().is_empty() {
        fields.insert(
            Value::String("page-range".to_string()),
            Value::String(pages.trim().to_string()),
        );
    }
    fields.insert(
        Value::String("parent".to_string()),
        parent_block(source, role)?,
    );

    let mut outer = serde_yaml_ng::Mapping::new();
    outer.insert(
        Value::String("new-item".to_string()),
        Value::Mapping(fields),
    );
    serde_yaml_ng::to_string(&Value::Mapping(outer)).map_err(|e| BibError::Yaml {
        path: Path::new("<book-part>").to_path_buf(),
        message: e.to_string(),
    })
}

/// Re-derive `existing_yaml`'s `parent:` block from `source`'s *current* fields, leaving
/// every other field (the part's own title/author/pages/…) untouched — "Refresh from source
/// book", for when the source book's own metadata changes after a part was created from it.
/// Returns YAML still keyed to the entry's own key; the caller validates it with a parse
/// round-trip before writing (same as `apply_fields_to_yaml`).
pub fn refresh_book_part_parent(
    existing_yaml: &str,
    source: &HEntry,
    role: ParentRole,
) -> Result<String> {
    use serde_yaml_ng::Value;

    let mut doc: Value = serde_yaml_ng::from_str(existing_yaml).map_err(|e| BibError::Yaml {
        path: Path::new("<entry>").to_path_buf(),
        message: e.to_string(),
    })?;
    let inner = doc
        .as_mapping_mut()
        .and_then(|m| m.values_mut().next())
        .and_then(|v| v.as_mapping_mut())
        .ok_or_else(|| BibError::Yaml {
            path: Path::new("<entry>").to_path_buf(),
            message: "entry YAML is not a single keyed mapping".to_string(),
        })?;
    inner.insert(
        Value::String("parent".to_string()),
        parent_block(source, role)?,
    );
    serde_yaml_ng::to_string(&doc).map_err(|e| BibError::Yaml {
        path: Path::new("<entry>").to_path_buf(),
        message: e.to_string(),
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

#[cfg(test)]
mod book_part_tests {
    use super::*;

    fn anthology() -> HEntry {
        let yaml = "the-book:\n  type: book\n  title: Essays on Being\n  author:\n    - Doe, Jane\n  date: 1985\n  publisher: Big Press\n";
        parse_single(yaml, Path::new("the-book.yml")).unwrap().entry
    }

    #[test]
    fn book_part_yaml_embeds_source_as_editor_parent() {
        let book = anthology();
        let yaml = book_part_yaml(
            &book,
            ParentRole::Editor,
            "chapter",
            "On Personhood",
            "Smith, John",
            "45-67",
        )
        .unwrap();
        let parsed = parse_single(&yaml, Path::new("new-item.yml")).unwrap();
        assert_eq!(
            title_string(&parsed.entry).as_deref(),
            Some("On Personhood")
        );
        assert_eq!(
            parsed.entry.page_range().map(|r| r.to_string()),
            Some("45-67".to_string())
        );
        let parent = parsed.entry.parents().first().expect("has a parent");
        assert_eq!(title_string(parent).as_deref(), Some("Essays on Being"));
        assert!(
            parent.editors().is_some(),
            "source author should become parent editor"
        );
        assert!(
            parent.authors().is_none(),
            "author should be moved, not duplicated"
        );
    }

    #[test]
    fn refresh_pulls_in_a_retitled_source() {
        let mut book = anthology();
        let original = book_part_yaml(
            &book,
            ParentRole::Editor,
            "chapter",
            "On Personhood",
            "Smith, John",
            "",
        )
        .unwrap();
        let added = parse_single(&original, Path::new("new-item.yml")).unwrap();

        book.set_title("Essays on Being, Revised".to_string().into());
        let refreshed = refresh_book_part_parent(
            &serialize_entry_as(&added.entry, &added.key).unwrap(),
            &book,
            ParentRole::Editor,
        )
        .unwrap();
        let reparsed = parse_single(&refreshed, Path::new("new-item.yml")).unwrap();
        assert_eq!(
            title_string(&reparsed.entry).as_deref(),
            Some("On Personhood")
        );
        let parent = reparsed.entry.parents().first().expect("has a parent");
        assert_eq!(
            title_string(parent).as_deref(),
            Some("Essays on Being, Revised")
        );
    }
}
