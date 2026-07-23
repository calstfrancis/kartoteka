//! `fond-doc` — PDF rendering, text extraction, and (later) annotation import/export,
//! shared across the Fond suite. Text extraction feeds the search index (`fond-index`);
//! the annotation *sidecar* format itself lives in `fond-bib`. Binds to a native PDFium
//! library at runtime — nothing here requires PDFium at build time, and the public API
//! exposes no UI-framework types (`docs/ARCHITECTURE.md` §4).

pub mod error;
pub mod pdf;

pub use error::{DocError, Result};
pub use pdf::{bind_pdfium, extract_text, extract_text_from_file, PdfText};
