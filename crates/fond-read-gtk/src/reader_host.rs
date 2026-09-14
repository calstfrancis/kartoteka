//! Shared tab host for the PDF/EPUB readers.
//!
//! By default, "Read" opens a new tab in one shared `Reader` window instead of its own
//! top-level window: before the PDFium singleton fix (see `fond_doc::bind_pdfium`), a second
//! independent reader window was a guaranteed crash the moment either reader closed, and
//! even with that fixed, a window per open document doesn't scale past two or three. A tab
//! can still be popped out to its own standalone window via [`ReaderTab::pop_out`] (wired to
//! a headerbar button by each reader) — deliberately the *only* way to detach a tab, rather
//! than also handling `AdwTabView`'s free-form drag-out-of-the-bar gesture: that gesture
//! moves the page to a brand new `TabView` GTK builds on the fly, which this module has no
//! hook into, so a `ReaderTab` handle (and the `pdf_hash`/`hash`-keyed dedup registry in
//! `lib.rs` built on top of it) would go stale the moment a drag-out happened instead of
//! only on an explicit, fully-tracked `pop_out` call.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

/// GObject data key `on_tab_closed` stashes its closure under, on the `TabPage` itself. Read
/// back by the generic `close-page` handler `new_tab_view` connects once per host window.
const ON_CLOSE_DATA_KEY: &str = "fond-reader-on-close";

thread_local! {
    // The default shared reader window + its tab view, created lazily on the first reader
    // opened and cleared here as soon as it closes (its last tab closed, or the user closed
    // the window directly) — so the *next* "Read" after every reader was closed starts a
    // fresh window rather than trying to resurrect a dead one.
    static MAIN_HOST: RefCell<Option<(adw::Window, adw::TabView)>> = const { RefCell::new(None) };
}

/// A handle to one reader's tab: parent window for its own nested dialogs/popovers, and a
/// way to close or re-present just this tab without knowing whether it's living in the
/// shared host window or one popped out on its own.
#[derive(Clone)]
pub struct ReaderTab {
    pub host_window: adw::Window,
    tab_view: adw::TabView,
    page: adw::TabPage,
}

impl ReaderTab {
    /// Select this tab and raise its window.
    pub fn present(&self) {
        self.tab_view.set_selected_page(&self.page);
        self.host_window.present();
    }

    /// Close just this tab (confirms immediately — there is nothing here to confirm).
    pub fn close(&self) {
        self.tab_view.close_page(&self.page);
    }

    /// Move this tab out of its current window into a brand-new standalone one — the
    /// explicit counterpart to dragging a tab out of the bar. Returns the new handle; the
    /// old one still refers to the same page but is no longer useful for `present`/`close`
    /// once the transfer completes (the page's tab view has changed).
    pub fn pop_out(&self, parent: &adw::ApplicationWindow) -> ReaderTab {
        let (new_host, new_view) = new_tab_view(parent);
        self.tab_view.transfer_page(&self.page, &new_view, 0);
        new_host.present();
        ReaderTab {
            host_window: new_host,
            tab_view: new_view,
            page: self.page.clone(),
        }
    }
}

/// Build a reader host window: an `adw::Window` with a header bar, a tab bar, and an
/// `adw::TabView` filling the rest — used both for the shared default window and for every
/// popped-out one, so both look and behave identically.
fn new_tab_view(parent: &adw::ApplicationWindow) -> (adw::Window, adw::TabView) {
    let host = adw::Window::new();
    host.set_title(Some("Reader"));
    host.set_transient_for(Some(parent));
    host.set_default_size(1000, 820);

    let tab_view = adw::TabView::new();
    let tab_bar = adw::TabBar::new();
    tab_bar.set_view(Some(&tab_view));

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    toolbar.add_top_bar(&header);
    toolbar.add_top_bar(&tab_bar);
    toolbar.set_content(Some(&tab_view));
    host.set_content(Some(&toolbar));

    // No `create-window` handler: dragging a tab out of the bar is left as GTK's default
    // (refused, snaps back) rather than spawning a window — see the module doc for why.

    // The last tab leaving this view (closed, or transferred out via drag/`pop_out`) closes
    // the now-empty window instead of leaving a stray blank "Reader" window behind.
    {
        let host_weak = host.downgrade();
        tab_view.connect_page_detached(move |view, _page, _position| {
            if view.n_pages() == 0 {
                if let Some(host) = host_weak.upgrade() {
                    host.close();
                }
            }
        });
    }

    // One generic close confirmation for every page this view will ever hold, rather than
    // each reader connecting (and having to reconnect on every `pop_out`/drag-detach) its
    // own: `on_tab_closed` stashes its cleanup closure on the `TabPage` itself, which travels
    // with the page across `transfer_page` calls, so looking it up here keeps working no
    // matter which window/view the page has moved through since it was opened. Always
    // confirms immediately — there is never anything here worth blocking a close over.
    tab_view.connect_close_page(move |view, page| {
        unsafe {
            if let Some(on_close) = page.data::<Rc<dyn Fn()>>(ON_CLOSE_DATA_KEY) {
                (on_close.as_ref())();
            }
        }
        view.close_page_finish(page, true);
        glib::Propagation::Stop
    });

    // If this is (or was) the shared default host, forget it on close so the next reader
    // opened builds a fresh one rather than reusing a window that's going away. A no-op for
    // a popped-out window, which was never stored here.
    host.connect_close_request(move |window| {
        MAIN_HOST.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.as_ref().map(|(h, _)| h == window).unwrap_or(false) {
                *slot = None;
            }
        });
        glib::Propagation::Proceed
    });

    (host, tab_view)
}

/// Open `content` as a new tab in the shared reader window — creating it if this is the
/// first reader opened, or if the previous shared window has since closed — and select it.
/// `title` labels the tab. Does not present the window itself: callers still finish wiring
/// up the reader's own signal handlers afterward, exactly as when each reader was its own
/// top-level window, so presenting is left to the caller's own final `ReaderTab::present()`
/// once that's done.
pub fn open_reader_tab(
    parent: &adw::ApplicationWindow,
    title: &str,
    content: &impl IsA<gtk4::Widget>,
) -> ReaderTab {
    let (host_window, tab_view) = MAIN_HOST.with(|cell| {
        let mut slot = cell.borrow_mut();
        if let Some(existing) = slot.clone() {
            return existing;
        }
        let created = new_tab_view(parent);
        *slot = Some(created.clone());
        created
    });

    let page = tab_view.append(content);
    page.set_title(title);
    tab_view.set_selected_page(&page);

    ReaderTab {
        host_window,
        tab_view,
        page,
    }
}

/// Run `on_close` once, when this tab is closed for good — its own tab-close button,
/// keyboard shortcut, or its host window closing and cascading through its pages. Runs as
/// the close is confirmed, before anything is torn down, so it's safe to touch the tab's own
/// still-live widgets from `on_close` (e.g. reading a WebView's scroll position) — the same
/// timing a plain top-level reader window's `close-request` gave each reader before tabs
/// existed. See `new_tab_view`'s `close-page` handler for how this is actually invoked.
pub fn on_tab_closed(tab: &ReaderTab, on_close: impl Fn() + 'static) {
    let boxed: Rc<dyn Fn()> = Rc::new(on_close);
    unsafe {
        tab.page.set_data(ON_CLOSE_DATA_KEY, boxed);
    }
}
