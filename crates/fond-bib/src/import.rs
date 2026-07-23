//! Migration import from Zotero. Slice A: the BetterBibTeX `.bib` importer (roadmap 2.1,
//! 2.3, 2.4, 2.6). The Zotero SQLite reader (collections, annotations) is a later slice.
//!
//! Imported entries **keep their original citation keys** — regenerating them would break
//! every `@key` reference in the user's existing Typst documents. Anything that does not
//! map cleanly is collected into [`ImportReport`] rather than dropped silently.

use std::path::{Path, PathBuf};

use biblatex::{Bibliography, ChunksExt};

use crate::error::{BibError, Result};
use crate::library::Library;
use crate::note::Attachment;
use crate::util;

/// Options controlling an import run.
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// Overwrite an entry whose key already exists. Default: false (skip + report).
    pub overwrite: bool,
    /// Hash and copy referenced attachment files into `attachments/`. Default: true.
    pub copy_attachments: bool,
    /// Directory that relative attachment paths are resolved against (e.g. the folder the
    /// `.bib` was exported to). Absolute paths ignore this.
    pub attachment_base: Option<PathBuf>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions {
            overwrite: false,
            copy_attachments: true,
            attachment_base: None,
        }
    }
}

/// The outcome of an import: what came in cleanly, and everything that did not.
#[derive(Debug, Default)]
pub struct ImportReport {
    /// Keys written.
    pub imported: Vec<String>,
    /// Keys already present and left untouched (no `--overwrite`).
    pub skipped_key_collisions: Vec<String>,
    /// Source keys unsafe as a cross-platform filename; skipped rather than remapped
    /// (remapping would silently break citations). `(key, reason)`.
    pub skipped_unsafe_keys: Vec<(String, String)>,
    /// Non-fatal parse diagnostics from the biblatex extras pass.
    pub parse_warnings: Vec<String>,
    /// Fields present in the source that Kartoteka did not explicitly map, so they may not
    /// have survived into the entry. `(key, field names)`.
    pub unmapped_fields: Vec<(String, Vec<String>)>,
    /// `(key, tag count written to the note)`.
    pub tags_written: Vec<(String, usize)>,
    /// `(key, blake3 hex)` for attachment files found, hashed, and copied.
    pub attachments_copied: Vec<(String, String)>,
    /// `(key, referenced path)` for attachment files that could not be found.
    pub attachments_missing: Vec<(String, String)>,
}

impl ImportReport {
    pub fn is_clean(&self) -> bool {
        self.skipped_key_collisions.is_empty()
            && self.skipped_unsafe_keys.is_empty()
            && self.parse_warnings.is_empty()
            && self.unmapped_fields.is_empty()
            && self.attachments_missing.is_empty()
    }
}

/// biblatex field names that Hayagriva's converter maps, or that we handle ourselves
/// (`keywords` → tags, `file` → attachment). Anything outside this set is flagged as
/// possibly-unmapped in the report.
const KNOWN_FIELDS: &[&str] = &[
    "title",
    "shorttitle",
    "author",
    "editor",
    "translator",
    "date",
    "year",
    "month",
    "day",
    "publisher",
    "location",
    "address",
    "journal",
    "journaltitle",
    "booktitle",
    "series",
    "volume",
    "number",
    "issue",
    "pages",
    "pagetotal",
    "edition",
    "chapter",
    "doi",
    "isbn",
    "issn",
    "url",
    "urldate",
    "note",
    "abstract",
    "language",
    "keywords",
    "file",
    "organization",
    "institution",
    "school",
    "howpublished",
    "type",
    "eprint",
    "eprinttype",
    "archiveprefix",
    "primaryclass",
    "annotation",
    "annote",
    "subtitle",
    "titleaddon",
    "shortjournal",
    "langid",
    "pmid",
    "eid",
    "crossref",
    "origdate",
    "eventdate",
    "venue",
];

impl Library {
    /// Import a BetterBibTeX / BibLaTeX `.bib` source string. Writes entries (keys
    /// preserved), tags into notes, and attachment records; regenerates `library.yml`.
    pub fn import_bibtex(&self, source: &str, opts: &ImportOptions) -> Result<ImportReport> {
        let mut report = ImportReport::default();

        // Bibliographic data via Hayagriva (clean Entry objects, keys preserved).
        let library = hayagriva::io::from_biblatex_str(source).map_err(|errors| {
            let joined = errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            BibError::Yaml {
                path: Path::new("<bibtex import>").to_path_buf(),
                message: format!("could not parse BibLaTeX source: {joined}"),
            }
        })?;

        // Extras (keywords, file) via biblatex directly — Hayagriva drops these. A parse
        // failure here is non-fatal: we still import the bibliographic data.
        let extras = match Bibliography::parse(source) {
            Ok(bib) => Some(bib),
            Err(e) => {
                report
                    .parse_warnings
                    .push(format!("keyword/file extraction skipped: {e}"));
                None
            }
        };

        let today = util::today_iso();

        for entry in library.iter() {
            let key = entry.key().to_string();

            if let Some(reason) = unsafe_key_reason(&key) {
                report.skipped_unsafe_keys.push((key, reason));
                continue;
            }
            if self.entry_path(&key).exists() && !opts.overwrite {
                report.skipped_key_collisions.push(key);
                continue;
            }

            self.write_entry(entry)?;
            report.imported.push(key.clone());

            // Extras: tags → note frontmatter, file → attachment records.
            if let Some(extra) = extras.as_ref().and_then(|b| b.get(&key)) {
                self.apply_extras(&key, extra, opts, &today, &mut report)?;
            }
        }

        self.regenerate_library_yml()?;
        Ok(report)
    }

    fn apply_extras(
        &self,
        key: &str,
        extra: &biblatex::Entry,
        opts: &ImportOptions,
        today: &str,
        report: &mut ImportReport,
    ) -> Result<()> {
        let mut note = self.load_note(key)?.unwrap_or_default();
        let mut touched = false;

        // Report fields we did not explicitly map.
        let unmapped: Vec<String> = extra
            .fields
            .keys()
            .filter(|f| !KNOWN_FIELDS.contains(&f.as_str()))
            .cloned()
            .collect();
        if !unmapped.is_empty() {
            report.unmapped_fields.push((key.to_string(), unmapped));
        }

        // Tags from `keywords`.
        if let Some(chunks) = extra.get("keywords") {
            let tags = split_keywords(&chunks.format_verbatim());
            if !tags.is_empty() {
                report.tags_written.push((key.to_string(), tags.len()));
                note.frontmatter.tags = merge_tags(&note.frontmatter.tags, &tags);
                touched = true;
            }
        }

        // Attachments from `file`.
        if opts.copy_attachments {
            if let Some(chunks) = extra.get("file") {
                for path in extract_attachment_paths(&chunks.format_verbatim()) {
                    match self.ingest_attachment(&path, opts.attachment_base.as_deref())? {
                        Some(att) => {
                            let hex = att.hash.trim_start_matches("blake3:").to_string();
                            report.attachments_copied.push((key.to_string(), hex));
                            if !note
                                .frontmatter
                                .attachments
                                .iter()
                                .any(|a| a.hash == att.hash)
                            {
                                note.frontmatter.attachments.push(att);
                            }
                            touched = true;
                        }
                        None => {
                            report
                                .attachments_missing
                                .push((key.to_string(), path.clone()));
                        }
                    }
                }
            }
        }

        if touched {
            if note.frontmatter.date_added.is_none() {
                note.frontmatter.date_added = Some(today.to_string());
            }
            self.write_note(key, &note)?;
        }
        Ok(())
    }

    /// Resolve, hash, and copy an attachment file into `attachments/`. Returns the record
    /// on success, or `None` if the file could not be found.
    fn ingest_attachment(&self, raw_path: &str, base: Option<&Path>) -> Result<Option<Attachment>> {
        let candidate = resolve_path(raw_path, base);
        if !candidate.is_file() {
            return Ok(None);
        }
        let bytes = std::fs::read(&candidate).map_err(|e| BibError::io(&candidate, e))?;
        let hex = blake3::hash(&bytes).to_hex().to_string();

        std::fs::create_dir_all(self.attachments_dir())
            .map_err(|e| BibError::io(self.attachments_dir(), e))?;
        let dest = self.attachment_blob_path(&hex);
        if !dest.exists() {
            std::fs::write(&dest, &bytes).map_err(|e| BibError::io(&dest, e))?;
        }

        let filename = candidate
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment")
            .to_string();

        Ok(Some(Attachment {
            hash: format!("blake3:{hex}"),
            filename,
            bytes: bytes.len() as u64,
            pages: None, // page count needs PDF parsing — Milestone 4.
        }))
    }
}

fn resolve_path(raw: &str, base: Option<&Path>) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    match base {
        Some(base) => base.join(p),
        None => p.to_path_buf(),
    }
}

/// A key that is not safe as a cross-platform filename is skipped (never silently
/// remapped, which would break citations). Returns the reason if unsafe.
fn unsafe_key_reason(key: &str) -> Option<String> {
    if key.is_empty() {
        return Some("empty key".to_string());
    }
    const ILLEGAL: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    if let Some(c) = key.chars().find(|c| ILLEGAL.contains(c) || c.is_control()) {
        return Some(format!("contains character '{c}' illegal in a filename"));
    }
    None
}

/// Split a `keywords` field into individual tags. Zotero separates on comma; some exports
/// use semicolons. Trims whitespace and drops empties.
fn split_keywords(value: &str) -> Vec<String> {
    value
        .split([',', ';'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

fn merge_tags(existing: &[String], incoming: &[String]) -> Vec<String> {
    let mut out = existing.to_vec();
    for tag in incoming {
        if !out.iter().any(|t| t == tag) {
            out.push(tag.clone());
        }
    }
    out
}

/// Parse a BetterBibTeX/Zotero `file` field into candidate filesystem paths. Handles the
/// `;`-separated list and Zotero's `title:path:mimetype` triple form (the mimetype segment
/// contains a `/`). Heuristic, since the field format is not rigorously specified.
fn extract_attachment_paths(field: &str) -> Vec<String> {
    field
        .split(';')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|part| {
            let segs: Vec<&str> = part.split(':').collect();
            if segs.len() >= 3 && segs.last().is_some_and(|m| m.contains('/')) {
                // title : path : mimetype  (path may itself contain colons)
                segs[1..segs.len() - 1].join(":")
            } else {
                part.to_string()
            }
        })
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_keywords() {
        assert_eq!(
            split_keywords("christology, liberation theology ; 20th-century"),
            vec!["christology", "liberation theology", "20th-century"]
        );
        assert!(split_keywords("  ,  ; ").is_empty());
    }

    #[test]
    fn extracts_plain_and_triple_file_paths() {
        assert_eq!(
            extract_attachment_paths("/home/cal/Zotero/storage/AB/Doc.pdf"),
            vec!["/home/cal/Zotero/storage/AB/Doc.pdf"]
        );
        assert_eq!(
            extract_attachment_paths("Cone 1970:/home/cal/store/Cone.pdf:application/pdf"),
            vec!["/home/cal/store/Cone.pdf"]
        );
        assert_eq!(
            extract_attachment_paths("a.pdf;b.pdf"),
            vec!["a.pdf", "b.pdf"]
        );
    }

    #[test]
    fn flags_unsafe_keys() {
        assert!(unsafe_key_reason("berdyaev1937destiny").is_none());
        assert!(unsafe_key_reason("bad/key").is_some());
        assert!(unsafe_key_reason("bad:key").is_some());
        assert!(unsafe_key_reason("").is_some());
    }
}
