//! Thin wrapper over Hayagriva's `Entry`/`Library` for the one-key-per-file layout.
//! `entries/<key>.yml` files are pure Hayagriva; this module parses, validates the
//! single-key invariant, and re-serializes them canonically.

use std::path::Path;

use hayagriva::Entry as HEntry;
use hayagriva::Library as HLibrary;

use crate::creator::{self, Creator, CreatorRole};
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

/// Parse a BibLaTeX/BibTeX snippet into entries (keys as written), without adding them.
pub fn parse_bibtex(source: &str) -> Result<Vec<HEntry>> {
    let library = hayagriva::io::from_biblatex_str(source).map_err(|errors| {
        let joined = errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        BibError::Import {
            message: format!("could not parse BibLaTeX: {joined}"),
        }
    })?;
    Ok(library.iter().cloned().collect())
}

fn parse_library(text: &str, path: &Path) -> Result<HLibrary> {
    hayagriva::io::from_yaml_str(text).map_err(|e| BibError::Yaml {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

/// The sort/citation-key family name: the first author, falling back to the first editor
/// and then the first other creator when the entry has no plain author — see
/// [`creator::sort_family_name`].
pub fn family_name(entry: &HEntry) -> Option<String> {
    creator::sort_family_name(entry)
}

/// Publication year, if dated.
///
/// Falls back to a parent's date (`date_any`, the same lookup CSL's `issued` uses): a book
/// part keeps its book's date only inside `parent:`, and reading just the entry's own date
/// made every chapter's generated key contain `nodate` and its duplicate-detection year empty
/// even though citations rendered the parent's year fine. (The editable Year *field* still
/// reads the entry's own date — see [`read_fields`] — so editing never copies a parent's year
/// onto the child.)
pub fn year(entry: &HEntry) -> Option<i32> {
    entry.date_any().map(|d| d.year)
}

/// Whether an entry of this (lowercased) type carries its top-level `location:` as a place of
/// *publication* that the editor should treat as the publisher location. A conference,
/// exhibition or misc entry uses that field for the event's place (CSL `event-place`) — real
/// data that must not be read into, or removed by, the publisher-location editor.
fn top_level_location_is_publication_place(entry_type: &str) -> bool {
    !matches!(entry_type, "conference" | "exhibition" | "misc")
}

/// True when the entry's place of publication is only a top-level `location:` (the shape older
/// versions wrote), so no citation style can see it. Never true for a conference/exhibition/
/// misc entry, whose top-level `location:` is legitimately the event place.
pub fn has_legacy_location(entry: &HEntry) -> bool {
    entry.location().is_some()
        && entry.publisher().and_then(|p| p.location()).is_none()
        && top_level_location_is_publication_place(
            &format!("{:?}", entry.entry_type()).to_lowercase(),
        )
}

/// All of the entry's "primary" creators (see [`creator::sort_family_name`]) as a single
/// display string (`"Cone, James H., Doe, Jane"`), for indexing and search. Empty if the
/// entry has no creators at all.
pub fn author_names(entry: &HEntry) -> String {
    creator::display_names(entry)
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

/// `source`'s own fields as a YAML mapping suitable for embedding as another entry's
/// `parent:` block, with `author` renamed to `editor` when `role` is `CreatorRole::Editor` —
/// the common case for a multi-contributor anthology, where whoever the book is catalogued
/// under functions as the volume's editor once individual chapters get their own entries.
/// Any other role leaves the source's fields untouched (the "keep as author" case, for a
/// single- or co-authored book being split into named chapters/sections).
fn parent_block(source: &HEntry, role: CreatorRole) -> Result<serde_yaml_ng::Value> {
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
    if role == CreatorRole::Editor {
        if let Some(map) = inner.as_mapping_mut() {
            // Not when the source already has real editors: renaming would overwrite them
            // (`insert` replaces), silently losing the volume's actual editors.
            let has_editor = map.contains_key(Value::String("editor".to_string()));
            if !has_editor {
                if let Some(author) = map.remove(Value::String("author".to_string())) {
                    map.insert(Value::String("editor".to_string()), author);
                }
            }
        }
    }
    Ok(inner)
}

/// Build a new "book part" (chapter/section) entry's YAML from an existing book/anthology
/// `source`: `source`'s own fields become the new entry's `parent:` block (see
/// `parent_block`), so the part cites correctly ("In: Editor (Ed.), Book Title…") without
/// duplicating data the source entry already owns — unlike a plain copy-and-edit workflow,
/// where the two entries start drifting apart the moment either is edited afterward; see `refresh_book_part_parent` for pulling the source's
/// latest fields back in later. `part_type` is normally `"chapter"`. Returns YAML under a
/// placeholder key; the caller re-keys it (`Library::add_from_yaml` already generates a
/// fresh key from title/author when it sees one).
pub fn book_part_yaml(
    source: &HEntry,
    role: CreatorRole,
    part_type: &str,
    title: &str,
    creators: &[Creator],
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
    write_creators_into(&mut fields, creators)?;
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
    role: CreatorRole,
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
/// are the human-facing strings shown in the form, except `creators`, which is the full
/// structured author/editor/translator/… list (see [`crate::creator`]). Everything else on
/// the entry is left untouched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryFields {
    /// Hayagriva entry type, lowercased (e.g. `book`, `article`).
    pub entry_type: String,
    pub title: String,
    /// The entry's full creator list (authors, editors, translators, …), in display order.
    pub creators: Vec<Creator>,
    /// Publication year as free text (empty = no date).
    pub year: String,
    pub publisher: String,
    /// Publication location (e.g. the city of publication, "London") — nested under
    /// `publisher:` as `{name, location}` in the YAML (Hayagriva's `Publisher::location`),
    /// **not** the entry's own top-level `location:` field. That top-level field feeds a
    /// different CSL variable (`event-place`, for a conference/exhibition an entry happened
    /// at) — most citation styles' "place of publication" element (CSL `publisher-place`,
    /// Zotero's "Place") reads the nested one instead. Confirmed against
    /// `hayagriva::csl::taxonomy`'s `StandardVariable::PublisherPlace` resolution.
    pub location: String,
    pub doi: String,
    pub isbn: String,
    /// Title of the work this one appears in — the journal for an article, the book for a
    /// chapter, the proceedings for a conference paper. Lives on the entry's first `parent:`.
    pub container: String,
    /// Volume and issue are read from the entry itself, else its first parent that has one
    /// (Hayagriva files them on either; an article's usually sit on its periodical parent).
    /// A save writes back to wherever the value already lives, so nothing moves.
    pub volume: String,
    pub issue: String,
    /// Page range as free text (`6-13`, `xiv-xx`).
    pub pages: String,
    pub url: String,
    /// Hayagriva type of the entry's first `parent:` (lowercased; empty when it has none).
    /// Recorded media need a particular one to cite their show — see [`crate::item_kind`].
    pub parent_type: String,
    /// Hayagriva `genre`: the medium label styles print after the title ("Podcast episode",
    /// "Video"), or for an interview "Interview by NAME" (see [`crate::item_kind`]).
    pub genre: String,
    /// Running time (`runtime`, CSL `dimensions`), as Hayagriva writes it (`54:00`, `01:02:03`).
    pub runtime: String,
}

/// Read the editable fields out of an entry, for populating the structured editor.
pub fn read_fields(entry: &HEntry) -> EntryFields {
    EntryFields {
        entry_type: format!("{:?}", entry.entry_type()).to_lowercase(),
        title: title_string(entry).unwrap_or_default(),
        creators: creator::parse_creators(entry),
        year: entry.date().map(|d| d.year.to_string()).unwrap_or_default(),
        publisher: entry
            .publisher()
            .and_then(|p| p.name())
            .map(|name| name.value.to_string())
            .unwrap_or_default(),
        // Falls back to the entry's own top-level `location:` if `publisher.location` is
        // unset — entries created before 2026-09-19 (via ISBN lookup or the manual "New
        // item" form) wrote it there by mistake, and this keeps that data visible/editable
        // instead of it silently reading as blank. `apply_fields_to_yaml` cleans up the
        // stray top-level key the next time this entry's publisher/location is saved.
        location: entry
            .publisher()
            .and_then(|p| p.location())
            .or_else(|| {
                top_level_location_is_publication_place(
                    &format!("{:?}", entry.entry_type()).to_lowercase(),
                )
                .then(|| entry.location())
                .flatten()
            })
            .map(|l| l.value.to_string())
            .unwrap_or_default(),
        doi: entry.doi().unwrap_or_default().to_string(),
        isbn: entry.isbn().unwrap_or_default().to_string(),
        container: entry
            .parents()
            .first()
            .and_then(title_string)
            .unwrap_or_default(),
        volume: entry
            .volume()
            .or_else(|| entry.parents().iter().find_map(|p| p.volume()))
            .map(|v| v.to_string())
            .unwrap_or_default(),
        issue: entry
            .issue()
            .or_else(|| entry.parents().iter().find_map(|p| p.issue()))
            .map(|v| v.to_string())
            .unwrap_or_default(),
        pages: entry
            .page_range()
            .map(|r| r.to_string())
            .unwrap_or_default(),
        url: entry.url().map(|u| u.to_string()).unwrap_or_default(),
        parent_type: entry
            .parents()
            .first()
            .map(|p| format!("{:?}", p.entry_type()).to_lowercase())
            .unwrap_or_default(),
        genre: entry
            .genre()
            .map(|g| g.value.to_string())
            .unwrap_or_default(),
        runtime: entry.runtime().map(|r| r.to_string()).unwrap_or_default(),
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
    // publisher/location share one YAML key: a plain string when there's no location (just
    // `publisher: Name`), or a `{name, location}` mapping when there is — matching
    // Hayagriva's own `Publisher` serialization (see `EntryFields::location`'s doc comment
    // for why location can't live at the entry's own top level instead).
    if edited.publisher != current.publisher || edited.location != current.location {
        let name = edited.publisher.trim();
        let location = edited.location.trim();
        if name.is_empty() && location.is_empty() {
            inner.remove(key_of("publisher"));
        } else if location.is_empty() {
            inner.insert(key_of("publisher"), key_of(name));
        } else {
            let mut publisher = serde_yaml_ng::Mapping::new();
            if !name.is_empty() {
                publisher.insert(key_of("name"), key_of(name));
            }
            publisher.insert(key_of("location"), key_of(location));
            inner.insert(key_of("publisher"), Value::Mapping(publisher));
        }
        // Remove a stray top-level `location:` left by an entry created before this was
        // fixed (see `EntryFields::location`'s doc comment) — otherwise it sits as inert,
        // confusing leftover data now that the field it's actually read from is the nested
        // one above.
        //
        // Only when it's the legacy stray this editor was showing (same type rule as
        // `read_fields`, and not a value the editor never displayed): a conference's own event
        // place is real data and stays.
        let type_lower = inner
            .get(key_of("type"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        if top_level_location_is_publication_place(&type_lower) {
            inner.remove(key_of("location"));
        }
    }

    // creators — author / editor / affiliated YAML fields, each removed if it ends up empty.
    if edited.creators != current.creators {
        write_creators_into(inner, &edited.creators)?;
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

    // pages / url — plain scalars on the entry itself. A url that carries an access date
    // (`{value, date}`) keeps the date; only its `value` changes.
    if edited.pages != current.pages {
        set_or_remove(inner, "page-range", edited.pages.trim());
    }
    if edited.url != current.url {
        let url = edited.url.trim();
        match inner.get_mut(key_of("url")) {
            Some(Value::Mapping(m)) if !url.is_empty() => {
                m.insert(key_of("value"), key_of(url));
            }
            _ => set_or_remove(inner, "url", url),
        }
    }

    // volume / issue — written to wherever the value already lives (the entry, else the first
    // parent that has one); a brand-new value goes on the entry itself, as the New-item form
    // does. Hayagriva resolves either placement when rendering a citation.
    for (field, was, now) in [
        ("volume", &current.volume, &edited.volume),
        ("issue", &current.issue, &edited.issue),
    ] {
        if was != now {
            set_scalar_where_it_lives(inner, field, now.trim());
        }
    }

    // genre / runtime — plain scalars on the entry. A runtime is normalized to the form
    // Hayagriva parses (`54 min` → `54:00`); one that can't be read is an error rather than a
    // silently dropped value.
    if edited.genre != current.genre {
        set_or_remove(inner, "genre", edited.genre.trim());
    }
    if edited.runtime != current.runtime {
        let typed = edited.runtime.trim();
        let runtime = if typed.is_empty() {
            String::new()
        } else {
            crate::item_kind::normalize_runtime(typed).ok_or_else(|| BibError::Yaml {
                path: Path::new("<entry>").to_path_buf(),
                message: format!(
                    "running time \"{typed}\" isn't one Kartoteka can read — try 54:00 or 1:02:03"
                ),
            })?
        };
        set_or_remove(inner, "runtime", &runtime);
    }

    // parent type — re-typing an existing parent (a podcast episode's show must be `audio`
    // for its name to appear in citations; see `crate::item_kind`).
    if edited.parent_type != current.parent_type && !edited.parent_type.trim().is_empty() {
        if let Some(parent) = first_parent_mut(inner) {
            parent.insert(key_of("type"), key_of(edited.parent_type.trim()));
        }
    }

    // container — the first parent's title. A parent is created when there is none, typed by
    // what the entry is (a journal for an article, a book for a chapter, a show for an
    // episode, …).
    if edited.container != current.container {
        let container = edited.container.trim();
        let parent_type = if edited.parent_type.trim().is_empty() {
            default_parent_type(
                inner
                    .get(key_of("type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            )
            .to_string()
        } else {
            edited.parent_type.trim().to_string()
        };
        set_container_title(inner, container, &parent_type);
    }

    serde_yaml_ng::to_string(&doc).map_err(|e| BibError::Yaml {
        path: Path::new("<entry>").to_path_buf(),
        message: e.to_string(),
    })
}

/// The Hayagriva type a freshly created `parent:` gets for an entry of `entry_type`.
fn default_parent_type(entry_type: &str) -> &'static str {
    let entry_type = entry_type.to_lowercase();
    crate::item_kind::ITEM_KINDS
        .iter()
        .find(|k| k.entry_type == entry_type)
        .map(|k| k.parent_type)
        .unwrap_or("periodical")
}

/// The mapping of an entry's first `parent:`, whether written as a mapping or as a list of
/// mappings.
fn first_parent_mut(entry: &mut serde_yaml_ng::Mapping) -> Option<&mut serde_yaml_ng::Mapping> {
    match entry.get_mut(serde_yaml_ng::Value::String("parent".to_string()))? {
        serde_yaml_ng::Value::Mapping(m) => Some(m),
        serde_yaml_ng::Value::Sequence(seq) => seq.first_mut()?.as_mapping_mut(),
        _ => None,
    }
}

/// Set a scalar field where it already lives: on the entry if it has the key, else on the
/// first parent that has it, else (new value) on the entry. An empty `value` removes it from
/// both places, so a cleared field really is cleared.
fn set_scalar_where_it_lives(entry: &mut serde_yaml_ng::Mapping, field: &str, value: &str) {
    use serde_yaml_ng::Value;
    let key = Value::String(field.to_string());
    // YAML numbers stay numbers (`volume: 7`), matching hand-written Hayagriva.
    let scalar = || match value.parse::<i64>() {
        Ok(n) => Value::Number(n.into()),
        Err(_) => Value::String(value.to_string()),
    };
    if value.is_empty() {
        entry.remove(&key);
        if let Some(parent) = first_parent_mut(entry) {
            parent.remove(&key);
        }
    } else if entry.contains_key(&key) {
        entry.insert(key, scalar());
    } else if let Some(parent) = first_parent_mut(entry).filter(|p| p.contains_key(&key)) {
        parent.insert(key, scalar());
    } else {
        entry.insert(key, scalar());
    }
}

/// Set the first parent's `title`, creating the parent when the entry has none. An empty
/// `title` removes it (and the parent too, when that leaves it with nothing but its type).
fn set_container_title(entry: &mut serde_yaml_ng::Mapping, title: &str, parent_type: &str) {
    use serde_yaml_ng::Value;
    let key = |s: &str| Value::String(s.to_string());
    if let Some(parent) = first_parent_mut(entry) {
        if title.is_empty() {
            parent.remove(key("title"));
        } else {
            parent.insert(key("title"), key(title));
        }
        return;
    }
    if title.is_empty() {
        return;
    }
    let mut parent = serde_yaml_ng::Mapping::new();
    parent.insert(key("type"), key(parent_type));
    parent.insert(key("title"), key(title));
    entry.insert(key("parent"), Value::Mapping(parent));
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

/// Write `creators` into `map`'s `author`/`editor`/`affiliated` keys (via
/// [`creator::write_creators`]), removing any of the three that end up empty. Each
/// `Person`/`PersonsWithRoles` serializes through Hayagriva's own `Serialize` impl (a bare
/// scalar `"Family, Given"` string, or its `{name, given-name, …}` map form), matching exactly
/// how these fields already look on disk.
pub(crate) fn write_creators_into(
    map: &mut serde_yaml_ng::Mapping,
    creators: &[Creator],
) -> Result<()> {
    use serde_yaml_ng::Value;

    fn to_value<T: serde::Serialize>(v: &T) -> Result<Value> {
        serde_yaml_ng::to_value(v).map_err(|e| BibError::Yaml {
            path: Path::new("<entry>").to_path_buf(),
            message: e.to_string(),
        })
    }

    fn set_seq(map: &mut serde_yaml_ng::Mapping, key: &str, items: Vec<Value>) {
        let k = Value::String(key.to_string());
        if items.is_empty() {
            map.remove(&k);
        } else {
            map.insert(k, Value::Sequence(items));
        }
    }

    let (authors, editors, affiliated) = creator::write_creators(creators);
    let author_vals: Vec<Value> = authors.iter().map(to_value).collect::<Result<_>>()?;
    let editor_vals: Vec<Value> = editors.iter().map(to_value).collect::<Result<_>>()?;
    let affiliated_vals: Vec<Value> = affiliated.iter().map(to_value).collect::<Result<_>>()?;

    set_seq(map, "author", author_vals);
    set_seq(map, "editor", editor_vals);
    set_seq(map, "affiliated", affiliated_vals);

    Ok(())
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
            CreatorRole::Editor,
            "chapter",
            "On Personhood",
            &[Creator::new(CreatorRole::Author, "Smith", "John")],
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
            CreatorRole::Editor,
            "chapter",
            "On Personhood",
            &[Creator::new(CreatorRole::Author, "Smith", "John")],
            "",
        )
        .unwrap();
        let added = parse_single(&original, Path::new("new-item.yml")).unwrap();

        book.set_title("Essays on Being, Revised".to_string().into());
        let refreshed = refresh_book_part_parent(
            &serialize_entry_as(&added.entry, &added.key).unwrap(),
            &book,
            CreatorRole::Editor,
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

#[cfg(test)]
mod field_tests {
    use super::*;

    const ARTICLE: &str = "doe2001top:\n  type: article\n  title: On Things\n  author: Doe, Jane\n  date: 2001\n  page-range: 5-9\n  parent:\n    type: periodical\n    title: Journal of Things\n    volume: 7\n    issue: 2\n";

    fn parse(yaml: &str) -> HEntry {
        parse_single(yaml, Path::new("x.yml")).unwrap().entry
    }

    fn edit(yaml: &str, f: impl FnOnce(&mut EntryFields)) -> (String, EntryFields) {
        let current = read_fields(&parse(yaml));
        let mut edited = current.clone();
        f(&mut edited);
        let out = apply_fields_to_yaml(yaml, &current, &edited).unwrap();
        let after = read_fields(&parse(&out));
        (out, after)
    }

    #[test]
    fn reads_container_volume_issue_pages() {
        let f = read_fields(&parse(ARTICLE));
        assert_eq!(f.container, "Journal of Things");
        assert_eq!(f.volume, "7");
        assert_eq!(f.issue, "2");
        assert_eq!(f.pages, "5-9");
    }

    #[test]
    fn volume_edit_stays_on_the_parent_where_it_lives() {
        let (out, after) = edit(ARTICLE, |f| f.volume = "8".into());
        assert_eq!(after.volume, "8");
        assert!(!out.contains("\n  volume:"), "moved to the entry: {out}");
        assert!(out.contains("    volume: 8"), "{out}");
    }

    #[test]
    fn untouched_fields_are_left_exactly_alone() {
        let (_, after) = edit(ARTICLE, |f| f.pages = "5-10".into());
        assert_eq!(after.pages, "5-10");
        assert_eq!((after.volume.as_str(), after.issue.as_str()), ("7", "2"));
        assert_eq!(after.container, "Journal of Things");
    }

    #[test]
    fn clearing_a_field_removes_it() {
        let (out, after) = edit(ARTICLE, |f| {
            f.issue = String::new();
            f.pages = String::new();
        });
        assert_eq!(after.issue, "");
        assert_eq!(after.pages, "");
        assert!(
            !out.contains("issue:") && !out.contains("page-range"),
            "{out}"
        );
    }

    #[test]
    fn a_new_container_creates_a_typed_parent() {
        let bare =
            "doe2001on:\n  type: article\n  title: On Things\n  author: Doe, Jane\n  date: 2001\n";
        let (out, after) = edit(bare, |f| {
            f.container = "Journal of Things".into();
            f.volume = "3".into();
            f.url = "https://example.org/x".into();
        });
        assert_eq!(after.container, "Journal of Things");
        assert_eq!(after.volume, "3");
        assert_eq!(after.url, "https://example.org/x");
        assert!(out.contains("type: periodical"), "{out}");
        let chapter = "c:\n  type: chapter\n  title: A Chapter\n  author: Doe, Jane\n";
        let (out, _) = edit(chapter, |f| f.container = "The Book".into());
        assert!(out.contains("type: anthology"), "{out}");
    }

    #[test]
    fn a_url_with_an_access_date_keeps_its_date() {
        let yaml = "w:\n  type: web\n  title: A Page\n  url:\n    value: https://old.example/\n    date: 2024-05-01\n";
        let (out, after) = edit(yaml, |f| f.url = "https://new.example/".into());
        assert_eq!(after.url, "https://new.example/");
        assert!(out.contains("2024-05-01"), "access date lost: {out}");
    }

    #[test]
    fn a_parent_written_as_a_list_is_handled() {
        let yaml = "a:\n  type: article\n  title: T\n  parent:\n  - type: periodical\n    title: Old Journal\n";
        let (_, after) = edit(yaml, |f| f.container = "New Journal".into());
        assert_eq!(after.container, "New Journal");
    }
}
