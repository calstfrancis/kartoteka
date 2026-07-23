//! `notes/<key>.md` — annotation prose with YAML frontmatter that owns the workflow
//! metadata Hayagriva has no field for (tags, read-status, rating, dates, attachments).
//! See `docs/DATA-MODEL.md`. Keeping this out of `entries/*.yml` is what lets the
//! generated `library.yml` stay pure Hayagriva.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{BibError, Result};

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
        let Some((yaml, body)) = split_frontmatter(text) else {
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

/// Split leading `---`-delimited YAML frontmatter from the body. Returns `(yaml, body)`
/// or `None` when there is no frontmatter block. Handles `\n` and `\r\n` line endings.
fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    // Find a closing fence line: a line that is exactly "---".
    let mut search_start = 0;
    loop {
        let idx = rest[search_start..].find("---")?;
        let abs = search_start + idx;
        let at_line_start = abs == 0 || rest.as_bytes()[abs - 1] == b'\n';
        let after = &rest[abs + 3..];
        let closes_line = after.is_empty() || after.starts_with('\n') || after.starts_with("\r\n");
        if at_line_start && closes_line {
            let yaml = &rest[..abs];
            let body = after
                .strip_prefix('\n')
                .or_else(|| after.strip_prefix("\r\n"))
                .unwrap_or(after);
            return Some((yaml, body));
        }
        search_start = abs + 3;
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
    fn round_trips_through_model() {
        let text = "---\ntags:\n  - christology\nread-status: reading\nattachments:\n  - hash: blake3:9f2bc4\n    filename: Cone-1970.pdf\n    bytes: 4823117\n    pages: 214\n---\nBody text here.\n";
        let note = Note::parse(text, &p()).unwrap();
        let reparsed = Note::parse(&note.to_text().unwrap(), &p()).unwrap();
        assert_eq!(note, reparsed);
        assert_eq!(note.frontmatter.attachments.len(), 1);
        assert_eq!(note.frontmatter.attachments[0].pages, Some(214));
    }
}
