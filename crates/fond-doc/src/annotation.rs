//! Import and export of *embedded* PDF markup annotations (Milestone 4.4). Import reads
//! highlights/underlines/strikeouts out of a PDF (with their highlighted text, so they can
//! be re-anchored to a differently-produced copy); export writes highlights back into a
//! PDF. Plain data types only — the sidecar format lives in `fond-bib`.

use pdfium_render::prelude::*;

use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfAnnotationKind {
    Highlight,
    Underline,
    Strikeout,
}

/// A markup annotation read from a PDF.
#[derive(Debug, Clone)]
pub struct PdfAnnotation {
    pub kind: PdfAnnotationKind,
    /// 1-based page number.
    pub page: u16,
    /// Highlight quads (8 floats each), in PDF user space.
    pub quadpoints: Vec<[f32; 8]>,
    /// The annotation's own note text, if any.
    pub contents: Option<String>,
    /// The highlighted text extracted from the page's text layer — the re-anchoring key.
    pub snippet: Option<String>,
}

/// A markup annotation to write into a PDF (export). Only highlights are supported for
/// writing (PDFium's high-level API exposes highlight creation).
#[derive(Debug, Clone)]
pub struct AnnotationToEmbed {
    pub page: u16,
    pub quadpoints: Vec<[f32; 8]>,
    pub contents: Option<String>,
}

fn rect_to_quad(r: &PdfRect) -> [f32; 8] {
    let (left, right, top, bottom) = (
        r.left().value,
        r.right().value,
        r.top().value,
        r.bottom().value,
    );
    // top-left, top-right, bottom-left, bottom-right
    [left, top, right, top, left, bottom, right, bottom]
}

fn quad_bounds(q: &[f32; 8]) -> PdfRect {
    let xs = [q[0], q[2], q[4], q[6]];
    let ys = [q[1], q[3], q[5], q[7]];
    let left = xs.iter().copied().fold(f32::INFINITY, f32::min);
    let right = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let bottom = ys.iter().copied().fold(f32::INFINITY, f32::min);
    let top = ys.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    // PdfRect::new(bottom, left, top, right)
    PdfRect::new(
        PdfPoints::new(bottom),
        PdfPoints::new(left),
        PdfPoints::new(top),
        PdfPoints::new(right),
    )
}

/// Extract embedded highlight/underline/strikeout annotations from PDF bytes.
pub fn extract_annotations(pdfium: &Pdfium, bytes: &[u8]) -> Result<Vec<PdfAnnotation>> {
    let document = pdfium.load_pdf_from_byte_slice(bytes, None)?;
    let mut out = Vec::new();

    for (page_index, page) in document.pages().iter().enumerate() {
        let text = page.text().ok();
        for annotation in page.annotations().iter() {
            let kind = match annotation.annotation_type() {
                PdfPageAnnotationType::Highlight => PdfAnnotationKind::Highlight,
                PdfPageAnnotationType::Underline => PdfAnnotationKind::Underline,
                PdfPageAnnotationType::Strikeout => PdfAnnotationKind::Strikeout,
                _ => continue,
            };

            let quadpoints = annotation
                .bounds()
                .ok()
                .map(|r| vec![rect_to_quad(&r)])
                .unwrap_or_default();

            let contents = annotation.contents().filter(|s| !s.trim().is_empty());
            let snippet = text
                .as_ref()
                .and_then(|t| t.for_annotation(&annotation).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());

            out.push(PdfAnnotation {
                kind,
                page: (page_index + 1) as u16,
                quadpoints,
                contents,
                snippet,
            });
        }
    }
    Ok(out)
}

/// Write highlight annotations into PDF bytes, returning the modified PDF. Each item's
/// quads become a highlight with the given note contents.
pub fn embed_highlights(
    pdfium: &Pdfium,
    bytes: &[u8],
    annotations: &[AnnotationToEmbed],
) -> Result<Vec<u8>> {
    let document = pdfium.load_pdf_from_byte_slice(bytes, None)?;
    let page_count = document.pages().len();

    for item in annotations {
        let index = item.page.saturating_sub(1);
        if index >= page_count {
            continue;
        }
        let mut page = document.pages().get(index)?;
        let mut annotation = page.annotations_mut().create_highlight_annotation()?;

        if let Some(first) = item.quadpoints.first() {
            annotation.set_bounds(quad_bounds(first))?;
        }
        for quad in &item.quadpoints {
            annotation
                .attachment_points_mut()
                .create_attachment_point_at_end(PdfQuadPoints::new_from_values(
                    quad[0], quad[1], quad[2], quad[3], quad[4], quad[5], quad[6], quad[7],
                ))?;
        }
        if let Some(contents) = &item.contents {
            annotation.set_contents(contents)?;
        }
    }

    Ok(document.save_to_bytes()?)
}
