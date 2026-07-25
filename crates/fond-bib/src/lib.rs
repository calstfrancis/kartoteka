//! `fond-bib` — the Hayagriva bibliography model and the on-disk vault layout shared
//! across the Fond suite (Kartoteka, Zerkalo, Skrizhal).
//!
//! The files on disk are authoritative; `library.yml` is derived and regenerated here.
//! This crate's public API exposes no UI-framework types (`docs/ARCHITECTURE.md` §4) and
//! re-exports the Hayagriva types callers need so they need not depend on `hayagriva`
//! directly for common cases.

#[cfg(feature = "acquire")]
pub mod acquire;
pub mod ai;
pub mod annotation;
pub mod collection;
pub mod entry;
pub mod error;
pub mod import;
pub mod key;
pub mod library;
pub mod note;
pub mod project;
pub mod relation;
pub mod render;
pub mod util;
pub mod zotero;

pub use ai::AiMetadata;
pub use annotation::{Annotation, AnnotationKind, AnnotationSidecar};
pub use collection::Collection;
pub use error::{BibError, Result};
pub use import::{ImportOptions, ImportReport};
pub use library::{FsckReport, Library, RelationReconcile, UsageMap};
pub use note::{Attachment, CitePrefs, Note, NoteFrontmatter, Progress, ReadStatus, Task};
pub use project::Project;
pub use relation::{Predicate, Relation};
pub use render::{resolve_style, style_from_csl, RenderedEntry};

// Re-export the CSL output format and style type so the CLI need not depend on
// `hayagriva` directly.
pub use hayagriva::citationberg::IndependentStyle;
pub use hayagriva::BufWriteFormat;

// Re-export the Hayagriva entry types so downstream crates can name them without a direct
// `hayagriva` dependency.
pub use hayagriva::Entry;
pub use hayagriva::Library as HayagrivaLibrary;
