/// Errors from PDF operations.
#[derive(Debug, thiserror::Error)]
pub enum DocError {
    #[error(
        "could not load the PDFium library ({path}): {message}\n\
             set PDFIUM_LIB_PATH to a directory containing libpdfium.so (or the file itself)"
    )]
    LibraryLoad { path: String, message: String },

    #[error("PDF error: {0}")]
    Pdfium(#[from] pdfium_render::prelude::PdfiumError),
}

pub type Result<T> = std::result::Result<T, DocError>;
