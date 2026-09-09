//! The PDF and EPUB readers.
//!
//! This module is on its way out of the application: it is being lifted into a
//! `fond-read-gtk` crate shared with Sputnik, so the reader is never written twice (see
//! `docs/READER-EXTRACTION.md`). It is therefore written as if it were already a separate
//! crate — it must not reach into `AppState`, `Widgets`, `Library`, or anything else the
//! application owns. Everything it needs from its embedder comes through [`ReaderHost`].
//!
//! Concretely: nothing in here may learn what a citation key is. Kartoteka identifies a
//! document by one; Sputnik will identify course materials by content hash instead
//! (`sputnik/docs/ARCHITECTURE.md` §3). The reader is handed a blob path and a host, and
//! that is all it knows.

use std::cell::RefCell;
use std::collections::HashMap;

use libadwaita as adw;
use libadwaita::prelude::*;

/// Everything the reader needs from whoever embedded it.
///
/// One instance per open document, constructed by the host. That is what lets the reader
/// stay ignorant of how a document is identified or where its annotations are stored:
/// Kartoteka's implementation closes over an open `Library` and a citation key, and
/// Sputnik's will route a library reading and a local course material to two different
/// places behind the same six methods.
pub(crate) trait ReaderHost {
    /// The document's annotation sidecar — an empty one if it has none yet, or if it
    /// could not be read. Infallible by design: the empty fallback has to be built by the
    /// host, because a sidecar is born knowing the key it will be written back to, and that
    /// key is exactly what the reader must not know. A reader that built its own fallback
    /// would save it to the wrong file.
    fn load_annotations(&self) -> fond_bib::AnnotationSidecar;

    /// Persist the sidecar. Called on every annotation add, edit, and delete — the reader
    /// holds the authoritative in-memory copy for the session and rewrites the whole
    /// sidecar each time.
    fn save_annotations(&self, sidecar: &fond_bib::AnnotationSidecar) -> Result<(), String>;

    /// Persist reading position, on reader close. Best-effort: a failure is not surfaced,
    /// since losing a resume position is not worth interrupting a close for.
    fn save_progress(&self, progress: fond_bib::Progress);

    /// The manual printed-page-numbering override, if the user has set one. Consulted only
    /// when the PDF declares no `/PageLabels` of its own, which the reader decides.
    fn page_label_override(&self) -> Option<fond_bib::PageLabelOverride>;

    /// Record (or, with `None`, clear) the manual page-numbering override.
    fn set_page_label_override(&self, value: Option<fond_bib::PageLabelOverride>);

    /// Show a transient confirmation or error, however the host shows those.
    fn notify(&self, message: &str);
}

thread_local! {
    /// Reader windows currently open, keyed by the document's content hash — so this is
    /// really "the same file", not "the same entry".
    ///
    /// Opening a second reader on a document already open would give each one an
    /// independent in-memory sidecar snapshot, and both rewrite the whole sidecar on every
    /// save, so the last close would silently discard the other's annotations. Instead the
    /// second attempt surfaces the window that is already open.
    ///
    /// Process-global rather than per-library: entries are removed only on a reader's own
    /// `close-request`, never on a library switch, which matches the behaviour this
    /// replaced (`AppState.open_readers`). GTK is single-threaded, so a `thread_local` is
    /// the whole of the synchronisation story.
    static OPEN_READERS: RefCell<HashMap<String, adw::Window>> = RefCell::new(HashMap::new());
}

/// The reader already open on `hash`, if there is one.
pub(crate) fn existing_window(hash: &str) -> Option<adw::Window> {
    OPEN_READERS.with(|r| r.borrow().get(hash).cloned())
}

/// Surface the reader already open on `hash`, reporting whether there was one.
pub(crate) fn present_existing(hash: &str) -> bool {
    match existing_window(hash) {
        Some(window) => {
            window.present();
            true
        }
        None => false,
    }
}

pub(crate) fn register_window(hash: &str, window: &adw::Window) {
    OPEN_READERS.with(|r| r.borrow_mut().insert(hash.to_string(), window.clone()));
}

pub(crate) fn unregister_window(hash: &str) {
    OPEN_READERS.with(|r| r.borrow_mut().remove(hash));
}
