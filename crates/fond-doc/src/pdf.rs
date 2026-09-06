//! PDF text extraction via PDFium (Milestone 4). Binding to the native PDFium library is
//! resolved at runtime; nothing here needs PDFium present at build time.

use std::io::Read;
use std::path::{Path, PathBuf};

use pdfium_render::prelude::{
    PdfDocumentMetadataTagType, PdfRect, PdfRenderConfig, PdfSearchDirection, PdfSearchOptions,
    Pdfium, Pixels,
};

use crate::error::{DocError, Result};

/// Bind to the PDFium native library.
///
/// Resolution order:
/// 1. `PDFIUM_LIB_PATH` — a directory containing `libpdfium.so` (or the shared-library file
///    itself).
/// 2. The system library search path.
///
/// PDFium is BSD-3-licensed and shipped as a separate native binary (see `docs/LICENSES.md`);
/// it is never vendored into the repository.
pub fn bind_pdfium() -> Result<Pdfium> {
    if let Ok(val) = std::env::var("PDFIUM_LIB_PATH") {
        let given = PathBuf::from(&val);
        let lib = if given.is_dir() {
            Pdfium::pdfium_platform_library_name_at_path(&given)
        } else {
            given
        };
        let bindings = Pdfium::bind_to_library(&lib).map_err(|e| DocError::LibraryLoad {
            path: lib.display().to_string(),
            message: e.to_string(),
        })?;
        return Ok(Pdfium::new(bindings));
    }

    let bindings = Pdfium::bind_to_system_library().map_err(|e| DocError::LibraryLoad {
        path: "<system>".to_string(),
        message: e.to_string(),
    })?;
    Ok(Pdfium::new(bindings))
}

/// Sniff whether the file at `path` looks like a PDF — for a file whose name has no `.pdf`
/// extension to go on. Just the header magic (`%PDF-`), read as a cheap few-byte peek rather
/// than the whole file; good enough since a corrupt-past-that-point file would fail to parse
/// later anyway rather than being silently misidentified as readable.
pub fn looks_like_pdf(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 5];
    file.read_exact(&mut magic).is_ok() && magic == *b"%PDF-"
}

/// Extracted text from a PDF, one `String` per page.
#[derive(Debug, Clone)]
pub struct PdfText {
    pub page_count: u16,
    pub pages: Vec<String>,
}

impl PdfText {
    /// All page text joined with blank lines — the form fed to the search index.
    pub fn full_text(&self) -> String {
        self.pages.join("\n\n")
    }
}

/// Extract the text layer from PDF bytes.
pub fn extract_text(pdfium: &Pdfium, bytes: &[u8]) -> Result<PdfText> {
    let document = pdfium.load_pdf_from_byte_slice(bytes, None)?;
    let page_count = document.pages().len();
    let mut pages = Vec::with_capacity(page_count as usize);
    for page in document.pages().iter() {
        pages.push(page.text()?.all());
    }
    Ok(PdfText { page_count, pages })
}

/// Embedded PDF document metadata plus page count.
#[derive(Debug, Clone, Default)]
pub struct PdfMeta {
    pub title: Option<String>,
    pub author: Option<String>,
    pub page_count: u16,
}

/// Sniff the embedded metadata (title/author) and page count from PDF bytes. Fields the
/// PDF leaves blank come back as `None`; a blank string is treated as absent.
pub fn extract_metadata(pdfium: &Pdfium, bytes: &[u8]) -> Result<PdfMeta> {
    let document = pdfium.load_pdf_from_byte_slice(bytes, None)?;
    let meta = document.metadata();
    let field = |tag| {
        meta.get(tag)
            .map(|t| t.value().trim().to_string())
            .filter(|s| !s.is_empty())
    };
    Ok(PdfMeta {
        title: field(PdfDocumentMetadataTagType::Title),
        author: field(PdfDocumentMetadataTagType::Author),
        page_count: document.pages().len(),
    })
}

/// A single PDF page rasterized to RGBA8 pixels (for an in-app viewer).
#[derive(Debug, Clone)]
pub struct RenderedPage {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA8, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// Render page `index` (0-based) of a PDF to RGBA8 pixels at `target_width` px wide, the
/// height following the page's aspect ratio. Uses PDFium's raw bitmap (no `image` feature).
pub fn render_page(
    pdfium: &Pdfium,
    bytes: &[u8],
    index: u16,
    target_width: u32,
) -> Result<RenderedPage> {
    let document = pdfium.load_pdf_from_byte_slice(bytes, None)?;
    let pages = document.pages();
    let page = pages.get(index)?;
    let config = PdfRenderConfig::new().set_target_width(target_width.max(1) as Pixels);
    let bitmap = page.render_with_config(&config)?;
    Ok(RenderedPage {
        width: bitmap.width() as u32,
        height: bitmap.height() as u32,
        rgba: bitmap.as_rgba_bytes(),
    })
}

/// Page count of a PDF file (cheap sniff for attachment records).
pub fn page_count(pdfium: &Pdfium, bytes: &[u8]) -> Result<u16> {
    Ok(pdfium.load_pdf_from_byte_slice(bytes, None)?.pages().len())
}

/// Page `index`'s (0-based) size in PDF points (`width`, `height`) — the scale needed to map
/// between PDF user-space coordinates (where annotation quadpoints live) and a rendered
/// page's pixel grid.
pub fn page_size(pdfium: &Pdfium, bytes: &[u8], index: u16) -> Result<(f32, f32)> {
    let document = pdfium.load_pdf_from_byte_slice(bytes, None)?;
    let page = document.pages().get(index)?;
    Ok((page.width().value, page.height().value))
}

/// One entry in a PDF's outline (bookmark tree), flattened into document order with `depth`
/// preserved for indentation — a jump list, not a tree widget, the same flattening choice
/// `fond_doc::epub::TocEntry` makes for its table of contents, for the same reason (simpler
/// to build a reader panel from, and the nesting rarely matters for "jump to this section").
#[derive(Debug, Clone)]
pub struct PdfOutlineEntry {
    pub title: String,
    /// 0 for a top-level bookmark, 1 for its direct children, and so on.
    pub depth: u32,
    /// 1-based page number, if the bookmark's destination resolves to one. `None` for a
    /// bookmark whose action isn't a same-document page jump (an external URL, for example).
    pub page: Option<u16>,
}

/// Read a PDF's outline (bookmark) tree, flattened into document order. Empty if the PDF has
/// no outline — common (most PDFs, especially scanned or short ones, have none) and not an
/// error.
pub fn outline(pdfium: &Pdfium, bytes: &[u8]) -> Result<Vec<PdfOutlineEntry>> {
    let document = pdfium.load_pdf_from_byte_slice(bytes, None)?;
    let mut out = Vec::new();
    for bookmark in document.bookmarks().iter() {
        let Some(title) = bookmark.title() else {
            continue;
        };
        // Depth is the bookmark's ancestor count — `PdfBookmarks::iter()` gives document
        // order but not depth directly, so walk the parent chain once per bookmark. Outline
        // trees are small enough (tens to low hundreds of entries) for this to be cheap.
        let mut depth = 0;
        let mut ancestor = bookmark.parent();
        while let Some(a) = ancestor {
            depth += 1;
            ancestor = a.parent();
        }
        let page = bookmark
            .destination()
            .and_then(|dest| dest.page_index().ok())
            .map(|idx| idx.saturating_add(1));
        out.push(PdfOutlineEntry { title, depth, page });
    }
    Ok(out)
}

/// Read every page's *document* page label — the PDF's own printed numbering (`/PageLabels`
/// in the catalog: e.g. lowercase-roman front matter — "i", "ii" — followed by arabic body
/// pages restarting at "1"), as opposed to the raw 1-based position of the page within the
/// file. Most PDFs have no `/PageLabels` at all, in which case every entry is `None` and a
/// caller should fall back to the raw page number — this is what lets a reader show (and let
/// you type in) the same page number the document itself prints on the page, the way Zotero
/// does, instead of always the raw file position.
///
/// One entry per page, in document order; index `i` is 0-based, matching `render_page`'s own
/// indexing (not `PdfOutlineEntry.page`, which is 1-based).
pub fn page_labels(pdfium: &Pdfium, bytes: &[u8]) -> Result<Vec<Option<String>>> {
    let document = pdfium.load_pdf_from_byte_slice(bytes, None)?;
    let mut out = Vec::with_capacity(document.pages().len() as usize);
    for page in document.pages().iter() {
        out.push(page.label().map(|s| s.to_string()));
    }
    Ok(out)
}

/// One match from `search_document`: the 0-based page it's on (matching `render_page`'s own
/// indexing) and one quad per line the match spans, in PDF user-space — the same shape
/// `Annotation.quadpoints`/`select_text_in_rect` use, so a match can be blended onto a
/// rendered page with `blend_highlights` exactly like a saved highlight.
#[derive(Debug, Clone)]
pub struct PdfSearchMatch {
    pub page: u16,
    pub quads: Vec<[f64; 8]>,
}

/// Search every page of a PDF for `query` (case-insensitive substring), returning every match
/// in document order. An empty (or whitespace-only) query returns no matches rather than
/// erroring — the caller's empty-search-box case, not something worth propagating as an
/// error up through the UI.
pub fn search_document(pdfium: &Pdfium, bytes: &[u8], query: &str) -> Result<Vec<PdfSearchMatch>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let document = pdfium.load_pdf_from_byte_slice(bytes, None)?;
    let options = PdfSearchOptions::new();
    let mut out = Vec::new();

    for (page_index, page) in document.pages().iter().enumerate() {
        let text = page.text()?;
        let search = text.search(query, &options)?;
        for segments in search.iter(PdfSearchDirection::SearchForward) {
            let quads: Vec<[f64; 8]> = segments
                .iter()
                .map(|segment| {
                    let b = segment.bounds();
                    [
                        b.left().value as f64,
                        b.top().value as f64,
                        b.right().value as f64,
                        b.top().value as f64,
                        b.left().value as f64,
                        b.bottom().value as f64,
                        b.right().value as f64,
                        b.bottom().value as f64,
                    ]
                })
                .collect();
            if !quads.is_empty() {
                out.push(PdfSearchMatch {
                    page: page_index as u16,
                    quads,
                });
            }
        }
    }
    Ok(out)
}

/// A real text-run selection: the covered text (reading order) and one axis-aligned quad
/// per line it spans, in PDF user-space — the same anchor `fond-bib`'s `Annotation` stores.
/// Unlike a plain drag-rectangle highlight, this hugs the actual glyphs rather than an
/// arbitrary screen region.
#[derive(Debug, Clone)]
pub struct TextSelection {
    pub text: String,
    pub quads: Vec<[f64; 8]>,
}

/// Find the text under a drag rectangle (`left`/`bottom`/`right`/`top`, in PDF points) on
/// page `index`, grouped into one quad per line it spans. `None` if the rectangle covers no
/// text (e.g. a blank margin) — the caller falls back to a plain rectangle highlight in that
/// case.
///
/// Walks every character on the page (`PdfPageText::chars`) rather than
/// `chars_inside_rect` — that method resolves only the two endpoint characters nearest the
/// rectangle's left/right edges and returns everything between them by *document* index,
/// which is correct for a single-line selection but wrong the moment a drag spans more than
/// one line. Filtering every character's own bounds against the rectangle is the approach a
/// real PDF viewer's click-drag selection uses, and is the only one that is correct for a
/// multi-line drag.
/// One line of a gathered selection: its accumulated bounding box, joined text (in document
/// order, which is reading order for normal prose), and the individual characters that make
/// it up — kept around so `select_text_range` can trim the first/last line down to an exact
/// click position after the fact, without re-walking the page.
struct Line {
    left: f32,
    right: f32,
    bottom: f32,
    top: f32,
    text: String,
    chars: Vec<(f32, f32, String)>,
}

/// Every character on `index` whose bounds overlap the rectangle, grouped into lines. Shared
/// by `select_text_in_rect` (rectangle-bounded) and `select_text_range` (reading-order,
/// line-aware) — both start from the same raw character gather.
fn gather_lines(
    pdfium: &Pdfium,
    bytes: &[u8],
    index: u16,
    left: f32,
    bottom: f32,
    right: f32,
    top: f32,
) -> Result<Vec<Line>> {
    let document = pdfium.load_pdf_from_byte_slice(bytes, None)?;
    let page = document.pages().get(index)?;
    let text = page.text()?;

    let (left, right) = (left.min(right), left.max(right));
    let (bottom, top) = (bottom.min(top), bottom.max(top));

    // One entry per selected character: its bounds and its own string (usually one grapheme).
    let mut hits: Vec<(PdfRect, String)> = Vec::new();
    for c in text.chars().iter() {
        let Ok(bounds) = c.tight_bounds().or_else(|_| c.loose_bounds()) else {
            continue;
        };
        let overlaps = bounds.left().value < right
            && bounds.right().value > left
            && bounds.bottom().value < top
            && bounds.top().value > bottom;
        if overlaps {
            hits.push((bounds, c.unicode_string().unwrap_or_default()));
        }
    }

    // Group consecutive hits into lines: a new hit starts a new line whenever its vertical
    // range no longer overlaps the current line's accumulated range.
    let mut lines: Vec<Line> = Vec::new();
    for (bounds, ch) in hits {
        let (l, r, b, t) = (
            bounds.left().value,
            bounds.right().value,
            bounds.bottom().value,
            bounds.top().value,
        );
        let starts_new_line = match lines.last() {
            Some(line) => b >= line.top || t <= line.bottom,
            None => true,
        };
        if starts_new_line {
            lines.push(Line {
                left: l,
                right: r,
                bottom: b,
                top: t,
                text: ch.clone(),
                chars: vec![(l, r, ch)],
            });
        } else {
            let line = lines.last_mut().expect("just checked non-empty");
            line.left = line.left.min(l);
            line.right = line.right.max(r);
            line.bottom = line.bottom.min(b);
            line.top = line.top.max(t);
            line.text.push_str(&ch);
            line.chars.push((l, r, ch));
        }
    }
    Ok(lines)
}

/// Drops every character in `line` outside `[min_x, max_x)` (either bound optional, meaning
/// unconstrained on that side) and recomputes the line's bounding box and text from what's
/// left. Used to trim a line-aware selection's first/last line down to the exact click
/// position, once the rest of the line's characters have already established the line itself.
fn trim_line(line: &mut Line, min_x: Option<f32>, max_x: Option<f32>) {
    line.chars.retain(|(l, r, _)| {
        min_x.map(|m| *r > m).unwrap_or(true) && max_x.map(|m| *l < m).unwrap_or(true)
    });
    if line.chars.is_empty() {
        return;
    }
    line.left = line
        .chars
        .iter()
        .map(|(l, _, _)| *l)
        .fold(f32::INFINITY, f32::min);
    line.right = line
        .chars
        .iter()
        .map(|(_, r, _)| *r)
        .fold(f32::NEG_INFINITY, f32::max);
    line.text = line.chars.iter().map(|(_, _, c)| c.as_str()).collect();
}

fn lines_to_selection(lines: &[Line]) -> TextSelection {
    let quads = lines
        .iter()
        .map(|line| {
            [
                line.left as f64,
                line.top as f64,
                line.right as f64,
                line.top as f64,
                line.left as f64,
                line.bottom as f64,
                line.right as f64,
                line.bottom as f64,
            ]
        })
        .collect();
    let joined_text = lines
        .iter()
        .map(|l| l.text.trim())
        .collect::<Vec<_>>()
        .join(" ");
    TextSelection {
        text: joined_text,
        quads,
    }
}

pub fn select_text_in_rect(
    pdfium: &Pdfium,
    bytes: &[u8],
    index: u16,
    left: f32,
    bottom: f32,
    right: f32,
    top: f32,
) -> Result<Option<TextSelection>> {
    let lines = gather_lines(pdfium, bytes, index, left, bottom, right, top)?;
    if lines.is_empty() {
        return Ok(None);
    }
    Ok(Some(lines_to_selection(&lines)))
}

/// Find the text a drag from `(start_x, start_y)` to `(end_x, end_y)` (PDF points) covers on
/// page `index`, the way a real text editor's click-drag selection works rather than a plain
/// rectangle: dragging straight down through the middle of a paragraph selects each
/// in-between line in full — left edge to right edge — not just the narrow column directly
/// under the pointer, and only the first and last lines are trimmed to the actual start/end
/// x position. `None` if the drag covers no text.
///
/// PDF y is bottom-up, so "top" (reading-order start) is whichever endpoint has the larger
/// y — direction-agnostic, a drag can run in either direction. A single-line drag has no
/// "middle" to leave untrimmed, so it degenerates to the same rectangle behaviour as
/// `select_text_in_rect`.
pub fn select_text_range(
    pdfium: &Pdfium,
    bytes: &[u8],
    index: u16,
    start_x: f32,
    start_y: f32,
    end_x: f32,
    end_y: f32,
) -> Result<Option<TextSelection>> {
    let ((top_x, top_y), (bottom_x, bottom_y)) = if start_y >= end_y {
        ((start_x, start_y), (end_x, end_y))
    } else {
        ((end_x, end_y), (start_x, start_y))
    };

    // Wider than any real PDF page (in points), so the initial gather is unconstrained
    // horizontally — trimming to the actual click x happens after, only on the end lines.
    const WIDE: f32 = 100_000.0;
    let mut lines = gather_lines(pdfium, bytes, index, -WIDE, bottom_y, WIDE, top_y)?;
    if lines.is_empty() {
        return Ok(None);
    }

    if lines.len() == 1 {
        let (lo, hi) = (top_x.min(bottom_x), top_x.max(bottom_x));
        trim_line(&mut lines[0], Some(lo), Some(hi));
    } else {
        let last = lines.len() - 1;
        trim_line(&mut lines[0], Some(top_x), None);
        trim_line(&mut lines[last], None, Some(bottom_x));
    }
    lines.retain(|l| !l.chars.is_empty());
    if lines.is_empty() {
        return Ok(None);
    }
    Ok(Some(lines_to_selection(&lines)))
}

/// Blend semi-transparent highlight rectangles into an already-rendered page's pixels, given
/// the page's PDF-space size (so PDF-space quadpoints — the same anchor `fond-bib`'s
/// `Annotation` stores — can be scaled onto the render's pixel grid) and a straight RGBA
/// colour. Each quad's axis-aligned bounding box is filled, not the exact quad polygon — fine
/// for the rectangular highlights this viewer creates; a rotated PDF-native quad would blend
/// as its bounding box.
///
/// Pure pixel math, no PDFium involved — unit-testable without a real PDF.
pub fn blend_highlights(
    page: &mut RenderedPage,
    page_width_pts: f32,
    page_height_pts: f32,
    quads: &[[f64; 8]],
    rgba: [u8; 4],
) {
    if page_width_pts <= 0.0 || page_height_pts <= 0.0 || page.width == 0 || page.height == 0 {
        return;
    }
    let scale_x = page.width as f32 / page_width_pts;
    let scale_y = page.height as f32 / page_height_pts;
    let alpha = rgba[3] as f32 / 255.0;

    for q in quads {
        let xs = [q[0], q[2], q[4], q[6]];
        let ys = [q[1], q[3], q[5], q[7]];
        let min_x = xs.iter().cloned().fold(f64::INFINITY, f64::min) as f32;
        let max_x = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max) as f32;
        let min_y = ys.iter().cloned().fold(f64::INFINITY, f64::min) as f32;
        let max_y = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max) as f32;

        let px0 = (min_x * scale_x).round().clamp(0.0, page.width as f32) as u32;
        let px1 = (max_x * scale_x).round().clamp(0.0, page.width as f32) as u32;
        // PDF y is bottom-up; pixel y is top-down.
        let py0 = ((page_height_pts - max_y) * scale_y)
            .round()
            .clamp(0.0, page.height as f32) as u32;
        let py1 = ((page_height_pts - min_y) * scale_y)
            .round()
            .clamp(0.0, page.height as f32) as u32;

        for y in py0..py1 {
            for x in px0..px1 {
                let idx = ((y * page.width + x) * 4) as usize;
                if idx + 3 >= page.rgba.len() {
                    continue;
                }
                for (channel, &fg) in rgba.iter().enumerate().take(3) {
                    let bg = page.rgba[idx + channel] as f32;
                    page.rgba[idx + channel] =
                        (bg * (1.0 - alpha) + fg as f32 * alpha).round() as u8;
                }
            }
        }
    }
}

/// Which kind of markup annotation `blend_annotations` should render a quad as. `Highlight`
/// fills the quad edge to edge, same as `blend_highlights`; `Underline`/`Strikeout` narrow it
/// to a thin band first, so it reads as a line rather than a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkupKind {
    Highlight,
    Underline,
    Strikeout,
}

/// As `blend_highlights`, but per-quad `kind`-aware — the PDF-reader render path's single
/// entry point for drawing all three drawable markup kinds (`AnnotationKind::Note` has no
/// quad to draw; it's marginal, not on-page). Reuses `blend_highlights`'s own pixel math
/// (band-narrowed for Underline/Strikeout) rather than duplicating it, so the fill/scale/
/// alpha logic — and its existing tests — stay the single source of truth.
pub fn blend_annotations(
    page: &mut RenderedPage,
    page_width_pts: f32,
    page_height_pts: f32,
    items: &[(MarkupKind, [f64; 8])],
    rgba: [u8; 4],
) {
    for (kind, q) in items {
        let narrowed = match kind {
            MarkupKind::Highlight => *q,
            // A thin band near the bottom of the text line.
            MarkupKind::Underline => narrow_band(q, 0.0, 0.18),
            // A thin band through the vertical middle.
            MarkupKind::Strikeout => narrow_band(q, 0.42, 0.58),
        };
        blend_highlights(page, page_width_pts, page_height_pts, &[narrowed], rgba);
    }
}

/// Replace quad `q`'s vertical extent with the band from `frac_bottom` to `frac_top` of its
/// own height (both in `[0.0, 1.0]`, measured up from the quad's bottom edge) — how
/// `blend_annotations` turns a full text-line quad into a thin underline/strikeout bar.
fn narrow_band(q: &[f64; 8], frac_bottom: f64, frac_top: f64) -> [f64; 8] {
    let ys = [q[1], q[3], q[5], q[7]];
    let min_y = ys.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_y = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let height = max_y - min_y;
    let top = min_y + height * frac_top;
    let bottom = min_y + height * frac_bottom;
    // top-left, top-right, bottom-left, bottom-right — same convention as `rect_to_quad`.
    [q[0], top, q[2], top, q[4], bottom, q[6], bottom]
}

/// Scan extracted text for the first DOI (`10.NNNN/...`). Returns it normalized (trailing
/// punctuation trimmed), or `None`. Dependency-free scanner — no regex crate.
pub fn find_doi(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(pos) = text[i..].find("10.") {
        let start = i + pos;
        // Require the "10." to start a token (not mid-number).
        if start > 0 && bytes[start - 1].is_ascii_digit() {
            i = start + 3;
            continue;
        }
        let mut j = start + 3;
        // At least four registrant digits.
        let digit_start = j;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j - digit_start >= 4 && j < bytes.len() && bytes[j] == b'/' {
            j += 1;
            let suffix_start = j;
            while j < bytes.len() && is_doi_char(bytes[j]) {
                j += 1;
            }
            if j > suffix_start {
                let doi = text[start..j].trim_end_matches(['.', ',', ';', ')', ']']);
                return Some(doi.to_string());
            }
        }
        i = start + 3;
    }
    None
}

fn is_doi_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b';' | b'(' | b')' | b'/' | b':')
}

/// Scan extracted text for an ISBN-10 or ISBN-13, checksum-validated so stray 10/13-digit
/// numbers (page ranges, figure numbers, other books' ISBNs quoted in a bibliography) are not
/// mistaken for the document's own identifier. Two passes, first match wins:
///
/// 1. Immediately after an "ISBN" label (case-insensitive) — the common case on a copyright
///    page, e.g. `ISBN 978-0-14-044913-6` or `ISBN-13: 9780140449136`.
/// 2. Anywhere in the text, for a checksum-valid ISBN-13 starting `978`/`979` — that prefix
///    plus a passing checksum is specific enough to trust without a label.
///
/// An unlabeled ISBN-10 is never accepted: its plain 10-digit form has no distinguishing
/// prefix, so without a label it's indistinguishable from any other 10-digit number.
///
/// Like `find_doi`, this takes the *first* match in document order, so a book's own ISBN
/// (front matter, within the first few pages) is expected to be found before any ISBN quoted
/// later in a bibliography or index — the same ordering assumption `find_doi` already relies
/// on for DOIs.
pub fn find_isbn(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();

    let mut search_from = 0;
    while let Some(pos) = lower[search_from..].find("isbn") {
        let label_end = search_from + pos + 4;
        if let Some(isbn) = scan_isbn_candidate(text, label_end) {
            return Some(isbn);
        }
        search_from = label_end;
    }

    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let is_prefix = matches!(&bytes[i..i + 3], b"978" | b"979");
        if is_prefix && (i == 0 || !bytes[i - 1].is_ascii_digit()) {
            if let Some(isbn) = scan_isbn_candidate(text, i) {
                if isbn.len() == 13 {
                    return Some(isbn);
                }
            }
        }
        i += 1;
    }
    None
}

/// From byte offset `start` (must be a char boundary) in `text`, skip past label punctuation
/// and an optional "10"/"13" edition-label token (as in "ISBN-13:"), then greedily consume an
/// ISBN-shaped run of digits/hyphens/spaces/dashes (with an optional trailing check-digit
/// `X`), and return it if the first 13 or first 10 digits checksum-validate.
fn scan_isbn_candidate(text: &str, start: usize) -> Option<String> {
    let chars: Vec<char> = text[start..].chars().take(40).collect();
    let mut i = 0;
    let lead = i;
    while i < chars.len() && i < lead + 8 && !chars[i].is_ascii_digit() {
        i += 1;
    }
    // Skip a bare "10"/"13" label token (e.g. the "-13" in "ISBN-13:") — distinct from the
    // real ISBN because it's exactly two digits followed by a non-digit separator.
    if i + 1 < chars.len() && chars[i].is_ascii_digit() && chars[i + 1].is_ascii_digit() {
        let two: String = chars[i..i + 2].iter().collect();
        let after = i + 2;
        let is_label_token = (two == "10" || two == "13")
            && chars
                .get(after)
                .map(|c| !c.is_ascii_digit())
                .unwrap_or(true);
        if is_label_token {
            i = after;
            let lead2 = i;
            while i < chars.len() && i < lead2 + 8 && !chars[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    if i >= chars.len() {
        return None;
    }

    let mut digits = String::new();
    let mut consumed = 0;
    while i < chars.len() && consumed < 24 && digits.len() < 13 {
        let c = chars[i];
        if c.is_ascii_digit() {
            digits.push(c);
        } else if c == 'x' || c == 'X' {
            digits.push('X');
        } else if matches!(c, '-' | ' ' | '\u{2013}' | '\u{2014}') {
            // separator, skip
        } else {
            break;
        }
        i += 1;
        consumed += 1;
    }

    if digits.len() >= 13 && is_valid_isbn13(&digits[..13]) {
        return Some(digits[..13].to_string());
    }
    if digits.len() >= 10 && is_valid_isbn10(&digits[..10]) {
        return Some(digits[..10].to_string());
    }
    None
}

fn is_valid_isbn13(s: &str) -> bool {
    if s.len() != 13 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let sum: u32 = s
        .bytes()
        .enumerate()
        .map(|(i, b)| {
            let d = (b - b'0') as u32;
            if i % 2 == 0 {
                d
            } else {
                d * 3
            }
        })
        .sum();
    sum % 10 == 0
}

fn is_valid_isbn10(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 10 || !bytes[..9].iter().all(|b| b.is_ascii_digit()) {
        return false;
    }
    if !(bytes[9].is_ascii_digit() || bytes[9] == b'X') {
        return false;
    }
    let sum: u32 = bytes
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            let d = if b == b'X' { 10 } else { (b - b'0') as u32 };
            d * (10 - i as u32)
        })
        .sum();
    sum % 11 == 0
}

/// Extract the text layer from a PDF file on disk.
pub fn extract_text_from_file(pdfium: &Pdfium, path: &Path) -> Result<PdfText> {
    let bytes = std::fs::read(path).map_err(|e| DocError::LibraryLoad {
        path: path.display().to_string(),
        message: format!("could not read PDF: {e}"),
    })?;
    extract_text(pdfium, &bytes)
}

#[cfg(test)]
mod tests {
    use super::{
        blend_annotations, blend_highlights, find_doi, find_isbn, looks_like_pdf, MarkupKind,
        RenderedPage,
    };

    #[test]
    fn looks_like_pdf_checks_header_magic_only() {
        let dir =
            std::env::temp_dir().join(format!("fond-doc-pdf-test-{}-sniff", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let pdf = dir.join("no_extension_at_all");
        std::fs::write(&pdf, b"%PDF-1.7\n%rest of file is irrelevant here").unwrap();
        assert!(looks_like_pdf(&pdf));

        let not_pdf = dir.join("some_file");
        std::fs::write(&not_pdf, b"not a pdf").unwrap();
        assert!(!looks_like_pdf(&not_pdf));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn finds_labeled_isbn13_with_hyphens() {
        assert_eq!(
            find_isbn("Copyright page.\nISBN 978-0-14-044913-6\nPenguin Classics"),
            Some("9780140449136".to_string())
        );
    }

    #[test]
    fn finds_isbn13_label_after_edition_token() {
        assert_eq!(
            find_isbn("ISBN-13: 9780140449136 (paperback)"),
            Some("9780140449136".to_string())
        );
        assert_eq!(
            find_isbn("ISBN-10: 0141439513"),
            Some("0141439513".to_string())
        );
    }

    #[test]
    fn finds_labeled_isbn10_with_check_digit_x() {
        // 043942089X is a real, checksum-valid ISBN-10 (with trailing check digit X).
        assert_eq!(
            find_isbn("ISBN: 0-439-42089-X"),
            Some("043942089X".to_string())
        );
    }

    #[test]
    fn finds_unlabeled_isbn13_by_prefix_and_checksum() {
        assert_eq!(
            find_isbn("some text 9780140449136 more text"),
            Some("9780140449136".to_string())
        );
    }

    #[test]
    fn rejects_unlabeled_isbn10_and_bad_checksums() {
        // No label and no 978/979 prefix: an ISBN-10 alone is not trusted.
        assert_eq!(find_isbn("call us at 0141439513 today"), None);
        // Labeled but checksum-invalid (last digit tampered).
        assert_eq!(find_isbn("ISBN 978-0-14-044913-7"), None);
        assert_eq!(find_isbn("no identifier here"), None);
    }

    #[test]
    fn finds_doi_in_text() {
        assert_eq!(
            find_doi("see https://doi.org/10.1093/mind/LIX.236.433 for more"),
            Some("10.1093/mind/LIX.236.433".to_string())
        );
        assert_eq!(
            find_doi("DOI:10.1000/xyz123. Next sentence."),
            Some("10.1000/xyz123".to_string())
        );
        assert_eq!(find_doi("no identifier here"), None);
        // A bare version number must not be mistaken for a DOI.
        assert_eq!(find_doi("version 10.2 of the tool"), None);
    }

    fn white_page(size: u32) -> RenderedPage {
        RenderedPage {
            width: size,
            height: size,
            rgba: vec![255; (size * size * 4) as usize],
        }
    }

    #[test]
    fn blend_highlights_tints_the_covered_region() {
        // A 10x10px render of a 10x10pt page: 1px == 1pt, no scaling to reason about.
        let mut page = white_page(10);
        // Quad covers x in [2,8], y in [2,8] in PDF space (bottom-up).
        let quad = [2.0, 8.0, 8.0, 8.0, 2.0, 2.0, 8.0, 2.0];
        blend_highlights(&mut page, 10.0, 10.0, &[quad], [255, 200, 0, 128]);

        // Center pixel (5,5) is inside the quad and inside the flipped pixel-space box.
        let idx = ((5 * page.width + 5) * 4) as usize;
        assert_ne!(
            &page.rgba[idx..idx + 3],
            &[255, 255, 255],
            "center pixel should be tinted"
        );
        // Corner pixel (0,0) is outside the quad and must stay untouched.
        assert_eq!(&page.rgba[0..3], &[255, 255, 255]);
    }

    #[test]
    fn blend_highlights_is_a_noop_for_degenerate_page_size() {
        let mut page = white_page(4);
        let before = page.rgba.clone();
        blend_highlights(
            &mut page,
            0.0,
            10.0,
            &[[0.0, 4.0, 4.0, 4.0, 0.0, 0.0, 4.0, 0.0]],
            [255, 0, 0, 255],
        );
        assert_eq!(page.rgba, before);
    }

    #[test]
    fn blend_annotations_fills_the_whole_quad_for_highlight() {
        // A 10x10px render of a 10x10pt page — quad spans y in [2, 8].
        let mut highlight_page = white_page(10);
        let quad = [2.0, 8.0, 8.0, 8.0, 2.0, 2.0, 8.0, 2.0];
        blend_annotations(
            &mut highlight_page,
            10.0,
            10.0,
            &[(MarkupKind::Highlight, quad)],
            [255, 200, 0, 255],
        );
        let mut direct = white_page(10);
        blend_highlights(&mut direct, 10.0, 10.0, &[quad], [255, 200, 0, 255]);
        assert_eq!(
            highlight_page.rgba, direct.rgba,
            "Highlight kind should blend identically to blend_highlights"
        );
    }

    #[test]
    fn blend_annotations_narrows_underline_and_strikeout_to_thin_bands() {
        // A 10x10px render of a 10x10pt page, quad spans the full page (y in [0, 10]).
        let quad = [0.0, 10.0, 10.0, 10.0, 0.0, 0.0, 10.0, 0.0];
        let rgba = [255, 0, 0, 255];

        let mut underline_page = white_page(10);
        blend_annotations(
            &mut underline_page,
            10.0,
            10.0,
            &[(MarkupKind::Underline, quad)],
            rgba,
        );
        // Underline band is near the bottom of the quad — pixel-space y near the *bottom* of
        // the image, since PDF y is bottom-up and pixel y is top-down. Row 9 (bottom-most)
        // should be tinted; row 0 (top) should not.
        let bottom_row_idx = ((9 * underline_page.width) * 4) as usize;
        let top_row_idx = 0usize;
        assert_ne!(
            &underline_page.rgba[bottom_row_idx..bottom_row_idx + 3],
            &[255, 255, 255]
        );
        assert_eq!(
            &underline_page.rgba[top_row_idx..top_row_idx + 3],
            &[255, 255, 255]
        );

        let mut strikeout_page = white_page(10);
        blend_annotations(
            &mut strikeout_page,
            10.0,
            10.0,
            &[(MarkupKind::Strikeout, quad)],
            rgba,
        );
        // Strikeout band is through the middle — row 5 tinted, rows 0 and 9 not.
        let middle_row_idx = ((5 * strikeout_page.width) * 4) as usize;
        assert_ne!(
            &strikeout_page.rgba[middle_row_idx..middle_row_idx + 3],
            &[255, 255, 255]
        );
        assert_eq!(
            &strikeout_page.rgba[top_row_idx..top_row_idx + 3],
            &[255, 255, 255]
        );
        assert_eq!(
            &strikeout_page.rgba[bottom_row_idx..bottom_row_idx + 3],
            &[255, 255, 255]
        );
    }
}
