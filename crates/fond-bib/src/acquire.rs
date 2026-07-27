//! Network acquisition of bibliographic metadata (Milestone 5). Feature-gated behind
//! `acquire` so lean consumers of `fond-bib` do not pull in `reqwest`. Uses a **blocking**
//! HTTP client on the calling thread — no async runtime (`docs/ARCHITECTURE.md` §3).
//!
//! Strategy: resolve a DOI via doi.org content negotiation asking for BibTeX, then reuse
//! the existing BibLaTeX parser (`Library::add_bibtex`) to turn it into an entry. This
//! keeps one metadata-mapping path instead of a second bespoke JSON mapper.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;

use crate::error::{BibError, Result};

const USER_AGENT: &str = concat!("kartoteka/", env!("CARGO_PKG_VERSION"));

fn net_err(context: &str, e: impl std::fmt::Display) -> BibError {
    BibError::Import {
        message: format!("{context}: {e}"),
    }
}

/// Strip common prefixes from a DOI string, leaving the bare `10.xxxx/...` form.
pub fn normalize_doi(doi: &str) -> String {
    let d = doi.trim();
    for prefix in [
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
        "doi:",
    ] {
        if let Some(rest) = d.strip_prefix(prefix) {
            return rest.trim().to_string();
        }
    }
    d.to_string()
}

/// Fetch a BibTeX record for a DOI via doi.org content negotiation
/// (`Accept: application/x-bibtex`). Returns the raw BibTeX string.
pub fn fetch_doi_bibtex(doi: &str) -> Result<String> {
    let doi = normalize_doi(doi);
    if doi.is_empty() {
        return Err(BibError::Import {
            message: "empty DOI".to_string(),
        });
    }
    let url = format!("https://doi.org/{doi}");

    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| net_err("could not build HTTP client", e))?;

    let response = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/x-bibtex")
        .send()
        .map_err(|e| net_err("DOI request failed", e))?;

    let status = response.status();
    if !status.is_success() {
        return Err(BibError::Import {
            message: format!("DOI lookup for '{doi}' failed: HTTP {status}"),
        });
    }

    let body = response
        .text()
        .map_err(|e| net_err("could not read DOI response", e))?;
    if body.trim().is_empty() {
        return Err(BibError::Import {
            message: format!("DOI '{doi}' returned no BibTeX record"),
        });
    }
    Ok(brace_bare_values(&body))
}

/// doi.org sometimes returns BibTeX with unquoted, unbraced field values such as
/// `month=July`, which the biblatex parser treats as an unknown string abbreviation and
/// rejects. Brace any bare alphabetic value (`=July` → `={July}`) so parsing succeeds.
///
/// Brace-aware and quote-aware: already-`{…}`/`"…"` values are copied verbatim (so `=`
/// inside a URL is never touched), and numeric values / page ranges are left alone. Works
/// on single-line records (which is how doi.org actually returns them).
fn brace_bare_values(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let len = chars.len();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;

    while i < len {
        let c = chars[i];
        out.push(c);
        i += 1;
        if c != '=' {
            continue; // structural chars (incl. entry braces) copied as-is
        }

        // A field value follows. Preserve whitespace, then handle by the value's shape.
        while i < len && chars[i].is_whitespace() {
            out.push(chars[i]);
            i += 1;
        }
        if i >= len {
            break;
        }
        match chars[i] {
            // Already braced: copy the balanced group verbatim (protects any inner `=`).
            '{' => {
                let mut depth = 0;
                while i < len {
                    let ch = chars[i];
                    out.push(ch);
                    i += 1;
                    if ch == '{' {
                        depth += 1;
                    } else if ch == '}' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
            }
            // Already quoted: copy verbatim.
            '"' => {
                out.push('"');
                i += 1;
                while i < len {
                    let ch = chars[i];
                    out.push(ch);
                    i += 1;
                    if ch == '"' {
                        break;
                    }
                }
            }
            // Bare word (e.g. `month=July`): brace it.
            ch if ch.is_ascii_alphabetic() => {
                let start = i;
                while i < len && !matches!(chars[i], ',' | '}' | '{' | '"') {
                    i += 1;
                }
                let raw: String = chars[start..i].iter().collect();
                let val = raw.trim_end();
                out.push('{');
                out.push_str(val);
                out.push('}');
                out.push_str(&raw[val.len()..]); // preserve trailing whitespace
            }
            // Numeric or other: leave for the normal loop.
            _ => {}
        }
    }
    out
}

/// Strip prefixes from an arXiv id, leaving the bare id (e.g. `2103.12345` or
/// `math/0211159`, optionally with a version suffix).
pub fn normalize_arxiv(id: &str) -> String {
    let d = id.trim();
    for prefix in [
        "https://arxiv.org/abs/",
        "http://arxiv.org/abs/",
        "arxiv:",
        "arXiv:",
    ] {
        if let Some(rest) = d.strip_prefix(prefix) {
            return rest.trim().to_string();
        }
    }
    d.to_string()
}

/// Fetch BibTeX for an arXiv paper by routing through its DataCite DOI
/// (`10.48550/arXiv.<id>`), which doi.org resolves to a BibTeX record. Works for papers
/// with a registered arXiv DOI (all since 2022, and many earlier ones).
pub fn fetch_arxiv_bibtex(id: &str) -> Result<String> {
    let id = normalize_arxiv(id);
    if id.is_empty() {
        return Err(BibError::Import {
            message: "empty arXiv id".to_string(),
        });
    }
    fetch_doi_bibtex(&format!("10.48550/arXiv.{id}"))
}

// --- ISBN via OpenLibrary ---

#[derive(Serialize)]
struct AcqEntry {
    #[serde(rename = "type")]
    entry_type: String,
    title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    author: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    publisher: Option<String>,
    #[serde(rename = "page-total", skip_serializing_if = "Option::is_none")]
    page_total: Option<u64>,
    #[serde(rename = "serial-number", skip_serializing_if = "Option::is_none")]
    serial_number: Option<Serials>,
}

#[derive(Serialize)]
struct Serials {
    isbn: String,
}

fn to_yaml_doc(entry: AcqEntry) -> Result<String> {
    let doc: BTreeMap<&str, AcqEntry> = BTreeMap::from([("_", entry)]);
    serde_yaml_ng::to_string(&doc).map_err(|e| BibError::Import {
        message: format!("could not build entry YAML: {e}"),
    })
}

/// A minimal Hayagriva YAML document (placeholder key `_`) from a title and optional
/// author — the fallback when a dropped PDF has no DOI but does have embedded metadata.
pub fn minimal_book_yaml(title: &str, author: Option<&str>) -> Result<String> {
    to_yaml_doc(AcqEntry {
        entry_type: "book".to_string(),
        title: title.to_string(),
        author: author.unwrap_or_default().to_string(),
        date: None,
        publisher: None,
        page_total: None,
        serial_number: None,
    })
}

/// A Hayagriva YAML document (placeholder key `_`) built from plain book fields — used to
/// seed an entry from EPUB OPF metadata when no ISBN lookup is available (or it failed).
/// `authors` are each `Family, Given` (or display) form; empty/`None` fields are omitted.
pub fn book_yaml(
    title: &str,
    authors: &[String],
    date: Option<&str>,
    publisher: Option<&str>,
    isbn: Option<&str>,
) -> Result<String> {
    use serde_yaml_ng::{Mapping, Value};
    let s = |v: &str| Value::String(v.to_string());
    fn clean(o: Option<&str>) -> Option<&str> {
        o.map(str::trim).filter(|v| !v.is_empty())
    }

    let mut inner = Mapping::new();
    inner.insert(s("type"), s("book"));
    inner.insert(s("title"), s(title.trim()));
    let authors: Vec<Value> = authors
        .iter()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty())
        .map(s)
        .collect();
    if !authors.is_empty() {
        inner.insert(s("author"), Value::Sequence(authors));
    }
    if let Some(d) = clean(date) {
        inner.insert(s("date"), s(d));
    }
    if let Some(p) = clean(publisher) {
        inner.insert(s("publisher"), s(p));
    }
    if let Some(i) = clean(isbn) {
        let mut sn = Mapping::new();
        sn.insert(s("isbn"), s(i));
        inner.insert(s("serial-number"), Value::Mapping(sn));
    }

    let mut doc = Mapping::new();
    doc.insert(s("_"), Value::Mapping(inner));
    serde_yaml_ng::to_string(&Value::Mapping(doc)).map_err(|e| BibError::Import {
        message: format!("could not build entry YAML: {e}"),
    })
}

/// Fetch book metadata for an ISBN from OpenLibrary and return it as a Hayagriva YAML
/// document (placeholder key `_`, since the caller regenerates the key).
pub fn fetch_isbn_yaml(isbn: &str) -> Result<String> {
    let isbn = isbn.trim().replace(['-', ' '], "");
    if isbn.is_empty() {
        return Err(BibError::Import {
            message: "empty ISBN".to_string(),
        });
    }
    let url =
        format!("https://openlibrary.org/api/books?bibkeys=ISBN:{isbn}&format=json&jscmd=data");
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| net_err("could not build HTTP client", e))?;
    let response = client
        .get(&url)
        .send()
        .map_err(|e| net_err("ISBN request failed", e))?;
    if !response.status().is_success() {
        return Err(BibError::Import {
            message: format!(
                "ISBN lookup for '{isbn}' failed: HTTP {}",
                response.status()
            ),
        });
    }
    let body = response
        .text()
        .map_err(|e| net_err("could not read ISBN response", e))?;
    isbn_json_to_yaml(&body, &isbn)
}

/// Map an OpenLibrary `jscmd=data` JSON response to a Hayagriva YAML document. Separated
/// from the network fetch so it can be tested offline.
pub fn isbn_json_to_yaml(json: &str, isbn: &str) -> Result<String> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| BibError::Import {
        message: format!("could not parse OpenLibrary response: {e}"),
    })?;

    // The response is keyed by "ISBN:<isbn>"; take that entry, or the only one present.
    let record = value
        .get(format!("ISBN:{isbn}"))
        .or_else(|| value.as_object().and_then(|o| o.values().next()))
        .ok_or_else(|| BibError::Import {
            message: format!("ISBN '{isbn}' not found in OpenLibrary"),
        })?;

    let title = record
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| BibError::Import {
            message: format!("OpenLibrary record for ISBN '{isbn}' has no title"),
        })?
        .to_string();

    let author = record
        .get("authors")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
                .collect::<Vec<_>>()
                .join(" and ")
        })
        .unwrap_or_default();

    let date = record
        .get("publish_date")
        .and_then(|v| v.as_str())
        .and_then(year_from_text)
        .map(|y| y.to_string());

    let publisher = record
        .get("publishers")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string());

    let page_total = record.get("number_of_pages").and_then(|v| v.as_u64());

    let entry = AcqEntry {
        entry_type: "book".to_string(),
        title,
        author,
        date,
        publisher,
        page_total,
        serial_number: Some(Serials {
            isbn: isbn.to_string(),
        }),
    };
    to_yaml_doc(entry)
}

fn year_from_text(s: &str) -> Option<i32> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if bytes[i..i + 4].iter().all(|b| b.is_ascii_digit())
            && (i == 0 || !bytes[i - 1].is_ascii_digit())
            && (i + 4 == bytes.len() || !bytes[i + 4].is_ascii_digit())
        {
            return s[i..i + 4].parse().ok();
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_doi_forms() {
        assert_eq!(normalize_doi("https://doi.org/10.1/abc"), "10.1/abc");
        assert_eq!(normalize_doi("doi:10.2/x"), "10.2/x");
        assert_eq!(normalize_doi("  10.3/y  "), "10.3/y");
    }

    #[test]
    fn braces_bare_values_in_single_line_record() {
        // The real doi.org shape: one line, braces around most values, bare `month`, and a
        // URL containing `=`-free but brace-wrapped content.
        let input = "@inproceedings{Akiba_2019, title={Optuna}, url={http://dx.doi.org/10.1145/3292500.3330701}, publisher={ACM}, year={2019}, month=July, pages={2623-2631} }";
        let out = brace_bare_values(input);
        assert!(out.contains("month={July}"), "bare month braced: {out}");
        assert!(
            out.contains("url={http://dx.doi.org/10.1145/3292500.3330701}"),
            "url intact: {out}"
        );
        assert!(out.contains("year={2019}"), "year intact: {out}");
        assert!(out.contains("title={Optuna}"), "title intact: {out}");
    }

    #[test]
    fn braces_bare_value_before_closing_brace() {
        let out = brace_bare_values("@a{k, publisher=ACM }");
        assert!(out.contains("publisher={ACM}"), "got: {out}");
    }

    #[test]
    fn normalizes_arxiv_forms() {
        assert_eq!(normalize_arxiv("arXiv:2103.12345"), "2103.12345");
        assert_eq!(
            normalize_arxiv("https://arxiv.org/abs/2103.12345"),
            "2103.12345"
        );
        assert_eq!(normalize_arxiv("math/0211159"), "math/0211159");
    }

    #[test]
    fn maps_openlibrary_json_to_yaml() {
        let json = r#"{
            "ISBN:9780140449136": {
                "title": "The Republic",
                "authors": [{"name": "Plato"}, {"name": "Desmond Lee"}],
                "publish_date": "October 2007",
                "publishers": [{"name": "Penguin Classics"}],
                "number_of_pages": 496
            }
        }"#;
        let yaml = isbn_json_to_yaml(json, "9780140449136").unwrap();
        assert!(yaml.contains("type: book"));
        assert!(yaml.contains("title: The Republic"));
        assert!(yaml.contains("Plato and Desmond Lee"));
        assert!(
            yaml.contains("date: '2007'")
                || yaml.contains("date: \"2007\"")
                || yaml.contains("date: 2007")
        );
        assert!(yaml.contains("Penguin Classics"));
        assert!(yaml.contains("page-total: 496"));
        assert!(yaml.contains("isbn: '9780140449136'") || yaml.contains("isbn: \"9780140449136\""));
    }
}
