//! `fond-doc` — PDF rendering, text extraction, and (later) annotation import/export,
//! shared across the Fond suite. Text extraction feeds the search index (`fond-index`);
//! the annotation *sidecar* format itself lives in `fond-bib`. Binds to a native PDFium
//! library at runtime — nothing here requires PDFium at build time, and the public API
//! exposes no UI-framework types (`docs/ARCHITECTURE.md` §4).

pub mod annotation;
pub mod epub;
pub mod error;
pub mod pdf;

pub use epub::{extract_metadata as extract_epub_metadata, EpubMetadata};

pub use annotation::{
    embed_highlights, extract_annotations, AnnotationToEmbed, PdfAnnotation, PdfAnnotationKind,
};
pub use error::{DocError, Result};
pub use pdf::{
    bind_pdfium, blend_highlights, extract_metadata, extract_text, extract_text_from_file,
    find_doi, find_isbn, page_count, page_size, render_page, select_text_in_rect, PdfMeta, PdfText,
    RenderedPage, TextSelection,
};

/// Re-exported so callers can name the binding handle without depending on `pdfium-render`.
pub use pdfium_render::prelude::Pdfium;
