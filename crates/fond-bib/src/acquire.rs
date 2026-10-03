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

// --- Title search: Crossref (articles, chapters) + OpenLibrary (books) ---

/// How a search hit is fetched once chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateId {
    /// Fetch by DOI (`fetch_doi_bibtex`).
    Doi(String),
    /// Fetch by ISBN (`fetch_isbn_yaml`).
    Isbn(String),
}

/// One result of a title search, with just enough to recognise the right work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: CandidateId,
    pub title: String,
    /// `"Family, Given"` each, as the source gave them.
    pub authors: Vec<String>,
    pub year: String,
    /// Journal or book the work appears in; empty for a book itself.
    pub container: String,
    /// `"Article"`, `"Book"`, `"Chapter"`, … — a plain-language kind.
    pub kind: String,
}

impl Candidate {
    /// `Cone, J. H.; Smith, A. · 1970 · Christianity and Crisis` — the second line of a result
    /// row. Shows at most three authors, then "et al.".
    pub fn byline(&self) -> String {
        let mut authors = self
            .authors
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        if self.authors.len() > 3 {
            authors.push_str(" et al.");
        }
        [authors, self.year.clone(), self.container.clone()]
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// The search result on its own, as an entry: the title, authors, year and container the
/// result row showed, plus its DOI or ISBN. What gets added when the full record can't be
/// fetched, and the source [`fill_from_candidate`] draws on.
pub fn candidate_entry(candidate: &Candidate) -> Result<hayagriva::Entry> {
    use crate::records::{Kind, Work};
    use crate::{Creator, CreatorRole};

    let kind = match candidate.kind.as_str() {
        "Article" | "Preprint" => Kind::Article,
        "Book" => Kind::Book,
        "Chapter" => Kind::Chapter,
        "Conference paper" => Kind::Conference,
        "Thesis" => Kind::Thesis,
        "Report" => Kind::Report,
        _ => Kind::Misc,
    };
    let (doi, isbn) = match &candidate.id {
        CandidateId::Doi(doi) => (doi.clone(), String::new()),
        CandidateId::Isbn(isbn) => (String::new(), isbn.clone()),
    };
    let work = Work {
        kind: Some(kind),
        title: candidate.title.clone(),
        creators: candidate
            .authors
            .iter()
            .map(|a| Creator::from_natural_text(CreatorRole::Author, a))
            .collect(),
        date: candidate.year.clone(),
        container: candidate.container.clone(),
        doi,
        isbn,
        ..Work::default()
    };
    crate::entry::parse_all(&work.to_yaml("_")?, std::path::Path::new("<search result>"))?
        .into_iter()
        .next()
        .ok_or_else(|| BibError::Import {
            message: "could not build an entry from the search result".to_string(),
        })
}

/// Fill in what a fetched record left out — its title, its authors (when it names no authors
/// or editors), its date — from the search result it was chosen from. Crossref's BibTeX for
/// some books and chapters carries no `title` at all (only `journal`/`booktitle`) and no
/// author, even when the search result showed both; added as fetched, such a record had
/// neither and was refused as unkeyable.
pub fn fill_from_candidate(entry: &mut hayagriva::Entry, candidate: &Candidate) -> Result<()> {
    let found = candidate_entry(candidate)?;
    if !crate::entry::title_string(entry).is_some_and(|t| !t.trim().is_empty()) {
        if let Some(title) = found.title() {
            entry.set_title(title.clone());
        }
    }
    let has_people = entry.authors().is_some_and(|a| !a.is_empty())
        || entry.editors().is_some_and(|e| !e.is_empty());
    if !has_people {
        if let Some(authors) = found.authors() {
            entry.set_authors(authors.to_vec());
        }
    }
    if entry.date_any().is_none() {
        if let Some(date) = found.date() {
            entry.set_date(*date);
        }
    }
    Ok(())
}

/// Search for a work by title (or a pasted citation): up to `rows` articles/chapters from
/// Crossref followed by up to `rows` books from OpenLibrary, best match first within each. A
/// source that fails is skipped; the search only fails when *both* do (offline, say).
pub fn search_works(query: &str, rows: usize) -> Result<Vec<Candidate>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let client = search_client()?;
    let crossref = client
        .get("https://api.crossref.org/works")
        .query(&[
            ("query.bibliographic", query),
            ("rows", &rows.to_string()),
            ("select", "DOI,title,author,issued,container-title,type"),
        ])
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| net_err("Crossref search failed", e))
        .and_then(|r| {
            r.text()
                .map_err(|e| net_err("could not read Crossref reply", e))
        })
        .and_then(|body| parse_crossref_items(&body));
    let openlibrary = client
        .get("https://openlibrary.org/search.json")
        .query(&[
            ("q", query),
            ("limit", &rows.to_string()),
            (
                "fields",
                "title,author_name,first_publish_year,isbn,publisher",
            ),
        ])
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| net_err("OpenLibrary search failed", e))
        .and_then(|r| {
            r.text()
                .map_err(|e| net_err("could not read OpenLibrary reply", e))
        })
        .and_then(|body| parse_openlibrary_docs(&body));

    match (crossref, openlibrary) {
        (Err(e), Err(_)) => Err(e),
        (a, b) => Ok(a
            .unwrap_or_default()
            .into_iter()
            .chain(b.unwrap_or_default())
            .collect()),
    }
}

fn search_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| net_err("could not build HTTP client", e))
}

/// Parse Crossref's `/works` reply into candidates. Items without a DOI or title are dropped.
pub fn parse_crossref_items(json: &str) -> Result<Vec<Candidate>> {
    let root: serde_json::Value = serde_json::from_str(json).map_err(|e| BibError::Import {
        message: format!("unreadable Crossref reply: {e}"),
    })?;
    let items = root["message"]["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    Ok(items
        .iter()
        .filter_map(|item| {
            let doi = item["DOI"].as_str()?.trim().to_string();
            let title = plain_text(item["title"].as_array()?.first()?.as_str()?);
            if doi.is_empty() || title.is_empty() {
                return None;
            }
            let authors = item["author"]
                .as_array()
                .map(|people| {
                    people
                        .iter()
                        .filter_map(|p| {
                            match (
                                p["family"].as_str(),
                                p["given"].as_str(),
                                p["name"].as_str(),
                            ) {
                                (Some(f), Some(g), _) => Some(format!("{f}, {g}")),
                                (Some(f), None, _) => Some(f.to_string()),
                                (None, _, Some(n)) => Some(n.to_string()),
                                _ => None,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            let year = item["issued"]["date-parts"][0][0]
                .as_i64()
                .map(|y| y.to_string())
                .unwrap_or_default();
            let container = item["container-title"]
                .as_array()
                .and_then(|c| c.first())
                .and_then(|c| c.as_str())
                .map(plain_text)
                .unwrap_or_default();
            Some(Candidate {
                id: CandidateId::Doi(doi),
                title,
                authors,
                year,
                container,
                kind: crossref_kind(item["type"].as_str().unwrap_or("")).to_string(),
            })
        })
        .collect())
}

fn crossref_kind(t: &str) -> &'static str {
    match t {
        "journal-article" => "Article",
        "book-chapter" | "book-section" | "reference-entry" => "Chapter",
        "book" | "monograph" | "edited-book" | "reference-book" => "Book",
        "proceedings-article" => "Conference paper",
        "posted-content" => "Preprint",
        "dissertation" => "Thesis",
        "report" => "Report",
        _ => "Work",
    }
}

/// Parse OpenLibrary's `search.json` reply into book candidates, each identified by an ISBN
/// (ISBN-13 preferred). Works with no usable ISBN are dropped, since there'd be nothing to
/// fetch the full record by.
pub fn parse_openlibrary_docs(json: &str) -> Result<Vec<Candidate>> {
    let root: serde_json::Value = serde_json::from_str(json).map_err(|e| BibError::Import {
        message: format!("unreadable OpenLibrary reply: {e}"),
    })?;
    let docs = root["docs"].as_array().cloned().unwrap_or_default();
    Ok(docs
        .iter()
        .filter_map(|doc| {
            let title = plain_text(doc["title"].as_str()?);
            let isbns: Vec<&str> = doc["isbn"]
                .as_array()?
                .iter()
                .filter_map(|i| i.as_str())
                .collect();
            let isbn = isbns
                .iter()
                .find(|i| i.len() == 13)
                .or_else(|| isbns.first())?
                .to_string();
            if title.is_empty() {
                return None;
            }
            Some(Candidate {
                id: CandidateId::Isbn(isbn),
                title,
                authors: doc["author_name"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|n| n.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
                year: doc["first_publish_year"]
                    .as_i64()
                    .map(|y| y.to_string())
                    .unwrap_or_default(),
                container: String::new(),
                kind: "Book".to_string(),
            })
        })
        .collect())
}

/// Titles come back with inline markup (`<i>Kenosis</i>`, `&amp;`): reduce to plain text.
fn plain_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
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

#[cfg(test)]
mod search_tests {
    use super::*;

    const CROSSREF: &str = r#"{
      "status": "ok",
      "message": { "items": [
        { "DOI": "10.2307/1234", "type": "journal-article",
          "title": ["Black Theology and <i>Black</i> Power &amp; the Church"],
          "author": [ {"given": "James H.", "family": "Cone"}, {"family": "Smith"} ],
          "issued": {"date-parts": [[1970, 3]]},
          "container-title": ["Christianity and Crisis"] },
        { "DOI": "10.1000/org", "type": "report",
          "title": ["A Report"], "author": [ {"name": "World Council of Churches"} ],
          "issued": {"date-parts": [[null]]} },
        { "DOI": "10.1000/untitled", "type": "journal-article", "title": [] },
        { "title": ["No DOI"] },
        { "DOI": "10.1000/chapter", "type": "book-chapter", "title": ["On Kenosis"],
          "author": [ {"given": "A", "family": "One"}, {"given": "B", "family": "Two"},
                      {"given": "C", "family": "Three"}, {"given": "D", "family": "Four"} ],
          "container-title": ["Process Theology Reader"] }
      ] }
    }"#;

    const OPENLIBRARY: &str = r#"{ "numFound": 3, "docs": [
        { "title": "A Black Theology of Liberation", "author_name": ["James H. Cone"],
          "first_publish_year": 1970, "isbn": ["0883441039", "9780883441039", "9781570752526"] },
        { "title": "Only Ten", "isbn": ["0140449132"] },
        { "title": "No ISBN at all", "author_name": ["Nobody"] }
    ] }"#;

    #[test]
    fn crossref_items_become_candidates() {
        let found = parse_crossref_items(CROSSREF).unwrap();
        let titles: Vec<_> = found.iter().map(|c| c.title.as_str()).collect();
        // The untitled and DOI-less items are dropped.
        assert_eq!(
            titles,
            [
                "Black Theology and Black Power & the Church",
                "A Report",
                "On Kenosis"
            ]
        );
        let first = &found[0];
        assert_eq!(first.id, CandidateId::Doi("10.2307/1234".into()));
        assert_eq!(first.authors, ["Cone, James H.", "Smith"]);
        assert_eq!(first.year, "1970");
        assert_eq!(first.container, "Christianity and Crisis");
        assert_eq!(first.kind, "Article");
        assert_eq!(
            first.byline(),
            "Cone, James H.; Smith · 1970 · Christianity and Crisis"
        );
    }

    #[test]
    fn sparse_and_unusual_items_do_not_break_parsing() {
        let found = parse_crossref_items(CROSSREF).unwrap();
        let report = &found[1];
        assert_eq!(report.authors, ["World Council of Churches"]);
        assert_eq!(report.year, "", "a null year is no year");
        assert_eq!(report.byline(), "World Council of Churches");
        assert_eq!(report.kind, "Report");
        assert_eq!(found[2].kind, "Chapter");
        assert_eq!(crossref_kind("dataset"), "Work");
        assert_eq!(crossref_kind(""), "Work");
    }

    #[test]
    fn a_long_author_list_is_abbreviated() {
        let chapter = &parse_crossref_items(CROSSREF).unwrap()[2];
        assert_eq!(chapter.authors.len(), 4);
        assert_eq!(
            chapter.byline(),
            "One, A; Two, B; Three, C et al. · Process Theology Reader"
        );
    }

    #[test]
    fn openlibrary_docs_become_book_candidates_identified_by_isbn() {
        let found = parse_openlibrary_docs(OPENLIBRARY).unwrap();
        assert_eq!(found.len(), 2, "the work with no ISBN is dropped");
        // ISBN-13 is preferred over the first-listed ISBN-10.
        assert_eq!(found[0].id, CandidateId::Isbn("9780883441039".into()));
        assert_eq!(found[0].kind, "Book");
        assert_eq!(found[0].byline(), "James H. Cone · 1970");
        // With only an ISBN-10 available, that is used.
        assert_eq!(found[1].id, CandidateId::Isbn("0140449132".into()));
    }

    #[test]
    fn replies_that_are_empty_or_not_json() {
        assert!(parse_crossref_items(r#"{"message":{"items":[]}}"#)
            .unwrap()
            .is_empty());
        assert!(parse_crossref_items("{}").unwrap().is_empty());
        assert!(parse_crossref_items("<html>rate limited</html>").is_err());
        assert!(parse_openlibrary_docs(r#"{"docs":[]}"#).unwrap().is_empty());
        assert!(parse_openlibrary_docs("nope").is_err());
    }

    #[test]
    fn titles_lose_markup_and_entities() {
        assert_eq!(
            plain_text("<i>Kenosis</i> &amp;  <b>Self</b>\n gift"),
            "Kenosis & Self gift"
        );
        assert_eq!(plain_text("a &lt; b"), "a < b");
    }

    #[test]
    fn an_empty_query_is_no_search_and_no_network() {
        assert!(search_works("   ", 5).unwrap().is_empty());
    }

    fn picked(id: CandidateId, kind: &str) -> Candidate {
        Candidate {
            id,
            title: "2. The Importance of The Brothers Karamazov".into(),
            authors: vec!["Jackson, Robert Louis".into()],
            year: "2017".into(),
            container: "The Brothers Karamazov".into(),
            kind: kind.into(),
        }
    }

    #[test]
    fn a_fetched_record_with_no_title_or_author_is_filled_from_the_search_result() {
        // Crossref's real BibTeX for this chapter: the title only as `booktitle`, no author —
        // unkeyable as fetched, though the search result showed both.
        let bibtex = brace_bare_values(
            "@inbook{2017, ISBN={9780300151725}, DOI={10.12987/9780300151725-005}, \
             booktitle={The Brothers Karamazov}, publisher={Yale University Press}, \
             year={2017}, month=Dec, pages={4–6} }",
        );
        let mut entries = crate::entry::parse_bibtex(&bibtex).unwrap();
        assert!(crate::entry::title_string(&entries[0]).is_none());
        let candidate = picked(
            CandidateId::Doi("10.12987/9780300151725-005".into()),
            "Chapter",
        );
        fill_from_candidate(&mut entries[0], &candidate).unwrap();

        let e = &entries[0];
        assert_eq!(
            crate::entry::title_string(e).as_deref(),
            Some("2. The Importance of The Brothers Karamazov")
        );
        assert_eq!(crate::entry::family_name(e).as_deref(), Some("Jackson"));
        // What the record did have is kept.
        assert!(e.page_range().is_some());
    }

    #[test]
    fn filling_never_overrides_what_the_record_has() {
        let mut entries = crate::entry::parse_bibtex(
            "@book{x, title={The Real Title}, editor={Smith, Ann}, year={1999}}",
        )
        .unwrap();
        fill_from_candidate(
            &mut entries[0],
            &picked(CandidateId::Doi("10.1/x".into()), "Book"),
        )
        .unwrap();
        let e = &entries[0];
        assert_eq!(
            crate::entry::title_string(e).as_deref(),
            Some("The Real Title")
        );
        // An editor-only book stays editor-only, not credited to the search's author.
        assert!(!e.authors().is_some_and(|a| !a.is_empty()));
        assert_eq!(crate::entry::year(e), Some(1999));
    }

    #[test]
    fn a_search_result_alone_makes_a_complete_entry() {
        let e = candidate_entry(&picked(
            CandidateId::Isbn("9780300151725".into()),
            "Chapter",
        ))
        .unwrap();
        assert_eq!(
            crate::entry::title_string(&e).as_deref(),
            Some("2. The Importance of The Brothers Karamazov")
        );
        assert_eq!(crate::entry::family_name(&e).as_deref(), Some("Jackson"));
        assert_eq!(crate::entry::year(&e), Some(2017));
        assert_eq!(
            e.parents()[0]
                .title()
                .map(|t| t.value.to_string())
                .as_deref(),
            Some("The Brothers Karamazov")
        );
        // Natural-order OpenLibrary names split on the last word.
        let mut book = picked(CandidateId::Isbn("9780451523884".into()), "Book");
        book.authors = vec!["Fyodor Dostoyevsky".into()];
        book.container.clear();
        let e = candidate_entry(&book).unwrap();
        assert_eq!(
            crate::entry::family_name(&e).as_deref(),
            Some("Dostoyevsky")
        );
    }
}
