//! `notes/<key>.md` — annotation prose with YAML frontmatter that owns the workflow
//! metadata Hayagriva has no field for (tags, read-status, rating, dates, attachments).
//! See `docs/DATA-MODEL.md`. Keeping this out of `entries/*.yml` is what lets the
//! generated `library.yml` stay pure Hayagriva.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{BibError, Result};
use crate::relation::Relation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadStatus {
    Unread,
    Reading,
    Read,
}

/// A record describing an attachment blob without the blob itself. Stored here (not in the
/// pure-Hayagriva entry file) so the entry stays clean while the record stays tracked and
/// hand-editable. `hash` doubles as the on-disk name in `attachments/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Attachment {
    /// Content address, e.g. `blake3:9f2b…`. Also the filename stem in `attachments/`.
    pub hash: String,
    pub filename: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
}

/// Coarse reading position (§8). Pairs with `read-status: reading`.
///
/// For a PDF, `page`/`of` are the whole story. For an EPUB — which has no fixed page
/// grid — `page`/`of` hold the 1-based chapter index and chapter count instead, and
/// `chapter_percent` (0-100) adds the scroll position within that chapter, so the reader
/// can resume at "12% through Chapter 4" rather than just "Chapter 4". `None` for a PDF,
/// where `page` alone is already precise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Progress {
    pub page: u32,
    pub of: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chapter_percent: Option<u8>,
}

/// Per-entry citation preferences (§13). `preferred_style` is advisory to export; `short`
/// is a hand-authored short form. Citation *history* is derived UI state, not stored here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CitePrefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_style: Option<String>,
}

impl CitePrefs {
    pub fn is_empty(&self) -> bool {
        self.short.is_none() && self.preferred_style.is_none()
    }
}

/// A per-item to-do (§20). Lives with the item so it merges per-key and is `vim`-fixable; a
/// global task view is a derived aggregation over these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Task {
    pub text: String,
    #[serde(default)]
    pub done: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct NoteFrontmatter {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_status: Option<ReadStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_added: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_read: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Citation keys of related entries (legacy untyped "see also" — see
    /// `Library::set_related`). Retained for back-compat; `kartoteka migrate` lifts these
    /// into `relations` with `predicate: related`. Kept indefinitely for anyone who never
    /// migrates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<String>,
    /// Typed relationship edges (see `docs/M2-SPEC.md` §1 and `Library::set_relations`).
    /// Holds both user-authored forward edges and Kartoteka-maintained inverse edges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<Relation>,
    /// Coarse reading position (§8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    /// Per-entry citation preferences (§13).
    #[serde(default, skip_serializing_if = "CitePrefs::is_empty")]
    pub cite: CitePrefs,
    /// Per-item to-dos (§20).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<Task>,
    /// Values for library-wide custom fields (see `custom_field::CustomFieldDefs`), keyed
    /// by field name. Always plain strings regardless of the field's declared type — a
    /// `Tag`-type value is comma-separated, a `Number`-type value is numeral text. A field
    /// with no value here just doesn't show one; nothing needs backfilling when a new
    /// field is defined.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub custom_fields: HashMap<String, String>,
    /// Set when this entry was created via "Create book part…" from another entry in this
    /// library (that entry's key) — lets "Refresh from source book" re-pull the parent
    /// book's current fields later, instead of the copy silently drifting out of sync the
    /// way a plain duplicate-and-edit (Zotero's "Create Book Section From Item") would.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from_book: Option<String>,
    /// Whether the source book's author(s) were copied into the new part's `parent:` block
    /// as `editor` or kept as `author` — remembered so a refresh regenerates the same shape
    /// instead of guessing. `None` for anything not created this way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from_role: Option<String>,
}

/// Split a faceted tag into `(facet, value)`. A tag containing `:` is faceted
/// (`discipline:theology/systematic` → `(Some("discipline"), "theology/systematic")`); a
/// plain tag returns `(None, tag)`. Only the first `:` splits, so values may contain `:`.
/// See `docs/M2-SPEC.md` §2 — facets are a convention over `tags:`, not a schema change.
pub fn split_facet(tag: &str) -> (Option<&str>, &str) {
    match tag.split_once(':') {
        Some((facet, value)) if !facet.is_empty() => (Some(facet), value),
        _ => (None, tag),
    }
}

/// A parsed note: frontmatter plus the Markdown body prose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Note {
    pub frontmatter: NoteFrontmatter,
    pub body: String,
}

impl Note {
    /// Parse a `notes/<key>.md` file body. A note with no `---` frontmatter block is valid
    /// (all-frontmatter-default, body = the whole text). `path` is used only for errors.
    pub fn parse(text: &str, path: &Path) -> Result<Note> {
        let Some((yaml, body)) = crate::util::split_frontmatter(text) else {
            return Ok(Note {
                frontmatter: NoteFrontmatter::default(),
                body: text.to_string(),
            });
        };

        let frontmatter: NoteFrontmatter = if yaml.trim().is_empty() {
            NoteFrontmatter::default()
        } else {
            serde_yaml_ng::from_str(yaml).map_err(|e| BibError::Frontmatter {
                path: path.to_path_buf(),
                message: e.to_string(),
            })?
        };

        Ok(Note {
            frontmatter,
            body: body.to_string(),
        })
    }

    /// Serialize back to file text. Frontmatter is emitted only when it carries something;
    /// an empty frontmatter yields a bare-body note (round-trips with [`Note::parse`]).
    pub fn to_text(&self) -> Result<String> {
        if self.frontmatter == NoteFrontmatter::default() {
            return Ok(self.body.clone());
        }
        let yaml =
            serde_yaml_ng::to_string(&self.frontmatter).map_err(|e| BibError::Frontmatter {
                path: Path::new("<note>").to_path_buf(),
                message: e.to_string(),
            })?;
        Ok(format!("---\n{yaml}---\n{}", self.body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p() -> PathBuf {
        PathBuf::from("notes/test.md")
    }

    #[test]
    fn parses_full_frontmatter() {
        let text = "---\ntags:\n  - christology\n  - liberation-theology\nread-status: read\nrating: 4\ndate-added: 2026-07-23\n---\nCone's pivot toward a Black christology.\n";
        let note = Note::parse(text, &p()).unwrap();
        assert_eq!(
            note.frontmatter.tags,
            vec!["christology", "liberation-theology"]
        );
        assert_eq!(note.frontmatter.read_status, Some(ReadStatus::Read));
        assert_eq!(note.frontmatter.rating, Some(4));
        assert_eq!(note.frontmatter.date_added.as_deref(), Some("2026-07-23"));
        assert_eq!(note.body, "Cone's pivot toward a Black christology.\n");
    }

    #[test]
    fn bare_body_note_has_no_frontmatter() {
        let text = "Just some prose, no metadata.\n";
        let note = Note::parse(text, &p()).unwrap();
        assert_eq!(note.frontmatter, NoteFrontmatter::default());
        assert_eq!(note.body, text);
        // And it round-trips to exactly the same bytes.
        assert_eq!(note.to_text().unwrap(), text);
    }

    #[test]
    fn splits_faceted_tags() {
        assert_eq!(
            split_facet("discipline:theology/systematic"),
            (Some("discipline"), "theology/systematic")
        );
        assert_eq!(split_facet("christology"), (None, "christology"));
        // A leading colon is not a facet.
        assert_eq!(split_facet(":weird"), (None, ":weird"));
    }

    #[test]
    fn round_trips_small_fields() {
        let text = "---\nprogress:\n  page: 112\n  of: 420\ncite:\n  short: Cone, Black Theology\n  preferred-style: chicago-author-date\ntasks:\n  - text: Verify the Barth quotation\n    done: false\n    due: 2026-08-15\n  - text: Re-read chapter 3\n    done: true\n---\nBody.\n";
        let note = Note::parse(text, &p()).unwrap();
        assert_eq!(
            note.frontmatter.progress,
            Some(Progress {
                page: 112,
                of: 420,
                chapter_percent: None
            })
        );
        assert_eq!(
            note.frontmatter.cite.short.as_deref(),
            Some("Cone, Black Theology")
        );
        assert_eq!(note.frontmatter.tasks.len(), 2);
        assert!(note.frontmatter.tasks[1].done);
        // Round-trips unchanged.
        assert_eq!(Note::parse(&note.to_text().unwrap(), &p()).unwrap(), note);
    }

    #[test]
    fn round_trips_typed_relations() {
        use crate::relation::Predicate;
        let text = "---\nrelations:\n  - predicate: critiques\n    target: barth1932kd\n  - predicate: cited-by\n    target: cone1975god\n    inverse: true\n---\nBody.\n";
        let note = Note::parse(text, &p()).unwrap();
        assert_eq!(note.frontmatter.relations.len(), 2);
        assert_eq!(
            note.frontmatter.relations[0].predicate,
            Predicate::Critiques
        );
        assert!(!note.frontmatter.relations[0].inverse);
        assert_eq!(note.frontmatter.relations[1].predicate, Predicate::CitedBy);
        assert!(note.frontmatter.relations[1].inverse);
        // Round-trips through the model unchanged.
        let reparsed = Note::parse(&note.to_text().unwrap(), &p()).unwrap();
        assert_eq!(note, reparsed);
    }

    #[test]
    fn round_trips_through_model() {
        let text = "---\ntags:\n  - christology\nread-status: reading\nattachments:\n  - hash: blake3:9f2bc4\n    filename: Cone-1970.pdf\n    bytes: 4823117\n    pages: 214\n---\nBody text here.\n";
        let note = Note::parse(text, &p()).unwrap();
        let reparsed = Note::parse(&note.to_text().unwrap(), &p()).unwrap();
        assert_eq!(note, reparsed);
        assert_eq!(note.frontmatter.attachments.len(), 1);
        assert_eq!(note.frontmatter.attachments[0].pages, Some(214));
    }
}
