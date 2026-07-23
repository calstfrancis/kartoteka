use std::path::PathBuf;

/// Errors from git-backed vault operations and filesystem watching.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("git error: {0}")]
    Git(#[from] git2::Error),

    #[error("i/o error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("filesystem watch error: {0}")]
    Watch(String),
}

pub type Result<T> = std::result::Result<T, VaultError>;
