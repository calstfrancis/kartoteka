//! Network acquisition of bibliographic metadata (Milestone 5). Feature-gated behind
//! `acquire` so lean consumers of `fond-bib` do not pull in `reqwest`. Uses a **blocking**
//! HTTP client on the calling thread — no async runtime (`docs/ARCHITECTURE.md` §3).
//!
//! Strategy: resolve a DOI via doi.org content negotiation asking for BibTeX, then reuse
//! the existing BibLaTeX parser (`Library::add_bibtex`) to turn it into an entry. This
//! keeps one metadata-mapping path instead of a second bespoke JSON mapper.

use std::time::Duration;

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
    Ok(body)
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
}
