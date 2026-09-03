//! The "Bookshelf" view: a cover-grid alternative to the entries spreadsheet, narrowed to
//! `entry_type == "book"`. Built on top of the *same* sorted model the spreadsheet uses
//! (`app_window::build_entries_column_view`'s `sort_model`), so collection/search filtering
//! stays centralized in `refresh_list` — this module only layers a book-only `CustomFilter`
//! and cover art on top, it doesn't duplicate any filtering logic.
//!
//! Cover art comes from the OpenLibrary Covers API, looked up by ISBN
//! (`fond_bib::acquire::fetch_isbn_cover`) and cached on disk via `Library::store_cover`
//! (`.kartoteka/covers/<isbn>.jpg`, derived/gitignored state, never touching the entry YAML
//! or note frontmatter). A book with no ISBN, or no cover found, shows a plain placeholder
//! card with its title/author — never a broken image.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{gdk, glib};

use fond_bib::Library;

use super::app_window::entry_row::EntryRow;
use super::app_window::AppState;

const COVER_WIDTH: i32 = 110;
const COVER_HEIGHT: i32 = 165;

/// The built grid view, handed back to `app_window::build()` so it can place `scroller` in
/// the view-mode `Stack` and wire `selection` to the shared detail pane, exactly as it
/// already does for the spreadsheet's own `selection`.
pub(crate) struct BookshelfView {
    pub(crate) scroller: gtk4::ScrolledWindow,
    pub(crate) selection: gtk4::SingleSelection,
}

/// Build the cover grid over `sort_model`. `state` is used to reach the currently-open
/// `Library` (for cover cache lookups/writes) fresh on every cell bind, so the grid keeps
/// working across a library reload/reopen without being rebuilt itself.
pub(crate) fn build_bookshelf_view(
    sort_model: &gtk4::SortListModel,
    state: Rc<RefCell<AppState>>,
) -> BookshelfView {
    let filter = gtk4::CustomFilter::new(|obj| {
        obj.downcast_ref::<EntryRow>()
            .map(|r| r.entry_type() == "book")
            .unwrap_or(false)
    });
    let filter_model = gtk4::FilterListModel::new(Some(sort_model.clone()), Some(filter));
    let selection = gtk4::SingleSelection::new(Some(filter_model));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);

    // ISBNs already handed to a background fetch this session, so scrolling back and forth
    // over the same book doesn't re-fire the request. No retry/backoff queue — this is a
    // single-user local app, a simple "already tried" set is enough.
    let attempted: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(HashSet::new()));

    // ISBN -> the card `Box` currently bound to it, so a background fetch that resolves
    // after its originating `ListItem` has been recycled or dropped (this app rebuilds the
    // whole entries store, and everything downstream of it, twice on every library open —
    // both refreshes happen before any network fetch could possibly complete) can still find
    // and update whatever card is *currently* showing that ISBN. Deliberately not cleared on
    // unbind: a stale entry pointing at a since-recycled widget is harmless (the update just
    // lands on an orphaned `Box` nobody is looking at), and last-bind-wins keeps it correct
    // for the common case of a single card per ISBN.
    let bound_cards: Rc<RefCell<HashMap<String, gtk4::Box>>> = Rc::new(RefCell::new(HashMap::new()));

    let factory = gtk4::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
            return;
        };
        item.set_child(Some(&build_card()));
    });
    {
        let state = state.clone();
        let attempted = attempted.clone();
        let bound_cards = bound_cards.clone();
        factory.connect_bind(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
                return;
            };
            let (Some(row), Some(card)) = (
                item.item().and_downcast::<EntryRow>(),
                item.child().and_downcast::<gtk4::Box>(),
            ) else {
                return;
            };
            bind_card(&card, &row, &state, &attempted, &bound_cards);
        });
    }

    let grid = gtk4::GridView::new(Some(selection.clone()), Some(factory));
    grid.add_css_class("bookshelf-grid");
    grid.set_min_columns(2);
    grid.set_max_columns(12);

    let scroller = gtk4::ScrolledWindow::new();
    scroller.set_child(Some(&grid));
    scroller.set_vexpand(true);

    BookshelfView { scroller, selection }
}

/// One card: a cover slot (placeholder icon, filled in by a real cover when available) plus
/// always-visible title/author captions below it. Built once per `ListItem` in
/// `connect_setup`; `connect_bind` (`bind_card`) resets and repopulates it on every bind,
/// since GTK recycles `ListItem`s while scrolling.
fn build_card() -> gtk4::Box {
    let card = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    card.add_css_class("bookshelf-card");

    let cover_slot = gtk4::Overlay::new();
    cover_slot.set_width_request(COVER_WIDTH);
    cover_slot.set_height_request(COVER_HEIGHT);
    cover_slot.add_css_class("bookshelf-cover-slot");

    let placeholder_icon = gtk4::Image::from_icon_name("x-office-document-symbolic");
    placeholder_icon.set_pixel_size(40);
    placeholder_icon.add_css_class("bookshelf-placeholder-icon");
    cover_slot.set_child(Some(&placeholder_icon));

    let cover_picture = gtk4::Picture::new();
    cover_picture.set_content_fit(gtk4::ContentFit::Cover);
    cover_picture.add_css_class("bookshelf-cover");
    cover_picture.set_visible(false);
    cover_slot.add_overlay(&cover_picture);

    let badge_label = gtk4::Label::new(None);
    badge_label.add_css_class("bookshelf-badge");
    badge_label.set_halign(gtk4::Align::End);
    badge_label.set_valign(gtk4::Align::Start);
    badge_label.set_margin_top(4);
    badge_label.set_margin_end(4);
    badge_label.set_visible(false);
    cover_slot.add_overlay(&badge_label);

    let title_label = gtk4::Label::new(None);
    title_label.add_css_class("caption-heading");
    title_label.set_xalign(0.0);
    title_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    title_label.set_width_request(COVER_WIDTH);

    let author_label = gtk4::Label::new(None);
    author_label.add_css_class("dim-label");
    author_label.add_css_class("caption");
    author_label.set_xalign(0.0);
    author_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    author_label.set_width_request(COVER_WIDTH);

    card.append(&cover_slot);
    card.append(&title_label);
    card.append(&author_label);

    // Attached via glib qdata rather than fields on a custom widget type, matching the
    // existing "row.set_data(...)"/"row.data::<T>(...)" idiom already used elsewhere in
    // this crate (e.g. app_window.rs's collection-row slug) — `connect_bind` and a resolved
    // cover fetch both need to find these children given only the card `Box`.
    unsafe {
        card.set_data("cover-picture", cover_picture);
        card.set_data("placeholder-icon", placeholder_icon);
        card.set_data("title-label", title_label);
        card.set_data("author-label", author_label);
        card.set_data("badge-label", badge_label);
    }

    card
}

fn card_widget<T: Clone + 'static>(card: &gtk4::Box, key: &str) -> Option<T> {
    (unsafe { card.data::<T>(key) }).map(|p| unsafe { p.as_ref() }.clone())
}

/// Apply a cached cover file to a card, if it decodes cleanly. Shared by `bind_card`'s
/// synchronous cache-hit path and the async fetch's completion callback.
fn apply_cover(card: &gtk4::Box, path: &std::path::Path) {
    let (Some(cover_picture), Some(placeholder_icon)) = (
        card_widget::<gtk4::Picture>(card, "cover-picture"),
        card_widget::<gtk4::Image>(card, "placeholder-icon"),
    ) else {
        return;
    };
    if let Ok(texture) = gdk::Texture::from_filename(path) {
        cover_picture.set_paintable(Some(&texture));
        cover_picture.set_visible(true);
        placeholder_icon.set_visible(false);
    }
}

/// Reset and repopulate one card for `row`. Runs on every bind, including a recycled cell
/// previously showing a different entry — every visual field is set unconditionally (never
/// left over from the last binding) before deciding whether a cover needs fetching.
fn bind_card(
    card: &gtk4::Box,
    row: &EntryRow,
    state: &Rc<RefCell<AppState>>,
    attempted: &Rc<RefCell<HashSet<String>>>,
    bound_cards: &Rc<RefCell<HashMap<String, gtk4::Box>>>,
) {
    let (
        Some(title_label),
        Some(author_label),
        Some(cover_picture),
        Some(placeholder_icon),
        Some(badge_label),
    ) = (
        card_widget::<gtk4::Label>(card, "title-label"),
        card_widget::<gtk4::Label>(card, "author-label"),
        card_widget::<gtk4::Picture>(card, "cover-picture"),
        card_widget::<gtk4::Image>(card, "placeholder-icon"),
        card_widget::<gtk4::Label>(card, "badge-label"),
    )
    else {
        return;
    };

    title_label.set_text(&row.title());
    author_label.set_text(&row.author());

    cover_picture.set_paintable(None::<&gdk::Texture>);
    cover_picture.set_visible(false);
    placeholder_icon.set_visible(true);

    match row.status().as_str() {
        "reading" => {
            badge_label.set_text("Reading");
            badge_label.remove_css_class("bookshelf-badge-read");
            badge_label.add_css_class("bookshelf-badge-reading");
            badge_label.set_visible(true);
        }
        "read" => {
            badge_label.set_text("Read");
            badge_label.remove_css_class("bookshelf-badge-reading");
            badge_label.add_css_class("bookshelf-badge-read");
            badge_label.set_visible(true);
        }
        _ => badge_label.set_visible(false),
    }

    let isbn = row.isbn();
    if isbn.trim().is_empty() {
        return;
    }
    let Some(library) = state.borrow().library.clone() else {
        return;
    };

    bound_cards.borrow_mut().insert(isbn.clone(), card.clone());

    let cache_path = library.cover_cache_path(&isbn);
    if cache_path.is_file() {
        apply_cover(card, &cache_path);
        return;
    }

    if !attempted.borrow_mut().insert(isbn.clone()) {
        return;
    }
    spawn_cover_fetch(bound_cards.clone(), isbn, library);
}

/// Fetch `isbn`'s cover on a worker thread and cache it. On success, look up whichever card
/// is *currently* registered in `bound_cards` for this ISBN and apply the texture directly —
/// rather than holding onto the specific `ListItem`/`Box` that triggered the fetch, which may
/// since have been recycled or dropped by a model refresh (this app resets the whole entries
/// store, and everything downstream of it, twice on every library open, well before a network
/// fetch could complete). Forcing GTK to *rebind* via a synthetic `items_changed` on the
/// selection/filter/sort models was tried and does not work here — `GridView` does not treat
/// an externally-emitted "items changed" on an unrelated position range as a cue to rebind
/// already-bound cells showing the same underlying objects, unlike the direct
/// store→sorter→selection→view chain the existing bulk-select checkbox refresh in
/// `app_window.rs` relies on (which has no extra `FilterListModel` in the middle). Updating
/// the widget directly sidesteps all of that. No toast on failure or "no cover" — a missing
/// thumbnail isn't worth interrupting the user for, and the placeholder from `bind_card`
/// already stands.
#[allow(deprecated)]
fn spawn_cover_fetch(bound_cards: Rc<RefCell<HashMap<String, gtk4::Box>>>, isbn: String, library: Library) {
    let (sender, receiver) =
        glib::MainContext::channel::<Option<std::path::PathBuf>>(glib::Priority::DEFAULT);
    let isbn_thread = isbn.clone();
    std::thread::spawn(move || {
        let path = fond_bib::acquire::fetch_isbn_cover(&isbn_thread)
            .ok()
            .flatten()
            .and_then(|bytes| library.store_cover(&isbn_thread, &bytes).ok());
        let _ = sender.send(path);
    });

    receiver.attach(None, move |path| {
        if let Some(path) = path {
            if let Some(card) = bound_cards.borrow().get(&isbn) {
                apply_cover(card, &path);
            }
        }
        glib::ControlFlow::Break
    });
}
