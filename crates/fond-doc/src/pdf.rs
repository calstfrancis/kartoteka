//! PDF text extraction via PDFium (Milestone 4). Binding to the native PDFium library is
//! resolved at runtime; nothing here needs PDFium present at build time.

use std::path::{Path, PathBuf};

use pdfium_render::prelude::{PdfDocumentMetadataTagType, Pdfium};

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

/// Page count of a PDF file (cheap sniff for attachment records).
pub fn page_count(pdfium: &Pdfium, bytes: &[u8]) -> Result<u16> {
    Ok(pdfium.load_pdf_from_byte_slice(bytes, None)?.pages().len())
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
    use super::find_doi;

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
}
