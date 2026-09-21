//! A content-addressed cache of text extracted from attachments, so a search-index rebuild
//! doesn't have to re-run PDFium/zip extraction over the whole library every time.
//!
//! Attachments are stored by content hash, so a cached entry is valid forever — there is no
//! invalidation, only "not extracted yet". The GUI rebuilds the index after nearly every edit
//! and used to pass "no text" for every attachment, silently wiping PDF/EPUB body text from
//! search until the next CLI `reindex`; reading from this cache makes those rebuilds keep it
//! without the cost of extracting again.

use std::fs;
use std::path::{Path, PathBuf};

/// What an extraction attempt produced.
pub enum Extraction {
    /// Extracted text (may legitimately be empty for an image-only PDF).
    Text(String),
    /// The extractor ran and there is nothing to index for this file (unreadable/corrupt).
    /// Remembered so it isn't retried on every rebuild.
    NoText,
    /// The extractor couldn't run at all (e.g. PDFium isn't installed). Not remembered —
    /// installing the library later should make it work.
    Unavailable,
}

fn cache_path(cache_dir: &Path, hex: &str) -> Option<PathBuf> {
    // The key is used as a filename; refuse anything but `[A-Za-z0-9_-]` so a hand-edited
    // record can't point outside the cache directory.
    (!hex.is_empty()
        && hex
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    .then(|| cache_dir.join(format!("{hex}.txt")))
}

/// Text for the attachment whose content hash is `hex`: from the cache if present, otherwise
/// (only when `extract_missing`) by running `extract` and caching the outcome. With
/// `extract_missing == false` this only ever reads — safe to call from a rebuild that must
/// stay fast. Returns `None` when there is no (non-empty) text.
pub fn cached_text(
    cache_dir: &Path,
    hex: &str,
    extract_missing: bool,
    extract: impl FnOnce() -> Extraction,
) -> Option<String> {
    let path = cache_path(cache_dir, hex)?;
    if let Ok(text) = fs::read_to_string(&path) {
        return (!text.is_empty()).then_some(text);
    }
    if !extract_missing {
        return None;
    }
    match extract() {
        Extraction::Text(text) => {
            store(&path, &text);
            (!text.is_empty()).then_some(text)
        }
        Extraction::NoText => {
            store(&path, "");
            None
        }
        Extraction::Unavailable => None,
    }
}

/// Best-effort write (temp file + rename so a crash never leaves a truncated entry that
/// would be read back as the attachment's whole text). A cache that can't be written just
/// means the next rebuild extracts again.
fn store(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        if fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let tmp = path.with_extension("txt.tmp");
    if fs::write(&tmp, text).is_ok() && fs::rename(&tmp, path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn extracts_once_then_serves_from_cache() {
        let dir = std::env::temp_dir().join(format!("fond-doc-cache-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let calls = Cell::new(0);
        let go = |allow| {
            cached_text(&dir, "abc123", allow, || {
                calls.set(calls.get() + 1);
                Extraction::Text("hello".into())
            })
        };
        assert_eq!(go(false), None, "read-only mode must not extract");
        assert_eq!(calls.get(), 0);
        assert_eq!(go(true).as_deref(), Some("hello"));
        assert_eq!(go(true).as_deref(), Some("hello"));
        assert_eq!(
            go(false).as_deref(),
            Some("hello"),
            "read-only mode still reads the cache"
        );
        assert_eq!(calls.get(), 1, "extracted more than once");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unavailable_is_not_cached_but_no_text_is() {
        let dir = std::env::temp_dir().join(format!("fond-doc-cache2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(
            cached_text(&dir, "aa", true, || Extraction::Unavailable),
            None
        );
        let ran = Cell::new(false);
        assert_eq!(
            cached_text(&dir, "aa", true, || {
                ran.set(true);
                Extraction::Text("x".into())
            })
            .as_deref(),
            Some("x")
        );
        assert!(ran.get(), "an Unavailable outcome must not be remembered");
        assert_eq!(cached_text(&dir, "bb", true, || Extraction::NoText), None);
        let ran = Cell::new(false);
        assert_eq!(
            cached_text(&dir, "bb", true, || {
                ran.set(true);
                Extraction::Text("y".into())
            }),
            None
        );
        assert!(!ran.get(), "a NoText outcome should be remembered");
        assert_eq!(
            cached_text(&dir, "../evil", true, || Extraction::Text("z".into())),
            None
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
