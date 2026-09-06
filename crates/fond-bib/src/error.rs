use std::path::PathBuf;

/// Errors from reading, writing, and validating the on-disk bibliography layout.
///
/// Concrete enum, never `Box<dyn Error>`, so it can cross the UI-agnostic seam into any
/// frontend (see `docs/ARCHITECTURE.md` §4).
#[derive(Debug, thiserror::Error)]
pub enum BibError {
    #[error("i/o error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse Hayagriva YAML in {path}: {message}")]
    Yaml { path: PathBuf, message: String },

    #[error("failed to parse YAML in {path}: {message}")]
    PlainYaml { path: PathBuf, message: String },

    #[error("entry file {path} must contain exactly one entry, found {found}")]
    NotSingleEntry { path: PathBuf, found: usize },

    #[error("entry file {path}: inner key '{inner}' does not match filename key '{file}'")]
    KeyMismatch {
        path: PathBuf,
        inner: String,
        file: String,
    },

    #[error("invalid note frontmatter in {path}: {message}")]
    Frontmatter { path: PathBuf, message: String },

    #[error("cannot derive a citation key: entry has neither an author nor a title")]
    UnkeyableEntry,

    #[error("import error: {message}")]
    Import { message: String },

    #[error("Zotero database error at {path}: {message}")]
    Zotero { path: PathBuf, message: String },

    #[error("not a valid library at {path}: {message}")]
    Layout { path: PathBuf, message: String },

    #[error("collection '{slug}': {message}")]
    Collection { slug: String, message: String },
}

impl BibError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        BibError::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, BibError>;
