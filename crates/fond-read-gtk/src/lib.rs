//! GTK4/libadwaita PDF and EPUB readers, shared across the Fond suite.
//!
//! Rendering, text extraction, search and the annotation schema come from `fond-doc` and
//! `fond-bib`; this crate is the widget layer over them — paged and continuous PDF views,
//! drag-to-annotate, EPUB chapter navigation, undo/redo, and the annotations dialog.
//!
//! The crate knows nothing about how the embedding application identifies a document.
//! Everything it needs comes through [`ReaderHost`]: Kartoteka's implementation resolves a
//! citation key against an open library, and Sputnik's routes a library reading and a local
//! course material to two different places behind the same methods. Nothing here may learn
//! what a citation key is — it is handed a blob path and a host, and that is all it knows.
//!
//! Extracted from Kartoteka's `app_window.rs`; see that repo's `docs/READER-EXTRACTION.md`
//! for the boundary survey this shape came from.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::Orientation;
use libadwaita as adw;

pub mod annotations;
pub mod epub;
pub mod pdf;

/// A slot holding a "rebuild this list" closure, filled in after the widgets it rebuilds
/// exist. Shared by both readers' notes sidebars.
pub type RebuildCell = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// Everything the reader needs from whoever embedded it.
///
/// One instance per open document, constructed by the host. That is what lets the reader
/// stay ignorant of how a document is identified or where its annotations are stored:
/// Kartoteka's implementation closes over an open `Library` and a citation key, and
/// Sputnik's will route a library reading and a local course material to two different
/// places behind the same six methods.
pub trait ReaderHost {
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
pub fn existing_window(hash: &str) -> Option<adw::Window> {
    OPEN_READERS.with(|r| r.borrow().get(hash).cloned())
}

/// Surface the reader already open on `hash`, reporting whether there was one.
pub fn present_existing(hash: &str) -> bool {
    match existing_window(hash) {
        Some(window) => {
            window.present();
            true
        }
        None => false,
    }
}

pub fn register_window(hash: &str, window: &adw::Window) {
    OPEN_READERS.with(|r| r.borrow_mut().insert(hash.to_string(), window.clone()));
}

pub fn unregister_window(hash: &str) {
    OPEN_READERS.with(|r| r.borrow_mut().remove(hash));
}

/// A flat, left-aligned popover row. Deliberately a copy of `app_window`'s helper of the
/// same name rather than a shared import: this module is on its way into its own crate, and
/// an eighteen-line button helper is not worth a dependency back on the application. If the
/// two ever need to differ, they already can.
pub fn popover_button(label: &str, destructive: bool) -> gtk4::Button {
    let button = gtk4::Button::new();
    button.add_css_class("flat");
    if destructive {
        button.add_css_class("destructive-action");
    }
    let lbl = gtk4::Label::new(Some(label));
    lbl.set_xalign(0.0);
    lbl.set_halign(gtk4::Align::Start);
    button.set_child(Some(&lbl));
    button
}

/// The margined rule between logical groups of popover rows. Copied for the same reason as
/// [`popover_button`].
pub fn popover_separator() -> gtk4::Separator {
    let sep = gtk4::Separator::new(Orientation::Horizontal);
    sep.set_margin_top(4);
    sep.set_margin_bottom(4);
    sep
}
