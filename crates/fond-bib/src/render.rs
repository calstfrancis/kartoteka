//! CSL bibliography rendering (Milestone 3). Formats entries into a reference list using a
//! CSL style — either one bundled in Hayagriva's archive (e.g. Chicago notes) or a `.csl`
//! file (e.g. the bundled SBL style; see `assets/styles/`).

use hayagriva::archive::{locales, ArchivedStyle};
use hayagriva::citationberg::{IndependentStyle, LocaleCode, Style};
use hayagriva::{
    BibliographyDriver, BibliographyRequest, BufWriteFormat, CitationItem, CitationRequest, Entry,
};

use crate::error::{BibError, Result};

/// The SBL notes style, bundled as a CC-BY-SA asset because Hayagriva's archive does not
/// carry it. See `assets/styles/LICENSE-STYLES.md`.
const SBL_CSL: &str = include_str!("../assets/styles/sbl-fullnote-bibliography.csl");

/// Resolve a style name to a CSL style.
///
/// Recognised friendly names: `sbl`, `chicago-notes`, `chicago-author-date`. Any other
/// name is looked up in Hayagriva's style archive by Hayagriva name or CSL id, so the full
/// bundled catalogue is reachable (e.g. `apa`, `mla`, `ieee`).
pub fn resolve_style(name: &str) -> Result<IndependentStyle> {
    match name.to_ascii_lowercase().as_str() {
        "sbl" | "society-of-biblical-literature" => style_from_csl(SBL_CSL),
        "chicago-notes" | "chicago" => archived("chicago-notes"),
        "chicago-author-date" => archived("chicago-author-date"),
        other => archived(other),
    }
}

/// Parse a CSL style from XML, requiring an independent style.
pub fn style_from_csl(xml: &str) -> Result<IndependentStyle> {
    match Style::from_xml(xml).map_err(|e| BibError::Import {
        message: format!("invalid CSL style: {e}"),
    })? {
        Style::Independent(style) => Ok(style),
        Style::Dependent(_) => Err(BibError::Import {
            message: "CSL style is a dependent style; an independent style is required".into(),
        }),
    }
}

fn archived(id_or_name: &str) -> Result<IndependentStyle> {
    // Try Hayagriva's short name, then a full CSL id, then a bare CSL id turned into the
    // Zotero-style URL Hayagriva stores (so `apa`, `ieee`, etc. resolve).
    let archived = ArchivedStyle::by_name(id_or_name)
        .or_else(|| ArchivedStyle::by_id(id_or_name))
        .or_else(|| ArchivedStyle::by_id(&format!("http://www.zotero.org/styles/{id_or_name}")))
        .ok_or_else(|| BibError::Import {
            message: format!(
                "unknown style '{id_or_name}' (try 'sbl', 'chicago-notes', or a CSL id / \
                 --style-file)"
            ),
        })?;
    match archived.get() {
        Style::Independent(style) => Ok(style),
        Style::Dependent(_) => Err(BibError::Import {
            message: format!("style '{id_or_name}' is a dependent style"),
        }),
    }
}

/// A rendered reference-list entry: its citation key and the formatted text.
#[derive(Debug, Clone)]
pub struct RenderedEntry {
    pub key: String,
    pub text: String,
}

/// Render a reference list for the given entries with the given style. The output order
/// follows the style's sorting (CSL styles usually sort alphabetically), not the input
/// order. `format` selects plain text vs. HTML.
pub fn render_bibliography(
    entries: &[&Entry],
    style: &IndependentStyle,
    format: BufWriteFormat,
) -> Result<Vec<RenderedEntry>> {
    let locales = locales();
    let mut driver: BibliographyDriver<Entry> = BibliographyDriver::new();

    for entry in entries {
        driver.citation(CitationRequest::from_items(
            vec![CitationItem::new(entry, None, None, false, None)],
            style,
            &locales,
        ));
    }

    let rendered = driver.finish(BibliographyRequest::new(style, None, &locales));
    let Some(bibliography) = rendered.bibliography else {
        return Err(BibError::Import {
            message: "this CSL style produces no bibliography (it is citation-only)".into(),
        });
    };

    let mut out = Vec::with_capacity(bibliography.items.len());
    for item in &bibliography.items {
        let mut text = String::new();
        item.content
            .write_buf(&mut text, format)
            .map_err(|e| BibError::Import {
                message: format!("failed to render entry '{}': {e}", item.key),
            })?;
        out.push(RenderedEntry {
            key: item.key.clone(),
            text: text.trim().to_string(),
        });
    }
    Ok(out)
}

/// Whether a locale-independent style name is one Kartoteka recognises without the archive
/// (used only for nicer error messages).
pub fn is_builtin_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "sbl" | "society-of-biblical-literature" | "chicago-notes" | "chicago"
    )
}

/// The `en-US` locale code, exposed for callers that need a default.
pub fn default_locale() -> LocaleCode {
    LocaleCode::en_us()
}

/// Escape the Typst-special characters that can appear in a rendered reference so the
/// generated `.typ` is safe to compile.
fn typst_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '#' | '$' | '*' | '_' | '`' | '@' | '<' | '>') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl crate::library::Library {
    /// Load a collection's entries (in collection order), skipping any whose entry file is
    /// missing. Returns the loaded entries plus the list of missing keys.
    fn collection_entries(&self, slug: &str) -> Result<(String, Vec<Entry>, Vec<String>)> {
        let collection = self.load_collection(slug)?;
        let mut entries = Vec::new();
        let mut missing = Vec::new();
        for key in &collection.keys {
            if self.entry_path(key).exists() {
                entries.push(self.load_entry(key)?.entry);
            } else {
                missing.push(key.clone());
            }
        }
        Ok((collection.name, entries, missing))
    }

    /// Render a collection as a formatted reference list (CSL sort order).
    pub fn bibliography_for_collection(
        &self,
        slug: &str,
        style: &IndependentStyle,
        format: BufWriteFormat,
    ) -> Result<Vec<RenderedEntry>> {
        let (_name, entries, _missing) = self.collection_entries(slug)?;
        let refs: Vec<&Entry> = entries.iter().collect();
        render_bibliography(&refs, style, format)
    }

    /// Build an annotated bibliography as a Typst document: each rendered reference paired
    /// with its note prose, in the collection's own order.
    pub fn annotated_bibliography_typ(
        &self,
        slug: &str,
        style: &IndependentStyle,
    ) -> Result<String> {
        let (name, entries, _missing) = self.collection_entries(slug)?;
        let refs: Vec<&Entry> = entries.iter().collect();
        let rendered = render_bibliography(&refs, style, BufWriteFormat::Plain)?;

        // Map key -> rendered text so we can emit in the collection's (not CSL's) order.
        let by_key: std::collections::HashMap<&str, &str> = rendered
            .iter()
            .map(|r| (r.key.as_str(), r.text.as_str()))
            .collect();

        let collection = self.load_collection(slug)?;
        let mut out = String::new();
        out.push_str(&format!("= Annotated Bibliography — {}\n\n", name));

        for key in &collection.keys {
            let Some(text) = by_key.get(key.as_str()) else {
                continue; // missing entry; skipped (fsck reports dangling refs)
            };
            out.push_str(&typst_escape(text));
            out.push_str("\n\n");
            if let Some(note) = self.load_note(key)? {
                let body = note.body.trim();
                if !body.is_empty() {
                    // Note prose is the user's own text; pass it through as an indented
                    // block quote without escaping (they may intend Typst markup).
                    out.push_str("#block(inset: (left: 1em))[\n");
                    out.push_str(body);
                    out.push_str("\n]\n\n");
                }
            }
        }
        Ok(out)
    }
}
