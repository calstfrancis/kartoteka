//! `fond-index` — tantivy full-text search and the SQLite derived metadata cache.
//!
//! Stub crate. Search grows in from Milestone 1 onward (see `docs/ROADMAP.md`); this crate
//! is scaffolded now only to fix the boundary. Whatever it stores is derived state under
//! `.kartoteka/` and must be fully rebuildable from the authoritative files — it may only
//! ever cache what the files already say (`docs/ARCHITECTURE.md` §1).
