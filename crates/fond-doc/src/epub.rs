//! Minimal EPUB metadata reader. An EPUB is a ZIP whose `META-INF/container.xml` points at
//! an OPF package document; the OPF's `<metadata>` block carries Dublin Core fields (title,
//! creator, date, publisher, identifier). We read just those — enough to seed a bibliographic
//! entry — without pulling in a full EPUB rendering stack. No PDFium involved; this is pure
//! ZIP + XML. The `.epub` file itself is attached to the entry by the caller (`fond-bib`).

use std::io::Read;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::error::{DocError, Result};

/// The bibliographic fields we lift from an EPUB's OPF metadata. Everything is best-effort:
/// a field absent from the OPF is `None`/empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EpubMetadata {
    pub title: Option<String>,
    /// Authors, each as close to `Family, Given` as the OPF allows (prefers `opf:file-as`).
    pub authors: Vec<String>,
    /// Publication date as written in the OPF (e.g. `1970` or `1970-01-01`).
    pub date: Option<String>,
    pub publisher: Option<String>,
    /// ISBN if the OPF carries one (via `opf:scheme="ISBN"` or a `urn:isbn:` identifier).
    pub isbn: Option<String>,
}

/// Read metadata from the EPUB at `path`. Errors only on a structurally broken file (not a
/// ZIP, no container, no OPF); a valid EPUB that simply lacks a field yields that field empty.
pub fn extract_metadata(path: &Path) -> Result<EpubMetadata> {
    let file = std::fs::File::open(path)
        .map_err(|e| DocError::Epub(format!("cannot open {}: {e}", path.display())))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| DocError::Epub(format!("not a valid EPUB/ZIP: {e}")))?;

    let container = read_zip_entry(&mut zip, "META-INF/container.xml")?
        .ok_or_else(|| DocError::Epub("EPUB has no META-INF/container.xml".to_string()))?;
    let opf_path = opf_path_from_container(&container)?;

    let opf = read_zip_entry(&mut zip, &opf_path)?
        .ok_or_else(|| DocError::Epub(format!("OPF not found at {opf_path}")))?;
    parse_opf_metadata(&opf)
}

/// Read a single ZIP entry as UTF-8 text, or `None` if it isn't present.
fn read_zip_entry<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<Option<String>> {
    let mut entry = match zip.by_name(name) {
        Ok(e) => e,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(DocError::Epub(format!("reading {name}: {e}"))),
    };
    let mut buf = String::new();
    entry
        .read_to_string(&mut buf)
        .map_err(|e| DocError::Epub(format!("reading {name}: {e}")))?;
    Ok(Some(buf))
}

/// The `full-path` of the first `<rootfile>` in `META-INF/container.xml`.
fn opf_path_from_container(xml: &str) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    loop {
        match reader.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if local_name(e.name().as_ref()) == b"rootfile" => {
                if let Some(p) = attr_value(&e, b"full-path") {
                    return Ok(p);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(DocError::Epub(format!("container.xml parse error: {e}"))),
            _ => {}
        }
    }
    Err(DocError::Epub("no <rootfile full-path=…> in container.xml".to_string()))
}

/// Parse the Dublin Core fields out of an OPF package document's `<metadata>` block.
fn parse_opf_metadata(xml: &str) -> Result<EpubMetadata> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut meta = EpubMetadata::default();
    // The element we're currently collecting text for, and any attributes we need after the
    // text arrives (creator's file-as, identifier's scheme).
    let mut current: Option<Vec<u8>> = None;
    let mut creator_file_as: Option<String> = None;
    let mut identifier_scheme: Option<String> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = local_name(e.name().as_ref()).to_vec();
                creator_file_as = attr_value(&e, b"file-as").or_else(|| attr_value(&e, b"opf:file-as"));
                identifier_scheme = attr_value(&e, b"scheme").or_else(|| attr_value(&e, b"opf:scheme"));
                current = Some(name);
            }
            Ok(Event::Text(t)) => {
                if let Some(name) = &current {
                    let text = t
                        .unescape()
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                    if !text.is_empty() {
                        absorb_field(&mut meta, name, &text, &creator_file_as, &identifier_scheme);
                    }
                }
            }
            Ok(Event::End(_)) => {
                current = None;
                creator_file_as = None;
                identifier_scheme = None;
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(DocError::Epub(format!("OPF parse error: {e}"))),
            _ => {}
        }
    }
    Ok(meta)
}

/// Fold one text-bearing Dublin Core element into the metadata being built.
fn absorb_field(
    meta: &mut EpubMetadata,
    element: &[u8],
    text: &str,
    creator_file_as: &Option<String>,
    identifier_scheme: &Option<String>,
) {
    match element {
        b"title" => {
            if meta.title.is_none() {
                meta.title = Some(text.to_string());
            }
        }
        b"creator" => {
            // Prefer the library-order `file-as` ("Family, Given"); fall back to display text.
            meta.authors
                .push(creator_file_as.clone().unwrap_or_else(|| text.to_string()));
        }
        b"date" => {
            if meta.date.is_none() {
                meta.date = Some(text.to_string());
            }
        }
        b"publisher" => {
            if meta.publisher.is_none() {
                meta.publisher = Some(text.to_string());
            }
        }
        b"identifier" if meta.isbn.is_none() => {
            meta.isbn = isbn_from_identifier(text, identifier_scheme);
        }
        _ => {}
    }
}

/// Recognise an ISBN from a `<dc:identifier>` value + optional `scheme` attribute. Handles
/// `scheme="ISBN"` and bare `urn:isbn:…` content; returns the digits/X normalised of hyphens.
fn isbn_from_identifier(text: &str, scheme: &Option<String>) -> Option<String> {
    let scheme_is_isbn = scheme
        .as_deref()
        .map(|s| s.eq_ignore_ascii_case("isbn"))
        .unwrap_or(false);
    let lower = text.to_ascii_lowercase();
    let urn = lower.strip_prefix("urn:isbn:");
    if scheme_is_isbn || urn.is_some() {
        let raw = urn.map(|s| s.to_string()).unwrap_or_else(|| text.to_string());
        let cleaned: String = raw.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }
    None
}

/// Attribute lookup by (possibly namespaced) local name. Matches `full-path`, `file-as`,
/// `scheme`, whether or not they carry an `opf:` prefix.
fn attr_value(e: &quick_xml::events::BytesStart, want: &[u8]) -> Option<String> {
    let want_local = local_name(want);
    for attr in e.attributes().flatten() {
        if local_name(attr.key.as_ref()) == want_local {
            return attr
                .unescape_value()
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|s| !s.is_empty());
        }
    }
    None
}

/// The part of an XML name after any `prefix:` (so `dc:title` → `title`, `opf:scheme` →
/// `scheme`).
fn local_name(name: &[u8]) -> &[u8] {
    match name.iter().position(|&b| b == b':') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dublin_core_with_file_as_and_isbn_scheme() {
        let opf = r#"<?xml version="1.0"?>
        <package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/">
          <metadata xmlns:opf="http://www.idpf.org/2007/opf">
            <dc:title>Black Theology and Black Power</dc:title>
            <dc:creator opf:file-as="Cone, James H." opf:role="aut">James H. Cone</dc:creator>
            <dc:date>1969</dc:date>
            <dc:publisher>Seabury Press</dc:publisher>
            <dc:identifier opf:scheme="ISBN">978-0-88344-158-1</dc:identifier>
          </metadata>
        </package>"#;
        let m = parse_opf_metadata(opf).unwrap();
        assert_eq!(m.title.as_deref(), Some("Black Theology and Black Power"));
        assert_eq!(m.authors, vec!["Cone, James H."]);
        assert_eq!(m.date.as_deref(), Some("1969"));
        assert_eq!(m.publisher.as_deref(), Some("Seabury Press"));
        assert_eq!(m.isbn.as_deref(), Some("9780883441581"));
    }

    #[test]
    fn parses_urn_isbn_and_display_author_fallback() {
        let opf = r#"<?xml version="1.0"?>
        <package xmlns:dc="http://purl.org/dc/elements/1.1/">
          <metadata>
            <dc:title>A Book</dc:title>
            <dc:creator>Jane Doe</dc:creator>
            <dc:identifier>urn:isbn:0-306-40615-2</dc:identifier>
          </metadata>
        </package>"#;
        let m = parse_opf_metadata(opf).unwrap();
        assert_eq!(m.authors, vec!["Jane Doe"]);
        assert_eq!(m.isbn.as_deref(), Some("0306406152"));
        assert!(m.publisher.is_none());
    }

    #[test]
    fn finds_opf_path_in_container() {
        let xml = r#"<?xml version="1.0"?>
        <container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
          <rootfiles>
            <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
          </rootfiles>
        </container>"#;
        assert_eq!(opf_path_from_container(xml).unwrap(), "OEBPS/content.opf");
    }
}
