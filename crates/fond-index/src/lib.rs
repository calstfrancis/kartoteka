//! `fond-index` — tantivy full-text search over a library's metadata, note prose,
//! annotation snippets, and PDF text. The index is derived state under `.kartoteka/`,
//! rebuildable from the authoritative files (`docs/ARCHITECTURE.md` §1). Public API exposes
//! no UI-framework types.

pub mod error;
pub mod index;

pub use error::{IndexError, Result};
pub use index::{SearchHit, SearchIndex};
