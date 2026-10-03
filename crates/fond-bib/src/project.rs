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

/// Extract the citation keys referenced in Typst source.
///
/// Two forms are recognised:
///
/// - `@key` — Typst's reference syntax: `@` followed by an identifier of letters, digits,
///   `_`, `-` and `.`, per Typst's label grammar. A leading `@` inside an email-like run
///   (`foo@bar`) is avoided by requiring the `@` to be at a boundary.
/// - `#cite(<key>)` — the function form, including `#cite(<key>, supplement: [p. 3])` and
///   `#cite(form: "prose", <key>)`. A `<label>` elsewhere in a document *defines* a label
///   (`= Intro <intro>`) and is not a citation, so `<key>` only counts inside a `cite(...)`
///   call's argument list.
///
/// Returns keys in first-seen order, de-duplicated.
pub fn scan_typst_citation_keys(src: &str) -> Vec<String> {
    let mut found: Vec<(usize, String)> = scan_at_keys(src);
    found.extend(scan_cite_calls(src));
    found.sort_by_key(|(pos, _)| *pos);

    let mut seen = std::collections::HashSet::new();
    found
        .into_iter()
        .filter_map(|(_, key)| seen.insert(key.clone()).then_some(key))
        .collect()
}

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// A trailing `.` or `-` is punctuation, not part of the key (Typst treats a trailing dot as
/// sentence punctuation).
fn trim_key(raw: &str) -> &str {
    raw.trim_end_matches(['.', '-'])
}

/// `@key` references, with their byte offsets.
fn scan_at_keys(src: &str) -> Vec<(usize, String)> {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let mut out = Vec::new();
    for (i, (byte_idx, c)) in chars.iter().enumerate() {
        if *c != '@' {
            continue;
        }
        // Boundary check: preceding char must not be an identifier char (so `a@b` — an
        // email — is not read as a citation of `b`).
        if i > 0 && is_key_char(chars[i - 1].1) {
            continue;
        }
        let start = byte_idx + 1;
        let len: usize = src[start..]
            .chars()
            .take_while(|c| is_key_char(*c))
            .map(char::len_utf8)
            .sum();
        let key = trim_key(&src[start..start + len]);
        if !key.is_empty() {
            out.push((*byte_idx, key.to_string()));
        }
    }
    out
}

/// `<key>` labels inside `cite(...)` calls, with the byte offset of the label.
fn scan_cite_calls(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = src[from..].find("cite(") {
        let call = from + rel;
        let args = call + "cite(".len();
        from = args;
        // `cite(` must start an identifier (`#cite(`, `{ cite(` — not `excite(`).
        if src[..call].chars().next_back().is_some_and(is_key_char) {
            continue;
        }
        let mut depth = 1usize;
        for (off, c) in src[args..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                '<' => {
                    let start = args + off + 1;
                    let len: usize = src[start..]
                        .chars()
                        .take_while(|c| is_key_char(*c))
                        .map(char::len_utf8)
                        .sum();
                    if src[start + len..].starts_with('>') {
                        let key = trim_key(&src[start..start + len]);
                        if !key.is_empty() {
                            out.push((args + off, key.to_string()));
                        }
                    }
                }
                _ => {}
            }
        }
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
        // the email `a@b.com` is not misread; the function form is found in its place.
        assert_eq!(
            keys,
            vec![
                "cone1970black",
                "berdyaev1937destiny",
                "gutierrez1971teologia"
            ]
        );
        assert!(!keys.contains(&"b.com".to_string()));
    }

    #[test]
    fn scans_cite_function_forms() {
        let src = concat!(
            "A @a1970x[p. 4] B #cite(<b1937y>) ",
            "C #cite(<c1999z>, supplement: [p. 3]) D @d2001w: ",
            "E #cite(form: \"prose\", <f2003u>) F #cite(<g2004t>, supplement: [see (p. 9)]) ",
            "G #{ cite(<h2005s>) }."
        );
        assert_eq!(
            scan_typst_citation_keys(src),
            vec!["a1970x", "b1937y", "c1999z", "d2001w", "f2003u", "g2004t", "h2005s"]
        );
    }

    #[test]
    fn label_definitions_and_lookalikes_are_not_citations() {
        // `<intro>` defines a label; `excite(<x>)` is not `cite(`; `<` as a comparison.
        let src = "= Intro <intro>\nSee #excite(<x1999a>) and #if 1 < 2 [yes]. #cite(<real2000b>)";
        assert_eq!(scan_typst_citation_keys(src), vec!["real2000b"]);
    }
}
