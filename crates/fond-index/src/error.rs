use std::path::PathBuf;

/// Errors from building or querying the search index.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("search index error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),

    #[error("i/o error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Bib(#[from] fond_bib::BibError),

    #[error("invalid search query: {0}")]
    Query(String),
}

pub type Result<T> = std::result::Result<T, IndexError>;
