//! `fond-bib` — the Hayagriva bibliography model and the on-disk vault layout shared
//! across the Fond suite (Kartoteka, Zerkalo, Skrizhal).
//!
//! The files on disk are authoritative; `library.yml` is derived and regenerated here.
//! This crate's public API exposes no UI-framework types (`docs/ARCHITECTURE.md` §4) and
//! re-exports the Hayagriva types callers need so they need not depend on `hayagriva`
//! directly for common cases.

pub mod collection;
pub mod entry;
pub mod error;
pub mod key;
pub mod library;
pub mod note;

pub use collection::Collection;
pub use error::{BibError, Result};
pub use library::{FsckReport, Library};
pub use note::{Attachment, Note, NoteFrontmatter, ReadStatus};

// Re-export the Hayagriva entry types so downstream crates can name them without a direct
// `hayagriva` dependency.
pub use hayagriva::Entry;
pub use hayagriva::Library as HayagrivaLibrary;
