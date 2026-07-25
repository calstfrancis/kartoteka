//! `projects/<slug>.yml` — a named project that declares the Typst documents it comprises,
//! so Kartoteka can scan them for `@key` citations. See `docs/M2-SPEC.md` §5.
//!
//! The declaration here is authoritative and hand-editable. The *reverse* map ("this source
//! is used in project X") is a scan result — churny and rebuildable — and lives in
//! `.kartoteka/`, never written back into `notes/` (see [`crate::Library::scan_usage`]).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{BibError, Result};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Typst source files this project comprises. Scanned for `@key` citations. A path that
    /// does not exist is a dangling reference reported by fsck, never a hard error.
    #[serde(default)]
    pub documents: Vec<PathBuf>,
}

impl Project {
    pub fn parse(text: &str, path: &Path) -> Result<Project> {
        serde_yaml_ng::from_str(text).map_err(|e| BibError::PlainYaml {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    }

    pub fn to_text(&self) -> Result<String> {
        serde_yaml_ng::to_string(self).map_err(|e| BibError::PlainYaml {
            path: Path::new("<project>").to_path_buf(),
            message: e.to_string(),
        })
    }
}

/// Extract the citation keys referenced by Typst `@key` syntax in `src`.
///
/// Typst references are `@` followed by an identifier: letters, digits, `_`, `-`, and `.`,
/// per Typst's label grammar. A leading `@` inside an email-like run (`foo@bar`) is avoided
/// by requiring the `@` to be at a boundary (start of string or a non-identifier char
/// before it). Returns keys in first-seen order, de-duplicated.
pub fn scan_typst_citation_keys(src: &str) -> Vec<String> {
    fn is_key_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
    }
    let bytes = src.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    for (i, (byte_idx, c)) in chars.iter().enumerate() {
        if *c != '@' {
            continue;
        }
        // Boundary check: preceding char must not be an identifier char (so `a@b` — an
        // email — is not read as a citation of `b`).
        if i > 0 && is_key_char(chars[i - 1].1) {
            continue;
        }
        // Collect the identifier following '@'.
        let start = byte_idx + 1;
        let mut end = start;
        for (bi, cc) in src[start..].char_indices() {
            if is_key_char(cc) {
                end = start + bi + cc.len_utf8();
            } else {
                break;
            }
        }
        if end > start {
            // A trailing '.' or '-' is punctuation, not part of the key (Typst treats a
            // trailing dot as sentence punctuation). Trim them.
            let key = src[start..end].trim_end_matches(['.', '-']).to_string();
            if !key.is_empty() && seen.insert(key.clone()) {
                out.push(key);
            }
        }
        let _ = bytes; // silence unused in case of future refactor
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn round_trips() {
        let text = "name: Dissertation\ndocuments:\n- ~/Writing/main.typ\n- ~/Writing/ch3.typ\n";
        let pr = Project::parse(text, &PathBuf::from("projects/diss.yml")).unwrap();
        assert_eq!(pr.name, "Dissertation");
        assert_eq!(pr.documents.len(), 2);
        let reparsed = Project::parse(&pr.to_text().unwrap(), &PathBuf::from("x")).unwrap();
        assert_eq!(pr, reparsed);
    }

    #[test]
    fn scans_typst_citation_keys() {
        let src = "As @cone1970black argues #cite(<berdyaev1937destiny>), see also @cone1970black again.\nContact a@b.com is not a citation. End @gutierrez1971teologia.";
        let keys = scan_typst_citation_keys(src);
        // De-duplicated, first-seen order; the trailing '.' after the last key is trimmed;
        // the email `a@b.com` is not misread.
        assert_eq!(keys, vec!["cone1970black", "gutierrez1971teologia"]);
        assert!(!keys.contains(&"b.com".to_string()));
    }
}
