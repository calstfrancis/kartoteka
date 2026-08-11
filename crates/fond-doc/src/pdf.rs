//! PDF text extraction via PDFium (Milestone 4). Binding to the native PDFium library is
//! resolved at runtime; nothing here needs PDFium present at build time.

use std::path::{Path, PathBuf};

use pdfium_render::prelude::{
    PdfDocumentMetadataTagType, PdfRect, PdfRenderConfig, Pdfium, Pixels,
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
pub fn select_text_in_rect(
    pdfium: &Pdfium,
    bytes: &[u8],
    index: u16,
    left: f32,
    bottom: f32,
    right: f32,
    top: f32,
) -> Result<Option<TextSelection>> {
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
    if hits.is_empty() {
        return Ok(None);
    }

    // Group consecutive hits (already in document order, which is reading order for normal
    // prose) into lines: a new hit starts a new line whenever its vertical range no longer
    // overlaps the current line's accumulated range.
    struct Line {
        left: f32,
        right: f32,
        bottom: f32,
        top: f32,
        text: String,
    }
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
                text: ch,
            });
        } else {
            let line = lines.last_mut().expect("just checked non-empty");
            line.left = line.left.min(l);
            line.right = line.right.max(r);
            line.bottom = line.bottom.min(b);
            line.top = line.top.max(t);
            line.text.push_str(&ch);
        }
    }

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

    Ok(Some(TextSelection {
        text: joined_text,
        quads,
    }))
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
                    page.rgba[idx + channel] = (bg * (1.0 - alpha) + fg as f32 * alpha).round() as u8;
                }
            }
        }
    }
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
    use super::{blend_highlights, find_doi, RenderedPage};

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
        assert_ne!(&page.rgba[idx..idx + 3], &[255, 255, 255], "center pixel should be tinted");
        // Corner pixel (0,0) is outside the quad and must stay untouched.
        assert_eq!(&page.rgba[0..3], &[255, 255, 255]);
    }

    #[test]
    fn blend_highlights_is_a_noop_for_degenerate_page_size() {
        let mut page = white_page(4);
        let before = page.rgba.clone();
        blend_highlights(&mut page, 0.0, 10.0, &[[0.0, 4.0, 4.0, 4.0, 0.0, 0.0, 4.0, 0.0]], [255, 0, 0, 255]);
        assert_eq!(page.rgba, before);
    }
}
