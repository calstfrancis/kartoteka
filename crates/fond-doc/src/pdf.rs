//! PDF text extraction via PDFium (Milestone 4). Binding to the native PDFium library is
//! resolved at runtime; nothing here needs PDFium present at build time.

use std::path::{Path, PathBuf};

use pdfium_render::prelude::Pdfium;

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

/// Extract the text layer from a PDF file on disk.
pub fn extract_text_from_file(pdfium: &Pdfium, path: &Path) -> Result<PdfText> {
    let bytes = std::fs::read(path).map_err(|e| DocError::LibraryLoad {
        path: path.display().to_string(),
        message: format!("could not read PDF: {e}"),
    })?;
    extract_text(pdfium, &bytes)
}
