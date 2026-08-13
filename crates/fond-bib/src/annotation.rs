//! `annots/<key>.json` — the annotation sidecar (highlights and marginalia), for either a
//! PDF or an EPUB attachment. See `docs/DATA-MODEL.md`. This is the authoritative record for
//! annotations; the PDF/EPUB itself is the disposable copy. A PDF annotation anchors on a
//! page number plus quadpoints plus a text snippet (with surrounding context), so it can be
//! re-located even on a differently-produced copy of the same PDF — never on internal PDF
//! object ids. An EPUB has no fixed page grid to hang quadpoints on, so it anchors on chapter
//! (a zip-internal path) plus that same snippet/prefix/suffix scheme instead
//! (`docs/M5-SPEC.md` 5C) — `page`/`quadpoints` stay `None`/empty for those. `page` and
//! `chapter` are mutually exclusive in practice (which one is set says which format authored
//! the annotation), but both live on one `Annotation` type rather than an enum, so a single
//! sidecar file and a single annotations-list UI keep working uniformly across formats.
//!
//! This module owns the on-disk format only (a plain JSON file, like `note.rs` owns the
//! note file). Reading annotations out of, and writing them into, actual PDF bytes is
//! `fond-doc`'s job (Milestone 4, needs PDFium); EPUB highlighting is applied live in the
//! reader UI (WebKit DOM search-and-wrap, not embedded into the EPUB file itself).

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{BibError, Result};
use crate::util::today_iso;

/// Current sidecar schema version. Bumped only on a breaking format change.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnnotationKind {
    Highlight,
    Underline,
    Strikeout,
    /// A freestanding marginal note not tied to highlighted text.
    Note,
}

/// A single annotation. For a PDF, `quadpoints` is the primary anchor when the PDF matches
/// `AnnotationSidecar::pdf_hash`, with `snippet` (plus prefix/suffix context) as the
/// re-anchor key when it does not. For an EPUB (no `quadpoints`, no fixed page grid),
/// `chapter` + `snippet` (+ context) is the only anchor there is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    /// App-generated stable id (ULID-style), so an annotation survives edits.
    pub id: String,
    pub kind: AnnotationKind,
    /// PDF page the annotation is on (1-based). `None` for an EPUB annotation, which
    /// anchors on `chapter` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    /// EPUB chapter the annotation is in, as the zip-internal path
    /// (`fond_doc::EpubBook::spine`'s own path shape, e.g. `OEBPS/chap1.xhtml`). `None` for
    /// a PDF annotation, which anchors on `page` + `quadpoints` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chapter: Option<String>,
    /// PDF highlight quads: arrays of 8 floats per rectangle, in PDF user space. Always empty
    /// for an EPUB annotation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quadpoints: Vec<[f64; 8]>,
    /// The highlighted text — the fine re-anchoring key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// A short window of text before the snippet, for disambiguation on re-anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet_prefix: Option<String>,
    /// A short window of text after the snippet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet_suffix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// The user's marginal comment on this annotation, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
}

/// The full sidecar for one entry's PDF.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnnotationSidecar {
    pub schema: u32,
    pub key: String,
    /// The attachment (blake3) hash these annotations were authored against. If the present
    /// blob's hash differs, callers should re-anchor by snippet rather than trust
    /// quadpoints blindly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf_hash: Option<String>,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
}

impl Annotation {
    /// Build an annotation imported from an embedded PDF annotation, with a deterministic
    /// id derived from its kind/page/text so re-importing the same highlight is idempotent
    /// (and re-anchoring by snippet still works across differently-produced PDFs).
    pub fn imported(
        kind: AnnotationKind,
        page: u32,
        quadpoints: Vec<[f64; 8]>,
        snippet: Option<String>,
        contents: Option<String>,
    ) -> Annotation {
        let seed = format!(
            "{kind:?}|{page}|{}|{}",
            snippet.as_deref().unwrap_or(""),
            contents.as_deref().unwrap_or("")
        );
        let hex = blake3::hash(seed.as_bytes()).to_hex();
        Annotation {
            id: format!("pdf-{}", &hex.as_str()[..16]),
            kind,
            page: Some(page),
            chapter: None,
            quadpoints,
            snippet,
            snippet_prefix: None,
            snippet_suffix: None,
            color: None,
            note: contents,
            created: None,
            modified: None,
        }
    }

    /// Build a fresh, user-drawn annotation from a live selection in the built-in PDF viewer
    /// — either real selected text (`snippet` is `Some`, from `fond_doc::select_text_in_rect`)
    /// or a plain drag rectangle over an area with no text (`snippet` is `None`). Unlike
    /// `imported`, the id is time-seeded rather than content-seeded: being newly created, not
    /// re-imported, it has no need for content-based idempotency, and a rectangle-only
    /// highlight has no content to seed from anyway. `color` is the hex string to record;
    /// `None` falls back to the original default amber, so existing callers that don't care
    /// about colour choice are unaffected.
    pub fn drawn(
        kind: AnnotationKind,
        page: u32,
        quadpoints: Vec<[f64; 8]>,
        snippet: Option<String>,
        note: Option<String>,
        color: Option<String>,
    ) -> Annotation {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let hex = blake3::hash(&nanos.to_le_bytes()).to_hex();
        let stamp = today_iso();
        Annotation {
            id: format!("hl-{}", &hex.as_str()[..16]),
            kind,
            page: Some(page),
            chapter: None,
            quadpoints,
            snippet,
            snippet_prefix: None,
            snippet_suffix: None,
            color: Some(color.unwrap_or_else(|| "#f6c344".to_string())),
            note,
            created: Some(stamp.clone()),
            modified: Some(stamp),
        }
    }

    /// Build a fresh, user-drawn annotation from a live text selection in the built-in EPUB
    /// reader. Unlike a PDF highlight, there's no fixed page grid to hang quadpoints on, so
    /// the anchor is `chapter` (a zip-internal path, `fond_doc::EpubBook::spine`'s own shape)
    /// plus the same snippet/prefix/suffix scheme `imported`/`drawn` use for re-anchoring a
    /// PDF highlight — here it's the *only* anchor, not a fallback. Id is time-seeded, same
    /// reasoning as `drawn`: newly created, not re-imported, so no idempotency need.
    pub fn drawn_epub(
        kind: AnnotationKind,
        chapter: String,
        snippet: String,
        snippet_prefix: Option<String>,
        snippet_suffix: Option<String>,
        note: Option<String>,
    ) -> Annotation {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let hex = blake3::hash(&nanos.to_le_bytes()).to_hex();
        let stamp = today_iso();
        Annotation {
            id: format!("hl-{}", &hex.as_str()[..16]),
            kind,
            page: None,
            chapter: Some(chapter),
            quadpoints: Vec::new(),
            snippet: Some(snippet),
            snippet_prefix,
            snippet_suffix,
            color: Some("#f6c344".to_string()),
            note,
            created: Some(stamp.clone()),
            modified: Some(stamp),
        }
    }
}

impl AnnotationSidecar {
    /// A new, empty sidecar for a key.
    pub fn new(key: impl Into<String>) -> AnnotationSidecar {
        AnnotationSidecar {
            schema: SCHEMA_VERSION,
            key: key.into(),
            pdf_hash: None,
            annotations: Vec::new(),
        }
    }

    /// Insert or replace an annotation by id (used when merging imported PDF annotations).
    pub fn upsert(&mut self, annotation: Annotation) {
        match self.annotations.iter_mut().find(|a| a.id == annotation.id) {
            Some(existing) => *existing = annotation,
            None => self.annotations.push(annotation),
        }
    }

    pub fn parse(text: &str, path: &Path) -> Result<AnnotationSidecar> {
        serde_json::from_str(text).map_err(|e| BibError::PlainYaml {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    }

    /// Serialize to pretty JSON with a trailing newline (stable field order for clean git
    /// diffs).
    pub fn to_json(&self) -> Result<String> {
        let mut s = serde_json::to_string_pretty(self).map_err(|e| BibError::PlainYaml {
            path: Path::new("<annotations>").to_path_buf(),
            message: e.to_string(),
        })?;
        s.push('\n');
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample() -> AnnotationSidecar {
        AnnotationSidecar {
            schema: 1,
            key: "cone1970black".into(),
            pdf_hash: Some("blake3:9f2bc4".into()),
            annotations: vec![Annotation {
                id: "01J8Z000".into(),
                kind: AnnotationKind::Highlight,
                page: Some(7),
                chapter: None,
                quadpoints: vec![[72.0, 640.1, 523.4, 640.1, 72.0, 628.7, 523.4, 628.7]],
                snippet: Some("the gospel of Jesus is a gospel of liberation".into()),
                snippet_prefix: Some("what I mean is that ".into()),
                snippet_suffix: Some(" for the oppressed".into()),
                color: Some("#f6c344".into()),
                note: Some("core thesis".into()),
                created: Some("2026-07-23T14:11:02Z".into()),
                modified: Some("2026-07-23T14:11:02Z".into()),
            }],
        }
    }

    #[test]
    fn round_trips_through_json() {
        let sidecar = sample();
        let json = sidecar.to_json().unwrap();
        let reparsed = AnnotationSidecar::parse(&json, &PathBuf::from("annots/x.json")).unwrap();
        assert_eq!(sidecar, reparsed);
        assert!(json.ends_with('\n'));
    }

    #[test]
    fn kind_serializes_lowercase() {
        let json = serde_json::to_string(&AnnotationKind::Strikeout).unwrap();
        assert_eq!(json, "\"strikeout\"");
    }

    #[test]
    fn empty_optionals_are_omitted() {
        let mut sidecar = AnnotationSidecar::new("k");
        sidecar.annotations.push(Annotation {
            id: "1".into(),
            kind: AnnotationKind::Note,
            page: Some(1),
            chapter: None,
            quadpoints: vec![],
            snippet: None,
            snippet_prefix: None,
            snippet_suffix: None,
            color: None,
            note: Some("just a margin note".into()),
            created: None,
            modified: None,
        });
        let json = sidecar.to_json().unwrap();
        assert!(!json.contains("quadpoints"));
        assert!(!json.contains("pdf_hash"));
        assert!(!json.contains("\"snippet\""));
        assert!(!json.contains("chapter"));
        assert!(json.contains("just a margin note"));
    }

    #[test]
    fn page_and_chapter_are_mutually_exclusive_omission() {
        // A PDF annotation omits `chapter`; an EPUB one omits `page`. Confirms the two
        // formats' anchors don't leak into each other's JSON.
        let pdf = Annotation {
            id: "1".into(),
            kind: AnnotationKind::Highlight,
            page: Some(3),
            chapter: None,
            quadpoints: vec![],
            snippet: None,
            snippet_prefix: None,
            snippet_suffix: None,
            color: None,
            note: None,
            created: None,
            modified: None,
        };
        let json = serde_json::to_string(&pdf).unwrap();
        assert!(json.contains("\"page\":3"));
        assert!(!json.contains("chapter"));

        let epub = Annotation::drawn_epub(
            AnnotationKind::Highlight,
            "OEBPS/chap1.xhtml".into(),
            "a gospel of liberation".into(),
            Some("what I mean is that ".into()),
            Some(" for the oppressed".into()),
            None,
        );
        let json = serde_json::to_string(&epub).unwrap();
        assert!(!json.contains("\"page\""));
        assert!(json.contains("\"chapter\":\"OEBPS/chap1.xhtml\""));
        assert!(json.contains("a gospel of liberation"));
    }
}
