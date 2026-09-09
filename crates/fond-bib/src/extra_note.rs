//! Child notes (`notes/<key>/<note-id>.md`) and standalone notes
//! (`standalone-notes/<note-id>.md`) — see `docs/NOTES-SPEC.md` Tier 1. Lighter than the
//! entry's primary note (`note.rs`'s `Note`/`NoteFrontmatter`): just tags plus created/
//! modified stamps, since read-status/progress/attachments/etc. only make sense once per
//! entry, on the primary note. A child note's identity is its parent key plus its note id;
//! a standalone note's identity is its note id alone — neither ever shows up as a row in the
//! entries spreadsheet or Bookshelf grid (`docs/NOTES-SPEC.md` Tier 1 decision).

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{BibError, Result};
use crate::util::today_iso;

/// Lightweight frontmatter for a child or standalone note.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ExtraNoteFrontmatter {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
}

/// A parsed child or standalone note: frontmatter plus Markdown body. No `title` field —
/// deliberately, to avoid a second copy of what the body's own first line already says (see
/// `title()`); this keeps the frontmatter genuinely minimal rather than growing into a copy
/// of the primary note's much heavier `NoteFrontmatter`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtraNote {
    pub frontmatter: ExtraNoteFrontmatter,
    pub body: String,
}

impl ExtraNote {
    /// A fresh note with today's date stamped as both created and modified.
    pub fn new(body: impl Into<String>) -> ExtraNote {
        let stamp = today_iso();
        ExtraNote {
            frontmatter: ExtraNoteFrontmatter {
                tags: Vec::new(),
                created: Some(stamp.clone()),
                modified: Some(stamp),
            },
            body: body.into(),
        }
    }

    /// Parse a note file's text. A file with no `---` frontmatter block is valid (defaults,
    /// body = the whole text) — same tolerance as the primary note's `Note::parse`.
    pub fn parse(text: &str, path: &Path) -> Result<ExtraNote> {
        let Some((yaml, body)) = crate::util::split_frontmatter(text) else {
            return Ok(ExtraNote {
                frontmatter: ExtraNoteFrontmatter::default(),
                body: text.to_string(),
            });
        };

        let frontmatter: ExtraNoteFrontmatter = if yaml.trim().is_empty() {
            ExtraNoteFrontmatter::default()
        } else {
            serde_yaml_ng::from_str(yaml).map_err(|e| BibError::Frontmatter {
                path: path.to_path_buf(),
                message: e.to_string(),
            })?
        };

        Ok(ExtraNote {
            frontmatter,
            body: body.to_string(),
        })
    }

    /// Serialize back to file text. Frontmatter is emitted only when it carries something,
    /// same round-trip discipline as the primary note.
    pub fn to_text(&self) -> Result<String> {
        if self.frontmatter == ExtraNoteFrontmatter::default() {
            return Ok(self.body.clone());
        }
        let yaml =
            serde_yaml_ng::to_string(&self.frontmatter).map_err(|e| BibError::Frontmatter {
                path: Path::new("<note>").to_path_buf(),
                message: e.to_string(),
            })?;
        Ok(format!("---\n{yaml}---\n{}", self.body))
    }

    /// A display title derived from the body: the first non-empty line, with a leading
    /// Markdown heading marker (`#`, `##`, …) stripped. `"Untitled note"` for an empty body,
    /// so every note has something to show in a list before the user has typed anything.
    pub fn title(&self) -> String {
        for line in self.body.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let stripped = trimmed.trim_start_matches('#').trim();
            return if stripped.is_empty() {
                trimmed.to_string()
            } else {
                stripped.to_string()
            };
        }
        "Untitled note".to_string()
    }
}

/// Generate a fresh, sortable note id: `<today's date>-<8 hex chars>`. The date prefix means
/// note ids sort in creation order in a directory listing without needing to open every file;
/// the hash suffix (time-seeded, same idiom as `Annotation::drawn`'s id) makes same-day
/// collisions astronomically unlikely without a counter to persist.
pub fn generate_note_id() -> String {
    let date = today_iso();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let hex = blake3::hash(&nanos.to_le_bytes()).to_hex();
    format!("{date}-{}", &hex.as_str()[..8])
}

/// Whether a string is well-formed as a note-id filename stem: `YYYY-MM-DD-` followed by one
/// or more lowercase hex characters. Doesn't require the id to have actually come from
/// `generate_note_id` (a hand-renamed file with a plausible shape is still "valid" here) —
/// just enough of a check that `fsck` can flag a file that clearly isn't a note id at all
/// (e.g. a stray `.md` dropped into `standalone-notes/` by hand).
pub fn is_valid_note_id(id: &str) -> bool {
    if id.len() < 12 || id.as_bytes()[4] != b'-' || id.as_bytes()[7] != b'-' {
        return false;
    }
    let (year, month, day) = (&id[0..4], &id[5..7], &id[8..10]);
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(year) || !all_digits(month) || !all_digits(day) {
        return false;
    }
    let (month, day) = (month.parse::<u32>().unwrap(), day.parse::<u32>().unwrap());
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return false;
    }
    let Some(hash) = id[10..].strip_prefix('-') else {
        return false;
    };
    !hash.is_empty() && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p() -> PathBuf {
        PathBuf::from("notes/somekey/test.md")
    }

    #[test]
    fn bare_body_note_has_no_frontmatter() {
        let text = "Just some prose, no metadata.\n";
        let note = ExtraNote::parse(text, &p()).unwrap();
        assert_eq!(note.frontmatter, ExtraNoteFrontmatter::default());
        assert_eq!(note.body, text);
        assert_eq!(note.to_text().unwrap(), text);
    }

    #[test]
    fn round_trips_through_model() {
        let text = "---\ntags:\n  - idea\ncreated: 2026-09-06\nmodified: 2026-09-06\n---\nA loose \
                     idea, not about any one book.\n";
        let note = ExtraNote::parse(text, &p()).unwrap();
        let reparsed = ExtraNote::parse(&note.to_text().unwrap(), &p()).unwrap();
        assert_eq!(note, reparsed);
        assert_eq!(note.frontmatter.tags, vec!["idea"]);
    }

    #[test]
    fn title_prefers_first_nonempty_line_and_strips_heading_marker() {
        let note = ExtraNote::parse("\n\n# A Working Title\n\nBody prose.\n", &p()).unwrap();
        assert_eq!(note.title(), "A Working Title");
    }

    #[test]
    fn title_falls_back_when_body_is_empty() {
        let note = ExtraNote::new("");
        assert_eq!(note.title(), "Untitled note");
    }

    #[test]
    fn title_uses_plain_first_line_with_no_heading_marker() {
        let note = ExtraNote::new("Just a line of prose.\nMore below.");
        assert_eq!(note.title(), "Just a line of prose.");
    }

    #[test]
    fn generated_note_ids_are_valid_and_date_prefixed() {
        let id = generate_note_id();
        assert!(is_valid_note_id(&id));
        assert_eq!(&id[..4], &today_iso()[..4]);
    }

    #[test]
    fn rejects_malformed_ids() {
        assert!(!is_valid_note_id("not-a-note-id"));
        assert!(!is_valid_note_id("2026-09-06"));
        assert!(!is_valid_note_id("2026-09-06-"));
        assert!(!is_valid_note_id("2026-13-99-abcd1234"));
    }
}
