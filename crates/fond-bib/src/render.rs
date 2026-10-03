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
/// Recognised friendly names: `sbl`, `chicago-notes`, `chicago-author-date`, `turabian`
/// (the 8th-edition full-note style; `turabian-author-date` is the author-date one). Any
/// other name is looked up in Hayagriva's style archive by Hayagriva name or CSL id, so the
/// full bundled catalogue is reachable (e.g. `apa`, `mla`, `ieee`) — see [`style_choices`].
pub fn resolve_style(name: &str) -> Result<IndependentStyle> {
    match name.to_ascii_lowercase().as_str() {
        "sbl" | "society-of-biblical-literature" => style_from_csl(SBL_CSL),
        "chicago-notes" | "chicago" => archived("chicago-notes"),
        "chicago-author-date" => archived("chicago-author-date"),
        "turabian" | "turabian-notes" | "turabian-fullnote" => archived("turabian-fullnote-8"),
        other => archived(other),
    }
}

/// A citation style a user can pick: the short `name` [`resolve_style`] accepts and a
/// human-readable `label`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyleChoice {
    pub name: String,
    pub label: String,
}

/// Styles worth offering first, in this order — the ones a humanities writer reaches for.
/// `(name, label)`; every name must resolve (checked by a test).
const FEATURED_STYLES: &[(&str, &str)] = &[
    ("sbl", "SBL Handbook of Style (notes)"),
    ("chicago-notes", "Chicago (notes)"),
    ("chicago-author-date", "Chicago (author-date)"),
    ("turabian-fullnote-8", "Turabian (notes), 8th ed."),
    ("turabian-author-date", "Turabian (author-date)"),
    ("apa", "APA"),
    ("mla", "MLA"),
];

/// Every style Kartoteka can render with: the featured humanities styles first, then the
/// whole bundled catalogue alphabetically by label. Names are what [`resolve_style`] and the
/// CLI's `--style` accept.
pub fn style_choices() -> Vec<StyleChoice> {
    let featured: Vec<StyleChoice> = FEATURED_STYLES
        .iter()
        .map(|(name, label)| StyleChoice {
            name: (*name).to_string(),
            label: (*label).to_string(),
        })
        .collect();
    let featured_ids: Vec<&str> = featured.iter().map(|c| c.name.as_str()).collect();

    let mut rest: Vec<StyleChoice> = ArchivedStyle::all()
        .iter()
        .filter_map(|style| {
            let name = *style.names().first()?;
            (!featured_ids.contains(&name)).then(|| StyleChoice {
                name: name.to_string(),
                label: style.display_name().to_string(),
            })
        })
        .collect();
    rest.sort_by_key(|c| c.label.to_lowercase());
    rest.dedup_by(|a, b| a.name == b.name);

    featured.into_iter().chain(rest).collect()
}

/// Styles whose name or label contains `needle` (case-insensitive), for "did you mean…".
pub fn suggest_styles(needle: &str) -> Vec<StyleChoice> {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    style_choices()
        .into_iter()
        .filter(|c| c.name.contains(&needle) || c.label.to_lowercase().contains(&needle))
        .collect()
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
        .ok_or_else(|| {
            let close: Vec<String> = suggest_styles(id_or_name)
                .into_iter()
                .take(5)
                .map(|c| c.name)
                .collect();
            let hint = if close.is_empty() {
                "run `kartoteka styles` for the full list, or pass --style-file".to_string()
            } else {
                format!("did you mean: {}?", close.join(", "))
            };
            BibError::Import {
                message: format!("unknown style '{id_or_name}' ({hint})"),
            }
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
    /// Load the entries for `keys` (in the order given), skipping any whose entry file is
    /// missing. Returns the loaded entries plus the list of missing keys.
    fn entries_for_keys(&self, keys: &[String]) -> Result<(Vec<Entry>, Vec<String>)> {
        let mut entries = Vec::new();
        let mut missing = Vec::new();
        for key in keys {
            if self.entry_path(key).exists() {
                entries.push(self.load_entry(key)?.entry);
            } else {
                missing.push(key.clone());
            }
        }
        Ok((entries, missing))
    }

    /// Render a reference list for the given citation keys (CSL sort order). Keys with no
    /// entry file are skipped.
    pub fn bibliography_for_keys(
        &self,
        keys: &[String],
        style: &IndependentStyle,
        format: BufWriteFormat,
    ) -> Result<Vec<RenderedEntry>> {
        let (entries, _missing) = self.entries_for_keys(keys)?;
        let refs: Vec<&Entry> = entries.iter().collect();
        render_bibliography(&refs, style, format)
    }

    /// Render a collection as a formatted reference list (CSL sort order).
    pub fn bibliography_for_collection(
        &self,
        slug: &str,
        style: &IndependentStyle,
        format: BufWriteFormat,
    ) -> Result<Vec<RenderedEntry>> {
        let collection = self.load_collection(slug)?;
        self.bibliography_for_keys(&collection.keys, style, format)
    }

    /// Render the whole library as a formatted reference list (CSL sort order).
    pub fn bibliography_for_all(
        &self,
        style: &IndependentStyle,
        format: BufWriteFormat,
    ) -> Result<Vec<RenderedEntry>> {
        self.bibliography_for_keys(&self.keys_sorted()?, style, format)
    }

    /// Build an annotated bibliography as a Typst document: each rendered reference paired
    /// with its note prose, in the collection's own order.
    pub fn annotated_bibliography_typ(
        &self,
        slug: &str,
        style: &IndependentStyle,
    ) -> Result<String> {
        let collection = self.load_collection(slug)?;
        self.annotated_bibliography_typ_for_keys(&collection.name, &collection.keys, style)
    }

    /// Build an annotated bibliography as a Typst document for the whole library, in citation-key
    /// order.
    pub fn annotated_bibliography_typ_for_all(&self, style: &IndependentStyle) -> Result<String> {
        self.annotated_bibliography_typ_for_keys("All entries", &self.keys_sorted()?, style)
    }

    /// Annotated bibliography for `keys`, in the order given, headed by `title`.
    pub fn annotated_bibliography_typ_for_keys(
        &self,
        title: &str,
        keys: &[String],
        style: &IndependentStyle,
    ) -> Result<String> {
        let rendered = self.bibliography_for_keys(keys, style, BufWriteFormat::Plain)?;

        // Map key -> rendered text so we can emit in the caller's (not CSL's) order.
        let by_key: std::collections::HashMap<&str, &str> = rendered
            .iter()
            .map(|r| (r.key.as_str(), r.text.as_str()))
            .collect();

        let mut out = String::new();
        out.push_str(&format!("= Annotated Bibliography — {title}\n\n"));

        for key in keys {
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
