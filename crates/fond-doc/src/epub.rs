//! EPUB metadata *and structure* reader. An EPUB is a ZIP whose `META-INF/container.xml`
//! points at an OPF package document; the OPF's `<metadata>` block carries Dublin Core
//! fields (title, creator, date, publisher, identifier) — enough to seed a bibliographic
//! entry, which is all this module did before M5. Reading (M5-SPEC.md 5B) additionally
//! needs the OPF's `<manifest>` (every resource, by id) and `<spine>` (reading order, by
//! idref) to walk chapters in order, plus a table of contents (EPUB3 nav document, or the
//! EPUB2 NCX fallback). No PDFium involved; this is pure ZIP + XML — the actual chapter
//! *rendering* is WebKitGTK's job, GTK-specific and so kept out of this crate
//! (`docs/ARCHITECTURE.md` §4). The `.epub` file itself is attached to the entry by the
//! caller (`fond-bib`).

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use quick_xml::events::{BytesText, Event};
use quick_xml::Reader;

use crate::error::{DocError, Result};

/// Decodes and XML-unescapes a text event, matching quick-xml <=0.39's removed
/// `BytesText::unescape()` (decode, then unescape with the predefined entities)
/// so every call site below keeps its old behavior unchanged.
fn unescape_text<'a>(
    t: &BytesText<'a>,
) -> std::result::Result<std::borrow::Cow<'a, str>, quick_xml::Error> {
    let decoded = t.decode()?;
    match quick_xml::escape::unescape(&decoded)? {
        std::borrow::Cow::Borrowed(_) => Ok(decoded),
        std::borrow::Cow::Owned(s) => Ok(s.into()),
    }
}

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

/// One chapter/section entry in a reader jump list (table of contents). Nesting from the
/// source nav/NCX is not preserved — a flat, document-ordered list is enough to jump to a
/// section, and much simpler than reproducing a tree the reader UI would have to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    pub label: String,
    /// Zip-internal path (resolved against the OPF's directory), with any `#fragment`
    /// preserved so a reader can jump straight to an in-chapter anchor.
    pub target: String,
}

/// An EPUB's reading structure: chapters in spine order, and (if present) a table of
/// contents. Resource bytes themselves aren't read here — call `extract_all` and serve
/// `spine`/`toc` paths as `file://` URIs under the extraction directory, so a chapter's own
/// relative links to sibling images/CSS resolve the normal browser way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpubBook {
    /// Chapter files in reading order, as zip-internal paths.
    pub spine: Vec<String>,
    /// Chapter/section jump list. Empty if the EPUB has neither an EPUB3 nav document nor
    /// an EPUB2 NCX — every chapter is still reachable via `spine`, just not by name.
    pub toc: Vec<TocEntry>,
}

/// Read an EPUB's spine (reading order) and table of contents. Errors if the file isn't a
/// valid EPUB/ZIP, has no container/OPF, or its spine resolves to zero chapters — a table of
/// contents is optional, a reading order is not.
pub fn open_book(path: &Path) -> Result<EpubBook> {
    let file = std::fs::File::open(path)
        .map_err(|e| DocError::Epub(format!("cannot open {}: {e}", path.display())))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| DocError::Epub(format!("not a valid EPUB/ZIP: {e}")))?;

    let container = read_zip_entry(&mut zip, "META-INF/container.xml")?
        .ok_or_else(|| DocError::Epub("EPUB has no META-INF/container.xml".to_string()))?;
    let opf_path = opf_path_from_container(&container)?;
    let opf_dir = dir_of(&opf_path).to_string();

    let opf = read_zip_entry(&mut zip, &opf_path)?
        .ok_or_else(|| DocError::Epub(format!("OPF not found at {opf_path}")))?;
    let structure = parse_opf_structure(&opf, &opf_dir)?;

    let spine: Vec<String> = structure
        .spine_idrefs
        .iter()
        .filter_map(|idref| structure.manifest.get(idref))
        .map(|item| item.path.clone())
        .collect();
    if spine.is_empty() {
        return Err(DocError::Epub(
            "EPUB spine has no readable chapters".to_string(),
        ));
    }

    // Table of contents: prefer the EPUB3 nav document (the manifest item whose
    // `properties` includes `nav`); fall back to the EPUB2 NCX the spine's own `toc`
    // attribute names. Each is parsed relative to *its own* directory, not the OPF's — a
    // nav/NCX file commonly lives alongside the chapters, in a different directory than the
    // OPF package document.
    let nav_item = structure.manifest.values().find(|i| i.is_nav);
    let toc = if let Some(nav) = nav_item {
        read_zip_entry(&mut zip, &nav.path)?
            .map(|xml| parse_nav_toc(&xml, dir_of(&nav.path)))
            .unwrap_or_default()
    } else if let Some(ncx) = structure
        .spine_toc_id
        .as_ref()
        .and_then(|id| structure.manifest.get(id))
    {
        read_zip_entry(&mut zip, &ncx.path)?
            .map(|xml| parse_ncx_toc(&xml, dir_of(&ncx.path)))
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    Ok(EpubBook { spine, toc })
}

/// Extract every file in the EPUB to `dest_dir`, preserving the zip's internal directory
/// structure, so a chapter's relative links to sibling images/CSS resolve correctly when
/// served over `file://`. Safe to call repeatedly (e.g. on every "Read" click) — `zip`'s
/// `extract` overwrites in place.
pub fn extract_all(path: &Path, dest_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(path)
        .map_err(|e| DocError::Epub(format!("cannot open {}: {e}", path.display())))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| DocError::Epub(format!("not a valid EPUB/ZIP: {e}")))?;
    zip.extract(dest_dir)
        .map_err(|e| DocError::Epub(format!("could not extract EPUB: {e}")))?;
    Ok(())
}

/// Extract a full-text approximation of an EPUB's chapter content, for the search index —
/// every spine chapter's rendered text, in reading order, tags stripped, joined the same way
/// `fond_doc::pdf::PdfText::full_text` joins PDF pages (blank line between chapters). A
/// chapter that fails to read is skipped rather than failing the whole extraction — matches
/// `open_book`'s "best-effort" stance on the parts of an EPUB that aren't the spine itself.
pub fn extract_text(path: &Path) -> Result<String> {
    let book = open_book(path)?;
    let file = std::fs::File::open(path)
        .map_err(|e| DocError::Epub(format!("cannot open {}: {e}", path.display())))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| DocError::Epub(format!("not a valid EPUB/ZIP: {e}")))?;

    let mut chapters = Vec::with_capacity(book.spine.len());
    for chapter_path in &book.spine {
        if let Ok(Some(xml)) = read_zip_entry(&mut zip, chapter_path) {
            let text = strip_tags(&xml);
            if !text.is_empty() {
                chapters.push(text);
            }
        }
    }
    Ok(chapters.join("\n\n"))
}

/// Strip XML/HTML tags from a chapter document, returning its concatenated visible text.
/// Skips `<script>`/`<style>` contents (not reader-visible text). Malformed markup degrades
/// to a best-effort partial result (whatever text was collected before the parse error)
/// rather than an error — a slightly incomplete search index entry beats no chapter at all.
fn strip_tags(xml: &str) -> String {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut out = String::new();
    // Depth counter for the `<script>`/`<style>` subtree currently being skipped, so nested
    // elements inside one don't prematurely end the skip on their own closing tag.
    let mut skip_depth: u32 = 0;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let qname = e.name();
                let name = local_name(qname.as_ref());
                if skip_depth > 0 || name == b"script" || name == b"style" {
                    skip_depth += 1;
                }
            }
            Ok(Event::End(_)) => {
                skip_depth = skip_depth.saturating_sub(1);
            }
            Ok(Event::Text(t)) => {
                if skip_depth == 0 {
                    if let Ok(s) = unescape_text(&t) {
                        let s = s.trim();
                        if !s.is_empty() {
                            out.push_str(s);
                            out.push(' ');
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    out.trim().to_string()
}

/// One `<manifest><item>` entry, resolved.
struct ManifestItem {
    /// Zip-internal path, already resolved relative to the OPF's directory.
    path: String,
    /// Whether this item's `properties` attribute lists `nav` — the EPUB3 marker for "this
    /// is the navigation document," picked out of the manifest rather than assumed to be at
    /// a fixed filename.
    is_nav: bool,
}

/// The parts of an OPF package document a reader needs beyond `<metadata>`: every resource
/// by id, and the spine's reading order (by idref) plus its optional NCX pointer.
struct OpfStructure {
    manifest: HashMap<String, ManifestItem>,
    /// idrefs in document order, skipping `linear="no"` items (out-of-band content like a
    /// pop-up footnote page, not part of the main reading order).
    spine_idrefs: Vec<String>,
    /// The `<spine toc="…">` attribute — an idref into `manifest`, for the EPUB2 NCX
    /// fallback when no EPUB3 nav item is present.
    spine_toc_id: Option<String>,
}

fn parse_opf_structure(xml: &str, opf_dir: &str) -> Result<OpfStructure> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut manifest = HashMap::new();
    let mut spine_idrefs = Vec::new();
    let mut spine_toc_id = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match local_name(e.name().as_ref()) {
                b"item" => {
                    let id = attr_value(&e, b"id");
                    let href = attr_value(&e, b"href");
                    let is_nav = attr_value(&e, b"properties")
                        .is_some_and(|p| p.split_ascii_whitespace().any(|tok| tok == "nav"));
                    if let (Some(id), Some(href)) = (id, href) {
                        let path = resolve_zip_path(opf_dir, &href);
                        manifest.insert(id, ManifestItem { path, is_nav });
                    }
                }
                b"itemref" => {
                    if attr_value(&e, b"linear").as_deref() != Some("no") {
                        if let Some(idref) = attr_value(&e, b"idref") {
                            spine_idrefs.push(idref);
                        }
                    }
                }
                b"spine" => {
                    spine_toc_id = attr_value(&e, b"toc");
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(DocError::Epub(format!("OPF parse error: {e}"))),
            _ => {}
        }
    }
    Ok(OpfStructure {
        manifest,
        spine_idrefs,
        spine_toc_id,
    })
}

/// Flatten an EPUB3 nav document's `<nav epub:type="toc">` list into document-ordered
/// `(label, href)` pairs. Only anchors inside the `toc` nav are captured — a nav document
/// may also carry `landmarks`/`page-list` navs, which aren't a chapter list.
fn parse_nav_toc(xml: &str, base_dir: &str) -> Vec<TocEntry> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut entries = Vec::new();

    let mut depth: i32 = 0;
    let mut toc_nav_depth: Option<i32> = None;
    let mut in_anchor = false;
    let mut current_href: Option<String> = None;
    let mut current_text = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = local_name(e.name().as_ref()).to_vec();
                if name == b"nav" && toc_nav_depth.is_none() {
                    let is_toc = attr_value(&e, b"type").as_deref() == Some("toc");
                    if is_toc {
                        toc_nav_depth = Some(depth);
                    }
                } else if name == b"a" && toc_nav_depth.is_some() {
                    current_href = attr_value(&e, b"href");
                    current_text.clear();
                    in_anchor = true;
                }
                depth += 1;
            }
            Ok(Event::Text(t)) => {
                if in_anchor {
                    if let Ok(s) = unescape_text(&t) {
                        current_text.push_str(&s);
                    }
                }
            }
            Ok(Event::End(e)) => {
                depth -= 1;
                let name = local_name(e.name().as_ref()).to_vec();
                if name == b"a" && in_anchor {
                    in_anchor = false;
                    if let Some(href) = current_href.take() {
                        let label = current_text.trim().to_string();
                        if !label.is_empty() {
                            entries.push(TocEntry {
                                label,
                                target: resolve_href_with_fragment(base_dir, &href),
                            });
                        }
                    }
                } else if name == b"nav" && toc_nav_depth == Some(depth) {
                    toc_nav_depth = None;
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    entries
}

/// Flatten an EPUB2 NCX `<navMap>` into document-ordered `(label, href)` pairs — each
/// `<navPoint>`'s `<navLabel><text>` paired with its own `<content src>`, nesting discarded.
fn parse_ncx_toc(xml: &str, base_dir: &str) -> Vec<TocEntry> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut entries = Vec::new();

    let mut pending_label: Option<String> = None;
    let mut in_text = false;
    let mut text_buf = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = local_name(e.name().as_ref()).to_vec();
                if name == b"text" {
                    in_text = true;
                    text_buf.clear();
                } else if name == b"content" {
                    if let Some(src) = attr_value(&e, b"src") {
                        if let Some(label) = pending_label.take() {
                            entries.push(TocEntry {
                                label,
                                target: resolve_href_with_fragment(base_dir, &src),
                            });
                        }
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if in_text {
                    if let Ok(s) = unescape_text(&t) {
                        text_buf.push_str(&s);
                    }
                }
            }
            Ok(Event::End(e)) => {
                if local_name(e.name().as_ref()) == b"text" && in_text {
                    in_text = false;
                    let label = text_buf.trim().to_string();
                    if !label.is_empty() {
                        pending_label = Some(label);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    entries
}

/// The directory portion of a zip-internal path (no trailing slash; empty for a root file).
fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(idx) => &path[..idx],
        None => "",
    }
}

/// Resolve an href (from `<item href>`, `<itemref>`, a nav `<a href>`, or an NCX
/// `<content src>`) that's relative to `base_dir`, into a zip-internal path — normalizing
/// `.`/`..` segments the way a browser would, and percent-decoding escaped characters (a
/// space as `%20` is common in EPUB filenames). Does not touch any `#fragment`; callers that
/// need one preserved use `resolve_href_with_fragment`.
fn resolve_zip_path(base_dir: &str, href: &str) -> String {
    let decoded = percent_decode(href);
    let mut segments: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').filter(|s| !s.is_empty()).collect()
    };
    for part in decoded.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            seg => segments.push(seg),
        }
    }
    segments.join("/")
}

/// As `resolve_zip_path`, but keeps a trailing `#fragment` (an in-chapter anchor) attached
/// to the resolved path rather than discarding it.
fn resolve_href_with_fragment(base_dir: &str, href: &str) -> String {
    let (path_part, fragment) = match href.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (href, None),
    };
    let resolved = resolve_zip_path(base_dir, path_part);
    match fragment {
        Some(f) if !f.is_empty() => format!("{resolved}#{f}"),
        _ => resolved,
    }
}

/// Decode `%XX` percent-escapes (the only escaping EPUB hrefs commonly use — a literal space
/// as `%20`). Malformed sequences are passed through unchanged rather than erroring, since a
/// slightly-too-permissive filename resolution is preferable to hard-failing a whole EPUB
/// over a cosmetic href quirk.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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
            Ok(Event::Empty(e)) | Ok(Event::Start(e))
                if local_name(e.name().as_ref()) == b"rootfile" =>
            {
                if let Some(p) = attr_value(&e, b"full-path") {
                    return Ok(p);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(DocError::Epub(format!("container.xml parse error: {e}"))),
            _ => {}
        }
    }
    Err(DocError::Epub(
        "no <rootfile full-path=…> in container.xml".to_string(),
    ))
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
                creator_file_as =
                    attr_value(&e, b"file-as").or_else(|| attr_value(&e, b"opf:file-as"));
                identifier_scheme =
                    attr_value(&e, b"scheme").or_else(|| attr_value(&e, b"opf:scheme"));
                current = Some(name);
            }
            Ok(Event::Text(t)) => {
                if let Some(name) = &current {
                    let text = unescape_text(&t)
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
        let raw = urn
            .map(|s| s.to_string())
            .unwrap_or_else(|| text.to_string());
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
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
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
    use std::io::Write;

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

    // -- resolve_zip_path / resolve_href_with_fragment --------------------------------

    #[test]
    fn resolves_relative_paths_against_base_dir() {
        assert_eq!(
            resolve_zip_path("OEBPS", "chap1.xhtml"),
            "OEBPS/chap1.xhtml"
        );
        assert_eq!(
            resolve_zip_path("OEBPS/toc", "../chap1.xhtml"),
            "OEBPS/chap1.xhtml"
        );
        assert_eq!(resolve_zip_path("", "chap1.xhtml"), "chap1.xhtml");
        assert_eq!(
            resolve_zip_path("OEBPS", "images/cover.png"),
            "OEBPS/images/cover.png"
        );
    }

    #[test]
    fn resolves_percent_encoded_spaces() {
        assert_eq!(
            resolve_zip_path("OEBPS", "chapter%20one.xhtml"),
            "OEBPS/chapter one.xhtml"
        );
    }

    #[test]
    fn keeps_fragment_separate_from_path_resolution() {
        assert_eq!(
            resolve_href_with_fragment("OEBPS", "chap1.xhtml#sec2"),
            "OEBPS/chap1.xhtml#sec2"
        );
        assert_eq!(
            resolve_href_with_fragment("OEBPS", "chap1.xhtml"),
            "OEBPS/chap1.xhtml"
        );
    }

    // -- parse_nav_toc / parse_ncx_toc -------------------------------------------------

    #[test]
    fn nav_toc_only_captures_the_toc_nav_not_landmarks() {
        let xml = r#"<?xml version="1.0"?>
        <html xmlns:epub="http://www.idpf.org/2007/ops">
          <body>
            <nav epub:type="landmarks">
              <ol><li><a href="chap1.xhtml">Start of content</a></li></ol>
            </nav>
            <nav epub:type="toc">
              <ol>
                <li><a href="chap1.xhtml">Chapter One</a></li>
                <li><a href="chap2.xhtml#sec2">Chapter Two, Section 2</a></li>
              </ol>
            </nav>
          </body>
        </html>"#;
        let toc = parse_nav_toc(xml, "OEBPS");
        assert_eq!(
            toc,
            vec![
                TocEntry {
                    label: "Chapter One".into(),
                    target: "OEBPS/chap1.xhtml".into(),
                },
                TocEntry {
                    label: "Chapter Two, Section 2".into(),
                    target: "OEBPS/chap2.xhtml#sec2".into(),
                },
            ]
        );
    }

    #[test]
    fn ncx_toc_pairs_labels_with_content_src() {
        let xml = r#"<?xml version="1.0"?>
        <ncx xmlns="http://www.daisy.org/z3986/2005/ncx/">
          <navMap>
            <navPoint id="np1" playOrder="1">
              <navLabel><text>Chapter One</text></navLabel>
              <content src="chap1.xhtml"/>
              <navPoint id="np1-1" playOrder="2">
                <navLabel><text>Section 1.1</text></navLabel>
                <content src="chap1.xhtml#sec1-1"/>
              </navPoint>
            </navPoint>
            <navPoint id="np2" playOrder="3">
              <navLabel><text>Chapter Two</text></navLabel>
              <content src="chap2.xhtml"/>
            </navPoint>
          </navMap>
        </ncx>"#;
        let toc = parse_ncx_toc(xml, "OEBPS");
        assert_eq!(
            toc,
            vec![
                TocEntry {
                    label: "Chapter One".into(),
                    target: "OEBPS/chap1.xhtml".into(),
                },
                TocEntry {
                    label: "Section 1.1".into(),
                    target: "OEBPS/chap1.xhtml#sec1-1".into(),
                },
                TocEntry {
                    label: "Chapter Two".into(),
                    target: "OEBPS/chap2.xhtml".into(),
                },
            ]
        );
    }

    // -- parse_opf_structure / open_book end-to-end ------------------------------------

    fn write_zip(files: &[(&str, &str)]) -> Vec<u8> {
        let buf = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, contents) in files {
            zip.start_file(*name, options).unwrap();
            zip.write_all(contents.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    const CONTAINER_XML: &str = r#"<?xml version="1.0"?>
    <container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
      <rootfiles>
        <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
      </rootfiles>
    </container>"#;

    #[test]
    fn open_book_orders_spine_skips_non_linear_and_reads_epub3_nav_toc() {
        let opf = r#"<?xml version="1.0"?>
        <package xmlns="http://www.idpf.org/2007/opf">
          <metadata><dc:title xmlns:dc="http://purl.org/dc/elements/1.1/">A Book</dc:title></metadata>
          <manifest>
            <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
            <item id="c1" href="chap1.xhtml" media-type="application/xhtml+xml"/>
            <item id="c2" href="chap2.xhtml" media-type="application/xhtml+xml"/>
            <item id="fn1" href="footnotes.xhtml" media-type="application/xhtml+xml"/>
          </manifest>
          <spine>
            <itemref idref="c1"/>
            <itemref idref="c2"/>
            <itemref idref="fn1" linear="no"/>
          </spine>
        </package>"#;
        let nav = r#"<?xml version="1.0"?>
        <html xmlns:epub="http://www.idpf.org/2007/ops">
          <body>
            <nav epub:type="toc">
              <ol>
                <li><a href="chap1.xhtml">Chapter One</a></li>
                <li><a href="chap2.xhtml">Chapter Two</a></li>
              </ol>
            </nav>
          </body>
        </html>"#;

        let bytes = write_zip(&[
            ("META-INF/container.xml", CONTAINER_XML),
            ("OEBPS/content.opf", opf),
            ("OEBPS/nav.xhtml", nav),
            ("OEBPS/chap1.xhtml", "<html><body>One</body></html>"),
            ("OEBPS/chap2.xhtml", "<html><body>Two</body></html>"),
            ("OEBPS/footnotes.xhtml", "<html><body>Notes</body></html>"),
        ]);

        let dir = std::env::temp_dir().join(format!(
            "fond-doc-epub-test-{}-{}",
            std::process::id(),
            "spine_toc"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let epub_path = dir.join("book.epub");
        std::fs::write(&epub_path, &bytes).unwrap();

        let book = open_book(&epub_path).unwrap();
        assert_eq!(
            book.spine,
            vec![
                "OEBPS/chap1.xhtml".to_string(),
                "OEBPS/chap2.xhtml".to_string()
            ],
            "the linear=\"no\" footnotes item must be excluded from the reading order"
        );
        assert_eq!(
            book.toc,
            vec![
                TocEntry {
                    label: "Chapter One".into(),
                    target: "OEBPS/chap1.xhtml".into(),
                },
                TocEntry {
                    label: "Chapter Two".into(),
                    target: "OEBPS/chap2.xhtml".into(),
                },
            ]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_book_falls_back_to_ncx_when_no_nav_item_present() {
        let opf = r#"<?xml version="1.0"?>
        <package xmlns="http://www.idpf.org/2007/opf">
          <metadata><dc:title xmlns:dc="http://purl.org/dc/elements/1.1/">A Book</dc:title></metadata>
          <manifest>
            <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
            <item id="c1" href="chap1.xhtml" media-type="application/xhtml+xml"/>
          </manifest>
          <spine toc="ncx">
            <itemref idref="c1"/>
          </spine>
        </package>"#;
        let ncx = r#"<?xml version="1.0"?>
        <ncx xmlns="http://www.daisy.org/z3986/2005/ncx/">
          <navMap>
            <navPoint id="np1"><navLabel><text>Chapter One</text></navLabel><content src="chap1.xhtml"/></navPoint>
          </navMap>
        </ncx>"#;

        let bytes = write_zip(&[
            ("META-INF/container.xml", CONTAINER_XML),
            ("OEBPS/content.opf", opf),
            ("OEBPS/toc.ncx", ncx),
            ("OEBPS/chap1.xhtml", "<html><body>One</body></html>"),
        ]);

        let dir = std::env::temp_dir().join(format!(
            "fond-doc-epub-test-{}-{}",
            std::process::id(),
            "ncx_fallback"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let epub_path = dir.join("book.epub");
        std::fs::write(&epub_path, &bytes).unwrap();

        let book = open_book(&epub_path).unwrap();
        assert_eq!(book.spine, vec!["OEBPS/chap1.xhtml".to_string()]);
        assert_eq!(
            book.toc,
            vec![TocEntry {
                label: "Chapter One".into(),
                target: "OEBPS/chap1.xhtml".into(),
            }]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_book_errors_on_empty_spine() {
        let opf = r#"<?xml version="1.0"?>
        <package xmlns="http://www.idpf.org/2007/opf">
          <metadata><dc:title xmlns:dc="http://purl.org/dc/elements/1.1/">Empty</dc:title></metadata>
          <manifest></manifest>
          <spine></spine>
        </package>"#;
        let bytes = write_zip(&[
            ("META-INF/container.xml", CONTAINER_XML),
            ("OEBPS/content.opf", opf),
        ]);

        let dir = std::env::temp_dir().join(format!(
            "fond-doc-epub-test-{}-{}",
            std::process::id(),
            "empty_spine"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let epub_path = dir.join("book.epub");
        std::fs::write(&epub_path, &bytes).unwrap();

        assert!(open_book(&epub_path).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    // -- strip_tags / extract_text -----------------------------------------------------

    #[test]
    fn strip_tags_keeps_visible_text_and_drops_script_and_style() {
        // Well-formed XML (as a real EPUB's XHTML must be): a literal `<` inside <script>
        // text would itself need escaping or CDATA, so this uses a `<`-free script body —
        // the point under test is that skip_depth correctly excludes script/style text, not
        // XML escaping.
        let xml = r#"<html><head><style>body{color:red}</style></head>
        <body>
          <p>Hello <em>world</em>.</p>
          <script>var visible = false;</script>
          <p>Second paragraph.</p>
        </body></html>"#;
        let text = strip_tags(xml);
        assert!(text.contains("Hello"));
        assert!(text.contains("world"));
        assert!(text.contains("Second paragraph."));
        assert!(!text.contains("color:red"));
        assert!(!text.contains("visible"));
    }

    #[test]
    fn extract_text_joins_spine_chapters_in_order() {
        let opf = r#"<?xml version="1.0"?>
        <package xmlns="http://www.idpf.org/2007/opf">
          <metadata><dc:title xmlns:dc="http://purl.org/dc/elements/1.1/">A Book</dc:title></metadata>
          <manifest>
            <item id="c1" href="chap1.xhtml" media-type="application/xhtml+xml"/>
            <item id="c2" href="chap2.xhtml" media-type="application/xhtml+xml"/>
          </manifest>
          <spine>
            <itemref idref="c1"/>
            <itemref idref="c2"/>
          </spine>
        </package>"#;
        let bytes = write_zip(&[
            ("META-INF/container.xml", CONTAINER_XML),
            ("OEBPS/content.opf", opf),
            (
                "OEBPS/chap1.xhtml",
                "<html><body><p>First chapter text.</p></body></html>",
            ),
            (
                "OEBPS/chap2.xhtml",
                "<html><body><p>Second chapter text.</p></body></html>",
            ),
        ]);

        let dir = std::env::temp_dir().join(format!(
            "fond-doc-epub-test-{}-{}",
            std::process::id(),
            "extract_text"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let epub_path = dir.join("book.epub");
        std::fs::write(&epub_path, &bytes).unwrap();

        let text = extract_text(&epub_path).unwrap();
        let first_idx = text.find("First chapter text.").unwrap();
        let second_idx = text.find("Second chapter text.").unwrap();
        assert!(
            first_idx < second_idx,
            "chapters must appear in spine order"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
