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
    #[serde(rename = "author", skip_serializing_if = "Vec::is_empty")]
    authors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    date: Option<String>,
    // A plain string when there's no location, or a `{name, location}` mapping when there
    // is — matching Hayagriva's own `Publisher` serialization. Built by `publisher_value`;
    // NOT two separate top-level `publisher`/`location` fields, which was this struct's
    // bug for a long time (fixed 2026-09-19) — Hayagriva's citation-style rendering reads
    // the place of publication from the nested `publisher.location`, not a top-level
    // `location:` (that instead feeds `event-place`, for a conference/exhibition entry).
    #[serde(skip_serializing_if = "Option::is_none")]
    publisher: Option<serde_yaml_ng::Value>,
    // Hayagriva's own `edition` field (CSL `edition`) — not `note`, which most styles never
    // render as an edition, so "2nd ed." used to never appear in a citation.
    #[serde(skip_serializing_if = "Option::is_none")]
    edition: Option<String>,
    #[serde(rename = "page-total", skip_serializing_if = "Option::is_none")]
    page_total: Option<u64>,
    #[serde(rename = "serial-number", skip_serializing_if = "Option::is_none")]
    serial_number: Option<Serials>,
}

#[derive(Serialize)]
struct Serials {
    isbn: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    oclc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lccn: Option<String>,
}

/// Build the `publisher:` YAML value from a name and/or location, matching Hayagriva's own
/// `Publisher` serialization: a plain string when there's no location, a `{name, location}`
/// mapping when there is. `None` when both are empty (field omitted entirely).
fn publisher_value(name: Option<&str>, location: Option<&str>) -> Option<serde_yaml_ng::Value> {
    use serde_yaml_ng::Value;
    let name = name.map(str::trim).filter(|s| !s.is_empty());
    let location = location.map(str::trim).filter(|s| !s.is_empty());
    match (name, location) {
        (None, None) => None,
        (Some(name), None) => Some(Value::String(name.to_string())),
        (name, Some(location)) => {
            let mut map = serde_yaml_ng::Mapping::new();
            if let Some(name) = name {
                map.insert(
                    Value::String("name".to_string()),
                    Value::String(name.to_string()),
                );
            }
            map.insert(
                Value::String("location".to_string()),
                Value::String(location.to_string()),
            );
            Some(Value::Mapping(map))
        }
    }
}

fn to_yaml_doc(entry: AcqEntry) -> Result<String> {
    let doc: BTreeMap<&str, AcqEntry> = BTreeMap::from([("_", entry)]);
    serde_yaml_ng::to_string(&doc).map_err(|e| BibError::Import {
        message: format!("could not build entry YAML: {e}"),
    })
}

/// A minimal Hayagriva YAML document (placeholder key `_`) from a title, optional author, and
/// optional ISBN — the fallback when a dropped PDF has no DOI, or has an ISBN that a network
/// lookup could not enrich, but does have embedded metadata. `isbn`, when present, is still
/// recorded under `serial-number` even though the richer OpenLibrary fields (date/publisher/
/// location/page count) could not be fetched, so it isn't lost and can be matched or looked up
/// again later.
pub fn minimal_book_yaml(title: &str, author: Option<&str>, isbn: Option<&str>) -> Result<String> {
    let isbn = isbn.map(str::trim).filter(|s| !s.is_empty());
    to_yaml_doc(AcqEntry {
        entry_type: "book".to_string(),
        title: title.to_string(),
        authors: author
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(|a| vec![a.to_string()])
            .unwrap_or_default(),
        date: None,
        publisher: None,
        edition: None,
        page_total: None,
        serial_number: isbn.map(|i| Serials {
            isbn: i.to_string(),
            oclc: None,
            lccn: None,
        }),
    })
}

/// A Hayagriva YAML document (placeholder key `_`) built from plain book fields — used to
/// seed an entry from EPUB OPF metadata when no ISBN lookup is available (or it failed).
/// `creators`' author/editor/affiliated split is written via
/// [`crate::entry::write_creators_into`]; empty/`None` fields are omitted.
pub fn book_yaml(
    title: &str,
    creators: &[crate::creator::Creator],
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
    crate::entry::write_creators_into(&mut inner, creators)?;
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
///
/// OpenLibrary's old "Books API" (`api/books?bibkeys=...&jscmd=data`) now returns a bare
/// HTTP 404 for every ISBN — confirmed live 2026-09-19, not a transient outage — so this
/// goes through the per-edition endpoint instead: `GET /isbn/{isbn}.json` (a redirect to
/// `/books/OL...M.json`), which is still live. That endpoint's JSON shape is different from
/// the old "data" format `isbn_json_to_yaml` parses (authors are `{"key": "/authors/..."}`
/// references, not `{"name": ...}`; publishers/publish_places are plain strings, not
/// `{"name": ...}` objects) — `edition_json_to_data_shape` resolves the author keys with one
/// extra request each and reshapes the rest, so `isbn_json_to_yaml` doesn't need to change.
pub fn fetch_isbn_yaml(isbn: &str) -> Result<String> {
    let isbn = isbn.trim().replace(['-', ' '], "");
    if isbn.is_empty() {
        return Err(BibError::Import {
            message: "empty ISBN".to_string(),
        });
    }
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| net_err("could not build HTTP client", e))?;

    let edition_url = format!("https://openlibrary.org/isbn/{isbn}.json");
    let response = client
        .get(&edition_url)
        .send()
        .map_err(|e| net_err("ISBN request failed", e))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(BibError::Import {
            message: format!("ISBN '{isbn}' not found in OpenLibrary"),
        });
    }
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
    let edition: serde_json::Value = serde_json::from_str(&body).map_err(|e| BibError::Import {
        message: format!("could not parse OpenLibrary response: {e}"),
    })?;

    let data_shaped = edition_json_to_data_shape(&edition, &client)?;
    let wrapped = serde_json::json!({ format!("ISBN:{isbn}"): data_shaped }).to_string();
    isbn_json_to_yaml(&wrapped, &isbn)
}

/// Reshape an OpenLibrary per-edition record (`/isbn/{isbn}.json` / `/books/OL...M.json`)
/// into the old Books API "data" shape that `isbn_json_to_yaml` parses: resolves each
/// `authors[].key` to a name via `/authors/{key}.json`, and wraps `publishers`/
/// `publish_places` (plain strings in the edition shape) as `{"name": ...}` objects.
fn edition_json_to_data_shape(
    edition: &serde_json::Value,
    client: &reqwest::blocking::Client,
) -> Result<serde_json::Value> {
    let mut out = edition.clone();
    let map = out.as_object_mut().ok_or_else(|| BibError::Import {
        message: "OpenLibrary response was not a JSON object".to_string(),
    })?;

    // Author keys: the edition's own, or — when the edition lists none — its work's (an
    // edition record often carries no `authors` at all, which used to give an authorless
    // entry with no hint why).
    let mut author_keys: Vec<String> = map
        .get("authors")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("key").and_then(|k| k.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if author_keys.is_empty() {
        let work_key = map
            .get("works")
            .and_then(|w| w.as_array())
            .and_then(|w| w.first())
            .and_then(|w| w.get("key"))
            .and_then(|k| k.as_str())
            .map(str::to_string);
        if let Some(work_key) = work_key {
            let work = fetch_json(client, &format!("https://openlibrary.org{work_key}.json"))?;
            author_keys = work
                .get("authors")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| {
                            x.get("author")
                                .and_then(|a| a.get("key"))
                                .or_else(|| x.get("key"))
                                .and_then(|k| k.as_str())
                                .map(str::to_string)
                        })
                        .collect()
                })
                .unwrap_or_default();
        }
    }
    let mut resolved = Vec::with_capacity(author_keys.len());
    for key in &author_keys {
        // A failed lookup is an error, not a silently missing author: a network blip used
        // to yield a book quietly credited to only some of its authors.
        let author = fetch_json(client, &format!("https://openlibrary.org{key}.json"))?;
        if let Some(name) = author.get("name").and_then(|n| n.as_str()) {
            resolved.push(serde_json::json!({ "name": name }));
        }
    }
    map.insert("authors".to_string(), serde_json::Value::Array(resolved));

    apply_edition_extras(map);
    Ok(serde_json::Value::Object(map.clone()))
}

fn fetch_json(client: &reqwest::blocking::Client, url: &str) -> Result<serde_json::Value> {
    let response = client
        .get(url)
        .send()
        .map_err(|e| net_err(&format!("OpenLibrary request to {url} failed"), e))?;
    if !response.status().is_success() {
        return Err(BibError::Import {
            message: format!(
                "OpenLibrary request to {url} failed: HTTP {}",
                response.status()
            ),
        });
    }
    response.json::<serde_json::Value>().map_err(|e| {
        net_err(
            &format!("could not read OpenLibrary response from {url}"),
            e,
        )
    })
}

/// The network-free half of reshaping an edition record into the "data" shape
/// [`isbn_json_to_yaml`] parses: wrap `publishers`/`publish_places` (plain strings on an
/// edition) as `{name}` objects, append the `subtitle` to the title, and gather identifiers
/// (an edition keeps `oclc_numbers`/`lccn` at the top level, not under `identifiers`).
fn apply_edition_extras(map: &mut serde_json::Map<String, serde_json::Value>) {
    use serde_json::{json, Value};

    for field in ["publishers", "publish_places"] {
        if let Some(arr) = map.get(field).and_then(|v| v.as_array()).cloned() {
            let wrapped: Vec<Value> = arr
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| json!({ "name": s }))
                .collect();
            map.insert(field.to_string(), Value::Array(wrapped));
        }
    }

    // "Title: Subtitle" — Chicago/SBL want the subtitle, and the edition record keeps it in a
    // separate field that the mapping used to ignore.
    if let (Some(title), Some(subtitle)) = (
        map.get("title")
            .and_then(|t| t.as_str())
            .map(str::to_string),
        map.get("subtitle")
            .and_then(|t| t.as_str())
            .map(str::trim)
            .map(str::to_string),
    ) {
        if !subtitle.is_empty() && !title.to_lowercase().contains(&subtitle.to_lowercase()) {
            let sep = if title.trim_end().ends_with(['?', '!', ':', '.']) {
                " "
            } else {
                ": "
            };
            map.insert(
                "title".to_string(),
                json!(format!("{}{sep}{subtitle}", title.trim_end())),
            );
        }
    }

    let mut identifiers = map
        .get("identifiers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    for (top, ident) in [("oclc_numbers", "oclc"), ("lccn", "lccn")] {
        if !identifiers.contains_key(ident) {
            if let Some(arr) = map.get(top).filter(|v| v.is_array()) {
                identifiers.insert(ident.to_string(), arr.clone());
            }
        }
    }
    map.insert("identifiers".to_string(), Value::Object(identifiers));
}

/// Minimum plausible size (bytes) for a real OpenLibrary cover JPEG at size `M`. A
/// defensive fallback alongside `default=false` below, in case a "success" response is
/// still a tiny placeholder image rather than a real cover.
const MIN_COVER_BYTES: usize = 1000;

/// Fetch a book's cover image (JPEG bytes) from the OpenLibrary Covers API, at size `M`
/// (medium — right-sized for a grid thumbnail). Returns `Ok(None)` — not an error — when
/// OpenLibrary has no cover for this ISBN. `default=false` asks the API to answer with a
/// plain HTTP 404 in that case instead of its own "no cover" placeholder graphic.
pub fn fetch_isbn_cover(isbn: &str) -> Result<Option<Vec<u8>>> {
    let isbn = isbn.trim().replace(['-', ' '], "");
    if isbn.is_empty() {
        return Err(BibError::Import {
            message: "empty ISBN".to_string(),
        });
    }
    let url = format!("https://covers.openlibrary.org/b/isbn/{isbn}-M.jpg?default=false");
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| net_err("could not build HTTP client", e))?;
    let response = client
        .get(&url)
        .send()
        .map_err(|e| net_err("cover request failed", e))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(BibError::Import {
            message: format!(
                "cover lookup for '{isbn}' failed: HTTP {}",
                response.status()
            ),
        });
    }
    let bytes = response
        .bytes()
        .map_err(|e| net_err("could not read cover response", e))?
        .to_vec();
    if bytes.len() < MIN_COVER_BYTES {
        return Ok(None);
    }
    Ok(Some(bytes))
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

    // OpenLibrary gives natural-order full names ("Desmond Lee"); reformat each through
    // `Creator::from_natural_text` (splits on the last whitespace: last token = family) so
    // this matches the `"Family, Given"` convention used everywhere else in the app, and so
    // multiple authors land as separate sequence entries rather than one bogus joined string.
    let authors: Vec<String> = record
        .get("authors")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
                .map(|name| {
                    crate::creator::Creator::from_natural_text(
                        crate::creator::CreatorRole::Author,
                        name,
                    )
                    .display_line()
                })
                .collect()
        })
        .unwrap_or_default();

    let date = record
        .get("publish_date")
        .and_then(|v| v.as_str())
        .and_then(openlibrary_date_to_hayagriva);

    let publisher = record
        .get("publishers")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string());

    let location = record
        .get("publish_places")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string());

    let note = record
        .get("edition_name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let page_total = record.get("number_of_pages").and_then(|v| v.as_u64());

    let identifiers = record.get("identifiers");
    let identifier = |key: &str| {
        identifiers
            .and_then(|v| v.get(key))
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };

    let entry = AcqEntry {
        entry_type: "book".to_string(),
        title,
        authors,
        date,
        publisher: publisher_value(publisher.as_deref(), location.as_deref()),
        edition: note,
        page_total,
        serial_number: Some(Serials {
            isbn: isbn.to_string(),
            oclc: identifier("oclc"),
            lccn: identifier("lccn"),
        }),
    };
    to_yaml_doc(entry)
}

/// Best-effort parse of OpenLibrary's freeform `publish_date` (e.g. `"October 2007"`,
/// `"Oct 01, 2007"`, `"2007"`) into a string Hayagriva's date parser accepts at whatever
/// precision the source actually supports: `YYYY-MM-DD`, `YYYY-MM`, or bare `YYYY`. `None`
/// if no 4-digit year can be found at all.
fn openlibrary_date_to_hayagriva(text: &str) -> Option<String> {
    let year = year_from_text(text)?;
    let mut month: Option<u8> = None;
    let mut numeric_others: Vec<u8> = Vec::new();

    for word in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        if word.len() == 4 && word.chars().all(|c| c.is_ascii_digit()) {
            continue; // the year itself
        }
        if let Ok(n) = word.parse::<u8>() {
            numeric_others.push(n);
            continue;
        }
        if month.is_none() {
            month = month_number(word);
        }
    }

    // No month name, but exactly one other number in 1..=12: a numeric month (e.g. "10/2007"),
    // not a day — a lone day-of-month with no month at all wouldn't appear in the wild.
    if month.is_none() && numeric_others.len() == 1 && (1..=12).contains(&numeric_others[0]) {
        month = numeric_others.pop();
    }
    let day = numeric_others.into_iter().find(|d| (1..=31).contains(d));

    Some(match (month, day) {
        (Some(m), Some(d)) => format!("{year:04}-{m:02}-{d:02}"),
        (Some(m), None) => format!("{year:04}-{m:02}"),
        _ => format!("{year:04}"),
    })
}

/// English month name (or a >=3 letter abbreviation of one) to its 1-12 number.
fn month_number(word: &str) -> Option<u8> {
    const NAMES: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    let lower = word.to_ascii_lowercase();
    if lower.len() < 3 {
        return None;
    }
    NAMES
        .iter()
        .position(|n| n.starts_with(&lower))
        .map(|i| (i + 1) as u8)
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
    fn minimal_book_yaml_carries_isbn_when_present() {
        let yaml =
            minimal_book_yaml("The Republic", Some("Plato"), Some(" 978-0-14-044913-6 ")).unwrap();
        assert!(yaml.contains("title: The Republic"));
        assert!(
            yaml.contains("author:") && yaml.contains("- Plato"),
            "got: {yaml}"
        );
        assert!(yaml.contains("isbn: 978-0-14-044913-6"), "got: {yaml}");
        assert!(!yaml.contains("date:"));
        assert!(!yaml.contains("publisher:"));
    }

    #[test]
    fn minimal_book_yaml_omits_serial_number_without_isbn() {
        let yaml = minimal_book_yaml("Some Book", None, None).unwrap();
        assert!(!yaml.contains("serial-number"));
    }

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
                "publish_places": [{"name": "London"}],
                "edition_name": "Revised edition",
                "number_of_pages": 496,
                "identifiers": {"oclc": ["123456"], "lccn": ["2007123456"]}
            }
        }"#;
        let yaml = isbn_json_to_yaml(json, "9780140449136").unwrap();
        assert!(yaml.contains("type: book"));
        assert!(yaml.contains("title: The Republic"));
        // Each OpenLibrary name lands as its own sequence entry, reformatted to the
        // "Family, Given" convention ("Desmond Lee" -> "Lee, Desmond") rather than joined
        // into a single bogus "Plato and Desmond Lee" string.
        assert!(yaml.contains("- Plato"), "got: {yaml}");
        assert!(yaml.contains("- Lee, Desmond"), "got: {yaml}");
        // Month is preserved, not truncated down to the bare year.
        assert!(
            yaml.contains("date: 2007-10") || yaml.contains("date: '2007-10'"),
            "got: {yaml}"
        );
        // Publisher name/location must land nested under one `publisher:` mapping (the shape
        // Hayagriva's citation-style rendering actually reads the "place of publication"
        // from — `publisher-place`/Zotero's "Place" — reads `publisher.location`, not a
        // top-level `location:` on the entry, which feeds a different CSL variable
        // (`event-place`). A prior version of this code wrote it at the top level and
        // citations silently rendered without a location as a result.
        assert!(yaml.contains("publisher:"), "got: {yaml}");
        assert!(yaml.contains("name: Penguin Classics"), "got: {yaml}");
        assert!(yaml.contains("location: London"), "got: {yaml}");
        // A top-level `location:` (2-space indent, sibling of `publisher:`) would be the old,
        // wrong shape — it must only appear nested under `publisher:` (4-space indent).
        assert!(
            !yaml.lines().any(|l| l == "  location: London"),
            "location must not be a top-level field: {yaml}"
        );
        assert!(yaml.contains("edition: Revised edition"), "got: {yaml}");
        assert!(yaml.contains("page-total: 496"));
        assert!(yaml.contains("isbn: '9780140449136'") || yaml.contains("isbn: \"9780140449136\""));
        assert!(yaml.contains("oclc: '123456'") || yaml.contains("oclc: 123456"));
        assert!(yaml.contains("lccn: '2007123456'") || yaml.contains("lccn: 2007123456"));
    }

    #[test]
    fn edition_record_extras_subtitle_identifiers_publishers() {
        let mut map = serde_json::json!({
            "title": "The Republic",
            "subtitle": "A dialogue on justice",
            "publishers": ["Penguin"],
            "publish_places": ["London"],
            "oclc_numbers": ["123456"],
            "lccn": ["2007123456"]
        })
        .as_object()
        .unwrap()
        .clone();
        apply_edition_extras(&mut map);
        assert_eq!(map["title"], "The Republic: A dialogue on justice");
        assert_eq!(map["publishers"][0]["name"], "Penguin");
        assert_eq!(map["publish_places"][0]["name"], "London");
        assert_eq!(map["identifiers"]["oclc"][0], "123456");
        assert_eq!(map["identifiers"]["lccn"][0], "2007123456");

        // A subtitle already in the title isn't repeated; one after a `?` gets a space.
        let mut m = serde_json::json!({"title": "Why? ", "subtitle": "Because"})
            .as_object()
            .unwrap()
            .clone();
        apply_edition_extras(&mut m);
        assert_eq!(m["title"], "Why? Because");
        let mut m = serde_json::json!({"title": "Dune: Messiah", "subtitle": "messiah"})
            .as_object()
            .unwrap()
            .clone();
        apply_edition_extras(&mut m);
        assert_eq!(m["title"], "Dune: Messiah");
    }

    #[test]
    fn isbn_yaml_edition_is_a_real_hayagriva_field_and_round_trips() {
        let json = r#"{"ISBN:1": {"title": "T", "authors": [{"name": "A B"}],
            "publish_date": "2001", "edition_name": "2nd ed.",
            "publishers": [{"name": "P"}], "publish_places": [{"name": "Rome"}]}}"#;
        let yaml = isbn_json_to_yaml(json, "1").unwrap();
        let parsed =
            crate::entry::parse_single(&yaml.replace("_:", "k:"), std::path::Path::new("x"))
                .expect("generated YAML must be valid Hayagriva");
        assert!(crate::entry::serialize_entry(&parsed.entry)
            .unwrap()
            .contains("edition: 2nd ed."));
        let f = crate::entry::read_fields(&parsed.entry);
        assert_eq!((f.publisher.as_str(), f.location.as_str()), ("P", "Rome"));
    }

    #[test]
    fn openlibrary_date_captures_month_and_day_when_present() {
        assert_eq!(
            openlibrary_date_to_hayagriva("October 2007"),
            Some("2007-10".to_string())
        );
        assert_eq!(
            openlibrary_date_to_hayagriva("Oct 01, 2007"),
            Some("2007-10-01".to_string())
        );
        assert_eq!(
            openlibrary_date_to_hayagriva("10/2007"),
            Some("2007-10".to_string())
        );
        assert_eq!(
            openlibrary_date_to_hayagriva("2007"),
            Some("2007".to_string())
        );
        assert_eq!(openlibrary_date_to_hayagriva("no date here"), None);
    }
}
