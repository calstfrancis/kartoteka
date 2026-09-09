//! The main application window: a sidebar list of entries with a live filter, and a detail
//! pane showing the selected entry's YAML and note. All data comes from `fond-bib`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gtk4::prelude::*;
use gtk4::{gdk, gio, glib, Orientation};
use libadwaita as adw;
use libadwaita::prelude::*;
use webkit6::prelude::*;

use fond_bib::{entry as bibentry, Library};

use crate::config::Config;
use crate::ui::reader::annotations::show_annotations_dialog;
use crate::ui::reader::epub::show_epub_reader;
use crate::ui::reader::pdf::show_pdf_reader;
use crate::ui::reader::ReaderHost;
use crate::ui::{bookshelf, friendly, worker};
use crate::{github, secret_store, webdav};

/// Which kind of identifier the acquire dialog is looking up.
#[derive(Clone, Copy)]
enum AcquireKind {
    Doi,
    Arxiv,
    Isbn,
}

/// A compact, display-ready summary of one entry.
struct EntrySummary {
    key: String,
    author: String,
    year: String,
    title: String,
    /// Whether a readable (present-on-disk) PDF/EPUB attachment exists, for the list row's
    /// availability icon — same detection `show_detail` uses for its own Read button, computed
    /// once at load time rather than re-reading each entry's note on every list render.
    has_pdf: bool,
    has_epub: bool,
    /// Comma-joined, for the optional Tags spreadsheet column.
    tags: String,
    /// `""`/"unread"/"reading"/"read", for the optional Status spreadsheet column.
    status: String,
    /// This entry's own custom-field values (§ custom fields), for optional per-field
    /// spreadsheet columns — same values `show_detail`'s custom field rows show, just also
    /// available at list-row granularity without a per-entry note re-read.
    custom_fields: HashMap<String, String>,
    /// Hayagriva entry type, lowercased (e.g. "book", "article") — used by the Bookshelf
    /// view's book-only filter.
    entry_type: String,
    /// ISBN as authored on the entry, or empty if none — used to look up/fetch a cached
    /// cover for the Bookshelf view.
    isbn: String,
}

#[derive(Default)]
pub(crate) struct AppState {
    pub(crate) library: Option<Library>,
    entries: Vec<EntrySummary>,
    /// Indices into `entries` matching the current filter, in display order.
    visible: Vec<usize>,
    query: String,
    /// Full-text index over the current library (rebuilt on open); `None` if unavailable.
    index: Option<fond_index::SearchIndex>,
    key_to_index: HashMap<String, usize>,
    /// Collection slugs, in display order (mirrors the collections list).
    collections: Vec<String>,
    /// Active collection filter (slug), or `None` for "All entries".
    collection_filter: Option<String>,
    /// Saved searches (name → query), loaded from config.
    saved_searches: Vec<(String, String)>,
    /// Whether the spreadsheet's checkbox column and bulk-action bar are showing — see
    /// `show_bulk_bar`/the "Select" header toggle.
    bulk_mode: bool,
    /// Keys checked in bulk-select mode. Cleared on entering/leaving bulk mode and on every
    /// bulk action's completion, but *not* on an ordinary list refresh — an edit elsewhere
    /// shouldn't silently drop an in-progress bulk selection.
    bulk_selected: HashSet<String>,
}

struct Widgets {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    subtitle: adw::WindowTitle,
    status_label: gtk4::Label,
    /// Backing store for the entries spreadsheet, in `AppState.visible` order — cleared and
    /// refilled by `refresh_list`, then re-sorted/selected live by `column_view`/`selection`.
    store: gio::ListStore,
    column_view: gtk4::ColumnView,
    selection: gtk4::SingleSelection,
    detail: gtk4::Box,
    collections_listbox: gtk4::ListBox,
    search: gtk4::SearchEntry,
    /// Switches between the first-run "no library open" status page and the actual
    /// three-pane library view — see `open_library`.
    content_stack: gtk4::Stack,
    config: Rc<RefCell<Config>>,
    /// Per-library custom-field spreadsheet columns currently in `column_view`, kept so
    /// `sync_custom_field_columns` can remove the previous library's set before adding the
    /// new one. See `open_library`.
    custom_columns: Rc<RefCell<Vec<gtk4::ColumnViewColumn>>>,
}

/// A `glib::Object` wrapper around one `EntrySummary`, for use as a `gio::ListStore` row in
/// the entries `ColumnView` — GTK's list widgets bind to `glib::Object` items, not plain Rust
/// structs. `idx` is the entry's stable position in `AppState.entries`; unlike the row's
/// position in the (sortable, filterable) `ColumnView` model, it never changes underneath an
/// open detail view.
pub(crate) mod entry_row {
    use super::EntrySummary;
    use glib::subclass::types::ObjectSubclassIsExt;

    glib::wrapper! {
        pub struct EntryRow(ObjectSubclass<imp::EntryRow>);
    }

    impl EntryRow {
        pub(super) fn new(idx: usize, e: &EntrySummary) -> Self {
            let obj: Self = glib::Object::new();
            let imp = obj.imp();
            imp.idx.set(idx);
            *imp.key.borrow_mut() = e.key.clone();
            *imp.title.borrow_mut() = e.title.clone();
            *imp.author.borrow_mut() = e.author.clone();
            *imp.year.borrow_mut() = e.year.clone();
            imp.has_pdf.set(e.has_pdf);
            imp.has_epub.set(e.has_epub);
            *imp.tags.borrow_mut() = e.tags.clone();
            *imp.status.borrow_mut() = e.status.clone();
            *imp.custom_fields.borrow_mut() = e.custom_fields.clone();
            *imp.entry_type.borrow_mut() = e.entry_type.clone();
            *imp.isbn.borrow_mut() = e.isbn.clone();
            obj
        }

        pub fn idx(&self) -> usize {
            self.imp().idx.get()
        }
        pub fn key(&self) -> String {
            self.imp().key.borrow().clone()
        }
        pub fn title(&self) -> String {
            self.imp().title.borrow().clone()
        }
        pub fn author(&self) -> String {
            self.imp().author.borrow().clone()
        }
        pub fn year(&self) -> String {
            self.imp().year.borrow().clone()
        }
        pub fn has_pdf(&self) -> bool {
            self.imp().has_pdf.get()
        }
        pub fn has_epub(&self) -> bool {
            self.imp().has_epub.get()
        }
        pub fn tags(&self) -> String {
            self.imp().tags.borrow().clone()
        }
        pub fn status(&self) -> String {
            self.imp().status.borrow().clone()
        }
        pub fn custom_field(&self, name: &str) -> String {
            self.imp()
                .custom_fields
                .borrow()
                .get(name)
                .cloned()
                .unwrap_or_default()
        }
        pub fn entry_type(&self) -> String {
            self.imp().entry_type.borrow().clone()
        }
        pub fn isbn(&self) -> String {
            self.imp().isbn.borrow().clone()
        }
        /// Update the cached display fields after a save, so the row reflects the edit
        /// immediately without waiting for the next full list rebuild.
        pub fn set_display(&self, title: String, author: String, year: String) {
            *self.imp().title.borrow_mut() = title;
            *self.imp().author.borrow_mut() = author;
            *self.imp().year.borrow_mut() = year;
        }
    }

    mod imp {
        use super::super::HashMap;
        use std::cell::{Cell, RefCell};

        #[derive(Default)]
        pub struct EntryRow {
            pub idx: Cell<usize>,
            pub key: RefCell<String>,
            pub title: RefCell<String>,
            pub author: RefCell<String>,
            pub year: RefCell<String>,
            pub has_pdf: Cell<bool>,
            pub has_epub: Cell<bool>,
            pub tags: RefCell<String>,
            pub status: RefCell<String>,
            pub custom_fields: RefCell<HashMap<String, String>>,
            pub entry_type: RefCell<String>,
            pub isbn: RefCell<String>,
        }

        #[glib::object_subclass]
        impl glib::subclass::types::ObjectSubclass for EntryRow {
            const NAME: &'static str = "KartotekaEntryRow";
            type Type = super::EntryRow;
        }

        impl glib::subclass::object::ObjectImpl for EntryRow {}
    }
}
use entry_row::EntryRow;

pub fn build(app: &adw::Application, config: Config) -> adw::ApplicationWindow {
    let state = Rc::new(RefCell::new(AppState::default()));
    let config = Rc::new(RefCell::new(config));
    // Declared early so the hamburger popover's recent-libraries quick-switcher (built
    // below, before `Widgets` exists) can populate it — see the fuller comment at its
    // original declaration site further down, near `build_entries_column_view`.
    let widgets_slot: Rc<RefCell<Option<Rc<Widgets>>>> = Rc::new(RefCell::new(None));

    // Window size and pane positions are restored from last session below (the "internal
    // window sizing remembered across sessions" that, along with the column/pane layout,
    // makes the app pick up where you left it rather than resetting every launch).
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Kartoteka")
        .default_width(config.borrow().window_width.unwrap_or(1300))
        .default_height(config.borrow().window_height.unwrap_or(680))
        .build();
    if config.borrow().window_maximized.unwrap_or(false) {
        window.maximize();
    }

    // Debounced config save, shared by every "remember this across sessions" signal below
    // (window size/maximized, both pane positions) — one shared timer so a flurry of resize
    // events while dragging a divider collapses into a single write ~400ms after it stops,
    // matching the debounce-and-guard idiom CLAUDE.md's UI standard calls for.
    let config_save_timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let schedule_config_save = {
        let config = config.clone();
        let timer = config_save_timer.clone();
        Rc::new(move || {
            if let Some(id) = timer.borrow_mut().take() {
                id.remove();
            }
            let config = config.clone();
            let timer_for_clear = timer.clone();
            let id = glib::timeout_add_local(Duration::from_millis(400), move || {
                config.borrow().save();
                *timer_for_clear.borrow_mut() = None;
                glib::ControlFlow::Break
            });
            *timer.borrow_mut() = Some(id);
        })
    };
    {
        let config = config.clone();
        let schedule_config_save = schedule_config_save.clone();
        window.connect_default_width_notify(move |w| {
            config.borrow_mut().window_width = Some(w.default_width());
            schedule_config_save();
        });
    }
    {
        let config = config.clone();
        let schedule_config_save = schedule_config_save.clone();
        window.connect_default_height_notify(move |w| {
            config.borrow_mut().window_height = Some(w.default_height());
            schedule_config_save();
        });
    }
    {
        let config = config.clone();
        let schedule_config_save = schedule_config_save.clone();
        window.connect_maximized_notify(move |w| {
            config.borrow_mut().window_maximized = Some(w.is_maximized());
            schedule_config_save();
        });
    }

    let toolbar_view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");

    let title = adw::WindowTitle::new("Kartoteka", "no library open");
    header.set_title_widget(Some(&title));

    let open_button = gtk4::Button::from_icon_name("folder-open-symbolic");
    open_button.set_tooltip_text(Some("Open library…"));
    header.pack_start(&open_button);

    let add_button = gtk4::Button::from_icon_name("list-add-symbolic");
    add_button.set_tooltip_text(Some("Acquire a reference…"));
    header.pack_start(&add_button);

    let menu_button = gtk4::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main menu")
        .build();
    menu_button.set_popover(Some(&build_hamburger_popover(
        &config,
        &state,
        &widgets_slot,
    )));
    header.pack_end(&menu_button);

    let reload_button = gtk4::Button::from_icon_name("view-refresh-symbolic");
    reload_button.set_tooltip_text(Some("Reload library"));
    header.pack_end(&reload_button);

    // Bulk-select toggle: turns on the spreadsheet's checkbox column and the bulk-action bar
    // below it (see `bulk_bar`) — for tagging, collecting, or deleting several entries at
    // once instead of one at a time in the detail pane.
    let bulk_toggle = gtk4::ToggleButton::builder()
        .icon_name("object-select-symbolic")
        .tooltip_text("Select multiple entries")
        .build();
    header.pack_end(&bulk_toggle);

    // Bookshelf toggle: swaps the entries pane between the spreadsheet and a cover-grid
    // view of just the book entries (see `ui::bookshelf`). Packed right after `bulk_toggle`
    // so the two entries-display-mode controls sit next to each other — `pack_end` calls
    // apply in reverse visual order, so this ends up immediately left of it.
    let bookshelf_toggle = gtk4::ToggleButton::builder()
        .icon_name("view-grid-symbolic")
        .tooltip_text("Bookshelf view — cover grid of books")
        .build();
    header.pack_end(&bookshelf_toggle);

    toolbar_view.add_top_bar(&header);

    // Collections pane (leftmost): "All entries" + one row per collection, with a + to
    // create a new one.
    let collections_box = gtk4::Box::new(Orientation::Vertical, 0);
    collections_box.set_width_request(190);
    collections_box.add_css_class("fond-sidebar");
    let coll_header = gtk4::Box::new(Orientation::Horizontal, 4);
    coll_header.set_margin_top(6);
    coll_header.set_margin_bottom(2);
    coll_header.set_margin_start(10);
    coll_header.set_margin_end(6);
    let coll_title = gtk4::Label::new(Some("Collections"));
    coll_title.add_css_class("dim-label");
    coll_title.add_css_class("caption-heading");
    coll_title.set_hexpand(true);
    coll_title.set_xalign(0.0);
    let coll_add = gtk4::Button::from_icon_name("list-add-symbolic");
    coll_add.add_css_class("flat");
    coll_add.set_tooltip_text(Some("New collection"));
    coll_header.append(&coll_title);
    coll_header.append(&coll_add);
    let collections_listbox = gtk4::ListBox::new();
    collections_listbox.add_css_class("fond-list");
    let coll_scroll = gtk4::ScrolledWindow::new();
    coll_scroll.set_child(Some(&collections_listbox));
    coll_scroll.set_vexpand(true);
    collections_box.append(&coll_header);
    collections_box.append(&coll_scroll);

    // Sidebar: search entry over a scrolled list.
    let sidebar = gtk4::Box::new(Orientation::Vertical, 0);
    sidebar.set_width_request(300);
    sidebar.add_css_class("fond-ground");
    let search = gtk4::SearchEntry::new();
    search.add_css_class("fond-search");
    search.set_placeholder_text(Some("Search your library"));
    search.set_tooltip_text(Some(
        "Searches titles, authors, and keys by default. Narrow it down with author:, title:, \
         tag:, type:, or year: — e.g. author:berdyaev year:1937",
    ));
    search.set_margin_top(6);
    search.set_margin_bottom(6);
    search.set_margin_start(6);
    search.set_margin_end(6);

    // Entries spreadsheet: a sortable, in-place-editable `ColumnView` in place
    // of the old card list — see `build_entries_column_view`. The factories it wires up need
    // `Widgets` (for toasts/reload on a committed edit), which doesn't exist until after this
    // block — `widgets_slot` (declared above, near `state`) is filled in once it does; edits
    // can't happen before the window is shown, so it's always populated by the time a
    // factory closure runs.
    let (column_view, store, selection, sort_model) = build_entries_column_view();
    apply_column_visibility(&column_view, &config.borrow());
    let column_order = config.borrow().column_order.clone();
    reorder_columns(&column_view, &column_order);
    // Column order changes (drag-to-reorder) show up as `items-changed` on the columns
    // model — the same debounced-save timer as window size/pane position, so a drag that
    // passes through several intermediate positions collapses into one write.
    {
        let config = config.clone();
        let schedule_config_save = schedule_config_save.clone();
        column_view
            .columns()
            .connect_items_changed(move |columns, _, _, _| {
                let order: Vec<String> = (0..columns.n_items())
                    .filter_map(|i| {
                        columns
                            .item(i)
                            .and_downcast::<gtk4::ColumnViewColumn>()
                            .and_then(|c| c.id().map(|id| id.to_string()))
                    })
                    .collect();
                config.borrow_mut().column_order = order;
                schedule_config_save();
            });
    }
    let custom_columns: Rc<RefCell<Vec<gtk4::ColumnViewColumn>>> =
        Rc::new(RefCell::new(Vec::new()));

    // Bulk-action bar: hidden until the header's "Select multiple" toggle turns it (and the
    // checkbox column) on. The three buttons are wired once `widgets` exists, further down.
    let bulk_bar = gtk4::Box::new(Orientation::Horizontal, 8);
    bulk_bar.add_css_class("toolbar");
    bulk_bar.set_visible(false);
    let bulk_count_label = gtk4::Label::new(Some("0 selected"));
    bulk_count_label.add_css_class("dim-label");
    bulk_count_label.set_hexpand(true);
    bulk_count_label.set_xalign(0.0);
    let bulk_tag_button = gtk4::Button::with_label("Add tag…");
    let bulk_collection_button = gtk4::Button::with_label("Add to collection…");
    let bulk_delete_button = gtk4::Button::with_label("Delete");
    bulk_delete_button.add_css_class("destructive-action");
    bulk_bar.append(&bulk_count_label);
    bulk_bar.append(&bulk_tag_button);
    bulk_bar.append(&bulk_collection_button);
    bulk_bar.append(&bulk_delete_button);

    let on_bulk_change: Rc<dyn Fn()> = {
        let state = state.clone();
        let bulk_count_label = bulk_count_label.clone();
        Rc::new(move || {
            let n = state.borrow().bulk_selected.len();
            bulk_count_label.set_text(&format!("{n} selected"));
        })
    };
    let select_column = add_bulk_select_column(&column_view, &state, on_bulk_change.clone());
    select_column.set_visible(false);

    let list_scroll = gtk4::ScrolledWindow::new();
    list_scroll.set_child(Some(&column_view));
    list_scroll.set_vexpand(true);

    let bookshelf_view = bookshelf::build_bookshelf_view(&sort_model, state.clone());

    // View-mode stack: swaps the entries pane between the spreadsheet and the Bookshelf
    // cover grid (see the `bookshelf_toggle` headerbar button below). Both are built
    // eagerly — constructing the `GridView` is cheap, and cover fetches only fire for
    // cells actually scrolled into view, so there's no eager-cost concern in building it
    // up front rather than lazily on first toggle (unlike the PDF reader's continuous view).
    let view_mode_stack = gtk4::Stack::new();
    view_mode_stack.add_named(&list_scroll, Some("list"));
    view_mode_stack.add_named(&bookshelf_view.scroller, Some("grid"));
    view_mode_stack.set_visible_child_name(if config.borrow().bookshelf_view {
        "grid"
    } else {
        "list"
    });
    view_mode_stack.set_vexpand(true);
    bookshelf_toggle.set_active(config.borrow().bookshelf_view);

    sidebar.append(&search);
    sidebar.append(&bulk_bar);
    sidebar.append(&view_mode_stack);

    // Detail pane: a vertical box of field rows, rebuilt on selection.
    let detail = gtk4::Box::new(Orientation::Vertical, 10);
    detail.set_margin_top(18);
    detail.set_margin_bottom(18);
    detail.set_margin_start(18);
    detail.set_margin_end(18);
    let detail_scroll = gtk4::ScrolledWindow::new();
    detail_scroll.set_child(Some(&detail));
    detail_scroll.set_hexpand(true);
    detail_scroll.set_vexpand(true);
    detail_scroll.add_css_class("fond-view");

    let inner_paned = gtk4::Paned::new(Orientation::Horizontal);
    inner_paned.set_start_child(Some(&sidebar));
    inner_paned.set_end_child(Some(&detail_scroll));
    inner_paned.set_resize_start_child(true);
    inner_paned.set_resize_end_child(false);
    // Wide enough on open that the spreadsheet's Key/Title/Author/Year/Files columns are all
    // comfortably visible without immediately having to drag the divider — the detail card
    // only needs to show one entry's fields, not compete with the list for space. Restored
    // from last session if this isn't a first run.
    inner_paned.set_position(config.borrow().detail_pane_position.unwrap_or(780));

    let paned = gtk4::Paned::new(Orientation::Horizontal);
    paned.set_start_child(Some(&collections_box));
    paned.set_end_child(Some(&inner_paned));
    paned.set_resize_start_child(false);
    paned.set_position(config.borrow().collections_pane_position.unwrap_or(190));

    {
        let config = config.clone();
        let schedule_config_save = schedule_config_save.clone();
        paned.connect_position_notify(move |p| {
            config.borrow_mut().collections_pane_position = Some(p.position());
            schedule_config_save();
        });
    }
    {
        let config = config.clone();
        let schedule_config_save = schedule_config_save.clone();
        inner_paned.connect_position_notify(move |p| {
            config.borrow_mut().detail_pane_position = Some(p.position());
            schedule_config_save();
        });
    }

    // First-run / no-library state: a friendly status page instead of a blank three-pane
    // window, shown until a library is open — `content_stack` switches to "library" the
    // first time `open_library` succeeds (including on a restored last-opened path).
    let empty_status = build_no_library_status_page(&state, &config, &widgets_slot);
    let content_stack = gtk4::Stack::new();
    content_stack.add_named(&empty_status, Some("empty"));
    content_stack.add_named(&paned, Some("library"));
    content_stack.set_visible_child_name("empty");
    toolbar_view.set_content(Some(&content_stack));

    // Status bar (house style): a status message on the left, a version → changelog
    // button on the right.
    let statusbar = gtk4::Box::new(Orientation::Horizontal, 6);
    statusbar.add_css_class("toolbar");
    statusbar.add_css_class("fond-chrome");
    statusbar.add_css_class("fond-statusbar");
    let status_label = gtk4::Label::new(Some("No library open"));
    status_label.add_css_class("dim-label");
    status_label.set_halign(gtk4::Align::Start);
    status_label.set_xalign(0.0);
    status_label.set_hexpand(true);
    status_label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
    let version_button = gtk4::Button::builder()
        .label(concat!("v", env!("CARGO_PKG_VERSION")))
        .tooltip_text("View changelog")
        .build();
    version_button.add_css_class("flat");
    version_button.add_css_class("caption");
    statusbar.append(&status_label);
    statusbar.append(&version_button);
    toolbar_view.add_bottom_bar(&statusbar);

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&toolbar_view));
    window.set_content(Some(&toasts));

    {
        let window = window.clone();
        version_button.connect_clicked(move |_| show_changelog(&window));
    }

    let widgets = Rc::new(Widgets {
        window: window.clone(),
        toasts,
        subtitle: title,
        status_label,
        store: store.clone(),
        column_view: column_view.clone(),
        selection: selection.clone(),
        detail,
        collections_listbox: collections_listbox.clone(),
        search: search.clone(),
        content_stack: content_stack.clone(),
        config: config.clone(),
        custom_columns: custom_columns.clone(),
    });
    *widgets_slot.borrow_mut() = Some(widgets.clone());

    // Collection selection → set the filter and refresh the list. Resolved from data
    // attached to the row itself (`refresh_collections`/`collection_row`) rather than its
    // index — the tree layout means a collection's position no longer maps to a stable
    // offset into `state.collections`/`saved_searches` the way a flat list's did.
    {
        let state = state.clone();
        let widgets = widgets.clone();
        collections_listbox.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            if let Some(slug) = (unsafe { row.data::<String>("collection-slug") })
                .map(|p| unsafe { p.as_ref() }.clone())
            {
                state.borrow_mut().collection_filter = Some(slug);
                widgets.search.set_text("");
                refresh_list(&state, &widgets);
            } else if let Some(name) = (unsafe { row.data::<String>("saved-search-name") })
                .map(|p| unsafe { p.as_ref() }.clone())
            {
                let query = state
                    .borrow()
                    .saved_searches
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, q)| q.clone());
                state.borrow_mut().collection_filter = None;
                if let Some(query) = query {
                    widgets.search.set_text(&query); // triggers refresh via search_changed
                }
            } else {
                // "All entries" — the only row with neither tag.
                state.borrow_mut().collection_filter = None;
                widgets.search.set_text("");
                refresh_list(&state, &widgets);
            }
        });
    }
    // New collection.
    {
        let state = state.clone();
        let widgets = widgets.clone();
        coll_add.connect_clicked(move |_| new_collection_dialog(&state, &widgets));
    }

    // --- wiring ---

    // Row selection → show detail. The selected item's `idx` is its stable position in
    // `AppState.entries`, independent of the column sorter's current order.
    {
        let state = state.clone();
        let widgets = widgets.clone();
        selection.connect_selected_notify(move |sel| {
            if let Some(row) = sel.selected_item().and_downcast::<EntryRow>() {
                show_detail(&state, &widgets, row.idx());
            }
        });
    }

    // Right-click a spreadsheet row for a context menu (Collections…/Create book
    // part…/Delete…) — see `show_entry_context_menu`. `ColumnView` has no per-row widget to
    // attach to directly, so the gesture lives on the view itself and resolves which row was
    // clicked via `pick()` plus the "row-key" qdata each column's cell factory stashes.
    {
        let state = state.clone();
        let widgets_for_click = widgets.clone();
        let column_view_for_pick = widgets.column_view.clone();
        let click = gtk4::GestureClick::new();
        click.set_button(gdk::BUTTON_SECONDARY);
        click.connect_pressed(move |_gesture, _n, x, y| {
            let Some(picked) = column_view_for_pick.pick(x, y, gtk4::PickFlags::DEFAULT) else {
                return;
            };
            let Some(key) = row_key_at(picked) else {
                return;
            };
            show_entry_context_menu(
                &state,
                &widgets_for_click,
                &column_view_for_pick,
                &key,
                x,
                y,
            );
        });
        widgets.column_view.add_controller(click);
    }

    // Live filter.
    {
        let state = state.clone();
        let widgets = widgets.clone();
        search.connect_search_changed(move |entry| {
            state.borrow_mut().query = entry.text().to_string();
            refresh_list(&state, &widgets);
        });
    }
    // Escape in the search field clears it and returns focus to the list — `SearchEntry`
    // fires `stop-search` on Escape but doesn't act on it itself; a search with no way to
    // back out of via the keyboard is a real keyboard-navigation gap, not just a nicety.
    {
        let widgets = widgets.clone();
        search.connect_stop_search(move |entry| {
            entry.set_text("");
            widgets.column_view.grab_focus();
        });
    }

    // Reload.
    {
        let state = state.clone();
        let widgets = widgets.clone();
        reload_button.connect_clicked(move |_| {
            let path = state
                .borrow()
                .library
                .as_ref()
                .map(|l| l.root().to_path_buf());
            if let Some(path) = path {
                open_library(&state, &widgets, path);
            } else {
                toast(&widgets, "No library open");
            }
        });
    }

    // Open.
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let config = config.clone();
        open_button.connect_clicked(move |_| {
            open_library_picker(&state, &widgets, &config);
        });
    }

    // Bulk-select mode: show/hide the checkbox column and action bar, and clear whatever was
    // checked when leaving it — a stale selection from a previous session in the bar would be
    // confusing, and re-entering should start from a clean slate.
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let bulk_bar = bulk_bar.clone();
        let select_column = select_column.clone();
        let on_bulk_change = on_bulk_change.clone();
        bulk_toggle.connect_toggled(move |b| {
            let on = b.is_active();
            state.borrow_mut().bulk_mode = on;
            if !on {
                state.borrow_mut().bulk_selected.clear();
            }
            select_column.set_visible(on);
            bulk_bar.set_visible(on);
            // Force the (recycled) checkbox cells to re-bind against the now-cleared/still
            // showing state, since GTK only rebinds on an actual model change.
            let n = widgets.store.n_items();
            widgets.store.items_changed(0, n, n);
            on_bulk_change();
        });
    }

    // Bookshelf toggle: swaps the entries pane between the spreadsheet and the cover grid.
    // Note bulk-select mode only affects the spreadsheet (its checkbox column) — toggling
    // to Bookshelf while it's on just leaves that column's state inert, not shown.
    {
        let config = config.clone();
        let view_mode_stack = view_mode_stack.clone();
        bookshelf_toggle.connect_toggled(move |b| {
            view_mode_stack.set_visible_child_name(if b.is_active() { "grid" } else { "list" });
            config.borrow_mut().bookshelf_view = b.is_active();
            config.borrow().save();
        });
    }

    // Bookshelf grid selection → show detail, mirroring the spreadsheet's own wiring above.
    // `idx()` is stable across both the full-sorted and book-filtered models, so
    // `show_detail` needs no changes to serve either view.
    {
        let state = state.clone();
        let widgets = widgets.clone();
        bookshelf_view
            .selection
            .connect_selected_notify(move |sel| {
                if let Some(row) = sel.selected_item().and_downcast::<EntryRow>() {
                    show_detail(&state, &widgets, row.idx());
                }
            });
    }

    // Right-click a book cover for the same context menu the spreadsheet offers (see
    // `show_entry_context_menu`) — the Bookshelf grid previously had no right-click at all.
    // `GridView`, like `ColumnView`, has no per-row widget to attach a gesture to directly,
    // so this resolves the clicked card via `pick()` + the "row-key" qdata `bind_card`
    // stashes on every card (the same idiom `row_key_at` already uses for the spreadsheet).
    {
        let state = state.clone();
        let widgets_for_click = widgets.clone();
        let scroller_for_pick = bookshelf_view.scroller.clone();
        let click = gtk4::GestureClick::new();
        click.set_button(gdk::BUTTON_SECONDARY);
        click.connect_pressed(move |_gesture, _n, x, y| {
            let Some(picked) = scroller_for_pick.pick(x, y, gtk4::PickFlags::DEFAULT) else {
                return;
            };
            let Some(key) = row_key_at(picked) else {
                return;
            };
            show_entry_context_menu(&state, &widgets_for_click, &scroller_for_pick, &key, x, y);
        });
        bookshelf_view.scroller.add_controller(click);
    }

    // Bulk actions: add a tag, add to a collection, or delete — applied to every currently
    // checked key. All three clear the bulk selection and reload on completion.
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let on_bulk_change = on_bulk_change.clone();
        bulk_tag_button.connect_clicked(move |b| {
            show_bulk_tag_popover(&state, &widgets, b.upcast_ref(), &on_bulk_change);
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let on_bulk_change = on_bulk_change.clone();
        bulk_collection_button.connect_clicked(move |b| {
            show_bulk_collection_popover(&state, &widgets, b.upcast_ref(), &on_bulk_change);
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let on_bulk_change = on_bulk_change.clone();
        bulk_delete_button.connect_clicked(move |_| {
            confirm_bulk_delete(&state, &widgets, &on_bulk_change);
        });
    }

    // Acquire button opens the dialog.
    {
        let state = state.clone();
        let widgets = widgets.clone();
        add_button.connect_clicked(move |_| show_acquire_dialog(&state, &widgets));
    }

    // Drag a PDF onto the window to add it.
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let drop = gtk4::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        drop.connect_drop(move |_, value, _, _| {
            let Ok(files) = value.get::<gdk::FileList>() else {
                // The drag source didn't offer anything GTK could convert to a file list
                // (e.g. it only provided plain text, or a non-local URI) — previously this
                // failed completely silently, which looked identical to the drop just not
                // registering at all.
                toast(
                    &widgets,
                    "Couldn't read the dropped file — try dragging from a file manager",
                );
                return false;
            };
            if state.borrow().library.is_none() {
                toast(&widgets, "Open a library first");
                return false;
            }
            let mut handled = false;
            let mut skipped_remote = false;
            for file in files.files() {
                let Some(path) = file.path() else {
                    // No local path — a remote/GVfs URI (e.g. dragged from a network share
                    // or a browser download that hasn't materialized locally). Previously
                    // silently ignored; note it instead of doing nothing.
                    skipped_remote = true;
                    continue;
                };
                if path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("pdf"))
                    == Some(true)
                {
                    import_pdf(&state, &widgets, path);
                    handled = true;
                } else {
                    toast(&widgets, "Only PDF files can be dropped");
                }
            }
            if skipped_remote && !handled {
                toast(
                    &widgets,
                    "Only local files can be dropped — try opening it first",
                );
            }
            handled
        });
        window.add_controller(drop);
    }

    // Hamburger actions (win.acquire / win.reindex / win.theme / win.about).
    let auto_backup_timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    add_window_actions(&window, &state, &widgets, &config, &auto_backup_timer);

    // Apply the saved colour scheme.
    apply_theme(
        &config
            .borrow()
            .theme
            .clone()
            .unwrap_or_else(|| "system".to_string()),
    );

    // Restore the last-opened library. The `.clone()` is bound to `last_library` first,
    // rather than matched directly in the `if let`'s scrutinee, so the temporary `Ref`
    // `config.borrow()` produces is dropped immediately instead of being kept alive for the
    // whole `if let` block (a real, pre-existing bug: `open_library` below can synchronously
    // trigger a column-reorder signal whose handler does `config.borrow_mut()` — with the
    // scrutinee-bound `Ref` still alive across that call, that panics with "already
    // borrowed").
    let last_library = config.borrow().library_path.clone();
    if let Some(path) = last_library {
        if path.is_dir() {
            open_library(&state, &widgets, path);
        }
    }

    // Start the automatic-backup timer if it was left on from a previous session. The
    // ticking closure re-checks `state.library` each time, so a single timer started here
    // (rather than one per library open/close) covers the whole window lifetime.
    start_auto_backup_timer(&state, &widgets, &config, &auto_backup_timer);

    window
}

/// The first-run / no-library-open page: a plain-language welcome instead of a blank
/// three-pane window, with the two ways to get started front and centre. Its buttons need
/// `Widgets` (for toasts), which doesn't exist yet when this is built — same `widgets_slot`
/// deferred-lookup pattern as `build_entries_column_view`.
fn build_no_library_status_page(
    state: &Rc<RefCell<AppState>>,
    config: &Rc<RefCell<Config>>,
    widgets_slot: &Rc<RefCell<Option<Rc<Widgets>>>>,
) -> adw::StatusPage {
    let page = adw::StatusPage::new();
    page.set_icon_name(Some("folder-symbolic"));
    page.set_title("Welcome to Kartoteka");
    page.set_description(Some(
        "A library is just a folder that holds your references, notes, and PDFs together. \
         Create a new one to get started, or open one you already have.",
    ));

    let buttons = gtk4::Box::new(Orientation::Horizontal, 8);
    buttons.set_halign(gtk4::Align::Center);
    let new_lib = gtk4::Button::with_label("New library…");
    new_lib.add_css_class("suggested-action");
    new_lib.add_css_class("pill");
    let open_lib = gtk4::Button::with_label("Open existing library…");
    open_lib.add_css_class("pill");
    buttons.append(&new_lib);
    buttons.append(&open_lib);
    page.set_child(Some(&buttons));

    {
        let state = state.clone();
        let widgets_slot = widgets_slot.clone();
        let config = config.clone();
        new_lib.connect_clicked(move |_| {
            let Some(widgets) = widgets_slot.borrow().clone() else {
                return;
            };
            show_new_library_dialog(&state, &widgets, &config);
        });
    }
    {
        let state = state.clone();
        let widgets_slot = widgets_slot.clone();
        let config = config.clone();
        open_lib.connect_clicked(move |_| {
            let Some(widgets) = widgets_slot.borrow().clone() else {
                return;
            };
            open_library_picker(&state, &widgets, &config);
        });
    }

    page
}

/// The main hamburger menu: a hand-built popover (house style — see `popover_button`)
/// rather than a `gio::Menu` model. With well over a dozen actions, a flat menu model read
/// as one undifferentiated wall of text; grouped rows with visible section breaks scan far
/// better, and this is the pattern CLAUDE.md's UI standard calls for once a hamburger has
/// "more than a handful" of actions (Zerkalo's is the reference). Every row still triggers
/// the same `win.*` `GAction`s `add_window_actions` registers — only the presentation
/// changed — except the theme rows, which are built directly so they can also update their
/// own bold/not-bold state on click (the house-style "name-as-label" toggle idiom, used here
/// in place of a nested Theme submenu).
fn build_hamburger_popover(
    config: &Rc<RefCell<Config>>,
    state: &Rc<RefCell<AppState>>,
    widgets_slot: &Rc<RefCell<Option<Rc<Widgets>>>>,
) -> gtk4::Popover {
    let (popover, rows) = popover_menu(230);

    let activate_row = |rows: &gtk4::Box, popover: &gtk4::Popover, label: &str, action: &str| {
        let row = popover_button(label, false);
        let popover = popover.clone();
        let action = action.to_string();
        row.connect_clicked(move |b| {
            popover.popdown();
            let _ = b.activate_action(&action, None);
        });
        rows.append(&row);
        row
    };

    activate_row(&rows, &popover, "New library…", "win.new-library");
    activate_row(&rows, &popover, "Open library…", "win.open-library");
    activate_row(&rows, &popover, "Move library…", "win.move-library").set_tooltip_text(Some(
        "Relocate the current library's folder — e.g. onto a different drive",
    ));

    // Quick-switcher: recently opened libraries, most-recent-first, excluding whichever is
    // open right now. Rebuilt every time the popover opens (via `connect_show` below) rather
    // than once at startup, since the recent list and the currently-open library both change
    // over the session (M4 Tier 4 — see `docs/M4-SPEC.md`).
    let recent_box = gtk4::Box::new(Orientation::Vertical, 2);
    rows.append(&recent_box);
    let refresh_recent: Rc<dyn Fn()> = {
        let recent_box = recent_box.clone();
        let popover = popover.clone();
        let config = config.clone();
        let state = state.clone();
        let widgets_slot = widgets_slot.clone();
        Rc::new(move || {
            while let Some(child) = recent_box.first_child() {
                recent_box.remove(&child);
            }
            let current = state
                .borrow()
                .library
                .as_ref()
                .map(|l| l.root().to_path_buf());
            let recents: Vec<PathBuf> = config
                .borrow()
                .recent_libraries
                .iter()
                .filter(|p| Some((*p).clone()) != current)
                .cloned()
                .collect();
            for path in recents {
                let label = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("library");
                let row = popover_button(&format!("Switch to \"{label}\"…"), false);
                row.set_tooltip_text(Some(&path.display().to_string()));
                let popover = popover.clone();
                let state = state.clone();
                let widgets_slot = widgets_slot.clone();
                let config = config.clone();
                let path = path.clone();
                row.connect_clicked(move |_| {
                    popover.popdown();
                    let Some(widgets) = widgets_slot.borrow().clone() else {
                        return;
                    };
                    config.borrow_mut().library_path = Some(path.clone());
                    config.borrow().save();
                    open_library(&state, &widgets, path.clone());
                });
                recent_box.append(&row);
            }
        })
    };
    refresh_recent();
    popover.connect_show(move |_| refresh_recent());

    rows.append(&popover_separator());
    activate_row(&rows, &popover, "New item…", "win.new-item");
    activate_row(&rows, &popover, "Acquire…", "win.acquire");
    activate_row(&rows, &popover, "Add PDF…", "win.add-pdf");
    activate_row(&rows, &popover, "Add EPUB…", "win.add-epub");
    activate_row(&rows, &popover, "Add folder of PDFs…", "win.add-folder");
    activate_row(&rows, &popover, "Add from URL…", "win.add-url");
    activate_row(&rows, &popover, "Import…", "win.import");
    rows.append(&popover_separator());
    activate_row(&rows, &popover, "Manage tags…", "win.tags");
    activate_row(&rows, &popover, "Custom fields…", "win.custom-fields");
    activate_row(&rows, &popover, "Columns…", "win.columns").set_tooltip_text(Some(
        "Show or hide optional spreadsheet columns — Tags, Status, and any custom fields",
    ));
    activate_row(&rows, &popover, "Nodes…", "win.nodes").set_tooltip_text(Some(
        "People, places, and other things you can connect your references to",
    ));
    activate_row(
        &rows,
        &popover,
        "Relations map (whole library)…",
        "win.library-graph",
    )
    .set_tooltip_text(Some(
        "A bird's-eye view of everything connected to everything, plus most-connected/\
             most-cited rankings",
    ));
    activate_row(&rows, &popover, "Tasks…", "win.tasks");
    activate_row(&rows, &popover, "Find duplicates…", "win.duplicates");
    rows.append(&popover_separator());
    activate_row(&rows, &popover, "Cite…", "win.cite");
    activate_row(&rows, &popover, "Export bibliography…", "win.export-bib");
    rows.append(&popover_separator());
    activate_row(&rows, &popover, "Save current search…", "win.save-search");
    activate_row(&rows, &popover, "Save a copy…", "win.save-copy").set_tooltip_text(Some(
        "Copy your whole library to a folder you choose — no setup required",
    ));
    activate_row(&rows, &popover, "Set up backup…", "win.backup-wizard").set_tooltip_text(Some(
        "Guided setup: sign in to GitHub, then commit and push in one step",
    ));
    activate_row(&rows, &popover, "Back up (git commit)…", "win.backup").set_tooltip_text(Some(
        "Versioned backups with git — more powerful, but needs a one-time git setup",
    ));
    activate_row(&rows, &popover, "Sign in to GitHub…", "win.github-signin");
    activate_row(&rows, &popover, "Back up to WebDAV…", "win.webdav-backup");
    activate_row(
        &rows,
        &popover,
        "Automatic backups…",
        "win.auto-backup-settings",
    );
    activate_row(&rows, &popover, "Reindex search", "win.reindex");
    rows.append(&popover_separator());

    let current = config
        .borrow()
        .theme
        .clone()
        .unwrap_or_else(|| "system".to_string());
    let theme_buttons: Rc<RefCell<Vec<(String, gtk4::Button)>>> = Rc::new(RefCell::new(Vec::new()));
    for (label, name) in [("System", "system"), ("Light", "light"), ("Dark", "dark")] {
        let row = popover_button(label, false);
        if name == current {
            row.add_css_class("fond-toggle-active");
        }
        rows.append(&row);
        theme_buttons.borrow_mut().push((name.to_string(), row));
    }
    for (name, row) in theme_buttons.borrow().iter() {
        let popover = popover.clone();
        let name = name.clone();
        let all = theme_buttons.clone();
        row.connect_clicked(move |b| {
            popover.popdown();
            let _ = b.activate_action("win.theme", Some(&name.to_variant()));
            for (n, btn) in all.borrow().iter() {
                if *n == name {
                    btn.add_css_class("fond-toggle-active");
                } else {
                    btn.remove_css_class("fond-toggle-active");
                }
            }
        });
    }
    rows.append(&popover_separator());

    activate_row(&rows, &popover, "Keyboard shortcuts", "win.shortcuts");
    activate_row(&rows, &popover, "About Kartoteka", "win.about");

    popover
}

/// Every accelerator with a `win.*`/reader-local action behind it, grouped for the shortcuts
/// dialog — the single place a new accelerator needs to also be added for it to actually be
/// discoverable (`Ctrl+?`/`F1`, or Menu → "Keyboard shortcuts").
const SHORTCUT_GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "Library",
        &[
            ("Ctrl+O", "Open library…"),
            ("Ctrl+Shift+N", "New library…"),
        ],
    ),
    (
        "Entries",
        &[
            ("Ctrl+N", "New item…"),
            ("Ctrl+K", "Cite (search and copy a citation)…"),
            ("Ctrl+F", "Focus the search field"),
        ],
    ),
    (
        "PDF/EPUB reader",
        &[("Ctrl+Z", "Undo"), ("Ctrl+Shift+Z", "Redo")],
    ),
    (
        "Help",
        &[("Ctrl+? or F1", "Keyboard shortcuts (this list)")],
    ),
];

/// A plain, hand-built list of every keyboard shortcut in the app — same "hand-built rows,
/// not a rigid template" house style as the hamburger popover, rather than
/// `Gtk.ShortcutsWindow`'s more constrained group/section model.
fn show_shortcuts_dialog(widgets: &Rc<Widgets>) {
    let dialog = adw::Window::new();
    dialog.set_title(Some("Keyboard shortcuts"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(420, 520);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    view.add_top_bar(&header);

    let outer = gtk4::Box::new(Orientation::Vertical, 16);
    outer.set_margin_top(16);
    outer.set_margin_bottom(16);
    outer.set_margin_start(18);
    outer.set_margin_end(18);

    for (group, shortcuts) in SHORTCUT_GROUPS {
        let group_label = gtk4::Label::new(Some(group));
        group_label.set_xalign(0.0);
        group_label.add_css_class("caption-heading");
        group_label.add_css_class("dim-label");
        outer.append(&group_label);
        for (accel, action) in *shortcuts {
            let row = gtk4::Box::new(Orientation::Horizontal, 12);
            let action_label = gtk4::Label::new(Some(action));
            action_label.set_xalign(0.0);
            action_label.set_hexpand(true);
            let accel_label = gtk4::Label::new(Some(accel));
            accel_label.add_css_class("dim-label");
            accel_label.add_css_class("caption");
            row.append(&action_label);
            row.append(&accel_label);
            outer.append(&row);
        }
    }

    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_child(Some(&outer));
    scroll.set_vexpand(true);
    view.set_content(Some(&scroll));
    dialog.set_content(Some(&view));
    dialog.present();
}

fn add_window_actions(
    window: &adw::ApplicationWindow,
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    config: &Rc<RefCell<Config>>,
    auto_backup_timer: &Rc<RefCell<Option<glib::SourceId>>>,
) {
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let config = config.clone();
        let action = gio::SimpleAction::new("new-library", None);
        action.connect_activate(move |_, _| show_new_library_dialog(&state, &widgets, &config));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let config = config.clone();
        let action = gio::SimpleAction::new("open-library", None);
        action.connect_activate(move |_, _| open_library_picker(&state, &widgets, &config));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let config = config.clone();
        let action = gio::SimpleAction::new("move-library", None);
        action.connect_activate(move |_, _| show_move_library_dialog(&state, &widgets, &config));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("acquire", None);
        action.connect_activate(move |_, _| show_acquire_dialog(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("new-item", None);
        action.connect_activate(move |_, _| show_new_item_dialog(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("cite", None);
        action.connect_activate(move |_, _| show_cite_picker(&state, &widgets));
        window.add_action(&action);
    }
    {
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("focus-search", None);
        action.connect_activate(move |_, _| {
            widgets.search.grab_focus();
        });
        window.add_action(&action);
    }
    {
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("shortcuts", None);
        action.connect_activate(move |_, _| show_shortcuts_dialog(&widgets));
        window.add_action(&action);
    }
    if let Some(app) = window.application() {
        app.set_accels_for_action("win.cite", &["<Primary>k"]);
        app.set_accels_for_action("win.new-item", &["<Primary>n"]);
        app.set_accels_for_action("win.open-library", &["<Primary>o"]);
        app.set_accels_for_action("win.new-library", &["<Primary><Shift>n"]);
        app.set_accels_for_action("win.focus-search", &["<Primary>f"]);
        app.set_accels_for_action("win.shortcuts", &["<Primary>question", "F1"]);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("add-pdf", None);
        action.connect_activate(move |_, _| show_add_pdf(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("add-epub", None);
        action.connect_activate(move |_, _| show_add_epub(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("add-folder", None);
        action.connect_activate(move |_, _| show_add_folder(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("add-url", None);
        action.connect_activate(move |_, _| show_add_url_dialog(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("export-bib", None);
        action.connect_activate(move |_, _| show_export_dialog(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("duplicates", None);
        action.connect_activate(move |_, _| show_duplicates_dialog(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("tags", None);
        action.connect_activate(move |_, _| show_tags_dialog(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("custom-fields", None);
        action.connect_activate(move |_, _| show_custom_fields_dialog(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("columns", None);
        action.connect_activate(move |_, _| show_columns_dialog(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("nodes", None);
        action.connect_activate(move |_, _| show_nodes_dialog(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("library-graph", None);
        action.connect_activate(move |_, _| show_library_graph(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("tasks", None);
        action.connect_activate(move |_, _| show_global_tasks_dialog(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("import", None);
        action.connect_activate(move |_, _| show_import_dialog(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("save-search", None);
        action.connect_activate(move |_, _| save_search_dialog(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("save-copy", None);
        action.connect_activate(move |_, _| show_save_copy_dialog(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("backup", None);
        action.connect_activate(move |_, _| show_backup_dialog(&state, &widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let config = config.clone();
        let auto_backup_timer = auto_backup_timer.clone();
        let action = gio::SimpleAction::new("backup-wizard", None);
        action.connect_activate(move |_, _| {
            show_backup_wizard(&state, &widgets, &config, &auto_backup_timer)
        });
        window.add_action(&action);
    }
    {
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("github-signin", None);
        action.connect_activate(move |_, _| show_github_signin(&widgets));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let config = config.clone();
        let action = gio::SimpleAction::new("webdav-backup", None);
        action.connect_activate(move |_, _| show_webdav_dialog(&state, &widgets, &config));
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let config = config.clone();
        let auto_backup_timer = auto_backup_timer.clone();
        let action = gio::SimpleAction::new("auto-backup-settings", None);
        action.connect_activate(move |_, _| {
            show_auto_backup_dialog(&state, &widgets, &config, &auto_backup_timer)
        });
        window.add_action(&action);
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let action = gio::SimpleAction::new("reindex", None);
        action.connect_activate(move |_, _| reindex(&state, &widgets));
        window.add_action(&action);
    }
    {
        let config = config.clone();
        let initial = config
            .borrow()
            .theme
            .clone()
            .unwrap_or_else(|| "system".to_string());
        let action = gio::SimpleAction::new_stateful(
            "theme",
            Some(glib::VariantTy::STRING),
            &initial.to_variant(),
        );
        action.connect_activate(move |action, param| {
            if let Some(name) = param.and_then(|p| p.str()).map(|s| s.to_string()) {
                apply_theme(&name);
                action.set_state(&name.to_variant());
                config.borrow_mut().theme = Some(name);
                config.borrow().save();
            }
        });
        window.add_action(&action);
    }
    {
        let window_for_about = window.clone();
        let action = gio::SimpleAction::new("about", None);
        action.connect_activate(move |_, _| show_about(&window_for_about));
        window.add_action(&action);
    }
}

fn apply_theme(name: &str) {
    let scheme = match name {
        "light" => adw::ColorScheme::ForceLight,
        "dark" => adw::ColorScheme::ForceDark,
        _ => adw::ColorScheme::Default,
    };
    adw::StyleManager::default().set_color_scheme(scheme);
}

fn reindex(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    let rebuilt = {
        let s = state.borrow();
        let Some(library) = s.library.as_ref() else {
            toast(widgets, "Open a library first");
            return;
        };
        let dir = library.root().join(".kartoteka").join("index");
        fond_index::SearchIndex::rebuild(library, &dir, |_| None, |_| None)
    };
    match rebuilt {
        Ok(index) => {
            state.borrow_mut().index = Some(index);
            toast(widgets, "Search index rebuilt");
        }
        Err(e) => toast(widgets, &format!("Reindex failed: {e}")),
    }
}

/// Show the changelog (embedded at compile time) in a scrollable window.
fn show_changelog(window: &adw::ApplicationWindow) {
    const CHANGELOG: &str = include_str!("../../../CHANGELOG.md");

    let win = adw::Window::builder()
        .transient_for(window)
        .title("Changelog")
        .default_width(660)
        .default_height(580)
        .build();
    let view = adw::ToolbarView::new();
    let bare_header = adw::HeaderBar::new();
    bare_header.add_css_class("fond-chrome");
    view.add_top_bar(&bare_header);

    let text = gtk4::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .left_margin(16)
        .right_margin(16)
        .top_margin(12)
        .bottom_margin(12)
        .build();
    text.buffer().set_text(CHANGELOG);
    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_child(Some(&text));
    scroll.set_vexpand(true);
    view.set_content(Some(&scroll));

    win.set_content(Some(&view));
    win.present();
}

fn show_about(window: &adw::ApplicationWindow) {
    let about = gtk4::AboutDialog::builder()
        .program_name("Kartoteka")
        .version(env!("CARGO_PKG_VERSION"))
        .comments("Plain-file reference manager and PDF library — part of Fond")
        .transient_for(window)
        .modal(true)
        .build();
    about.present();
}

/// Pick a PDF, identify it (DOI sniff or embedded metadata), create the entry, and attach
/// the PDF. Identification and any network lookup run on a worker thread.
fn show_add_pdf(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    if state.borrow().library.is_none() {
        toast(widgets, "Open a library first");
        return;
    }
    let dialog = gtk4::FileDialog::builder().title("Add PDF").build();
    let parent = widgets.window.clone();
    let state = state.clone();
    let widgets = widgets.clone();
    dialog.open(Some(&parent), gio::Cancellable::NONE, move |result| {
        if let Ok(file) = result {
            if let Some(path) = file.path() {
                import_pdf(&state, &widgets, path);
            }
        }
    });
}

#[allow(deprecated)]
fn import_pdf(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, path: PathBuf) {
    toast(widgets, "Reading PDF…");

    // (is_bibtex, payload, pages) on success.
    let (sender, receiver) = worker::channel::<Result<(bool, String, Option<u32>), String>>();
    let worker_path = path.clone();
    std::thread::spawn(move || {
        let _ = sender.send(identify_pdf(&worker_path));
    });

    let state = state.clone();
    let widgets = widgets.clone();
    receiver.attach(move |result| {
        match result {
            Ok((is_bibtex, payload, pages)) => {
                let added = {
                    let s = state.borrow();
                    let library = s.library.as_ref().expect("library open");
                    if is_bibtex {
                        library.add_bibtex(&payload)
                    } else {
                        library.add_from_yaml(&payload)
                    }
                };
                match added {
                    Ok(keys) if !keys.is_empty() => {
                        let key = keys[0].clone();
                        let attached = {
                            let s = state.borrow();
                            let library = s.library.as_ref().expect("library open");
                            library.store_attachment(&key, &path, pages)
                        };
                        match attached {
                            Ok(_) => toast(&widgets, &format!("Added {key} with its PDF")),
                            Err(e) => toast(
                                &widgets,
                                &format!(
                                    "Added {key}, but couldn't attach the PDF: {}",
                                    friendly::bib_error(&e)
                                ),
                            ),
                        }
                        reload_current(&state, &widgets);
                    }
                    Ok(_) => toast(&widgets, "The record produced no entry"),
                    Err(e) => toast(&widgets, &friendly::bib_error(&e)),
                }
            }
            Err(e) => toast(&widgets, &format!("Couldn't read that PDF: {e}")),
        }
        glib::ControlFlow::Break
    });
}

/// Worker-thread identification: sniff a DOI from the PDF text (article), else an ISBN
/// (book), else build a minimal entry from embedded metadata. Each network step is soft — a
/// lookup failure falls through to the next signal rather than failing the whole import.
/// Returns `(is_bibtex, payload, page_count)`.
fn identify_pdf(path: &std::path::Path) -> Result<(bool, String, Option<u32>), String> {
    let pdfium = fond_doc::bind_pdfium().map_err(|e| format!("PDFium unavailable: {e}"))?;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let pages = fond_doc::page_count(&pdfium, &bytes).ok().map(|n| n as u32);

    let text = fond_doc::extract_text(&pdfium, &bytes)
        .ok()
        .map(|t| t.full_text());
    let mut isbn_seen = None;

    if let Some(text) = &text {
        if let Some(doi) = fond_doc::find_doi(text) {
            if let Ok(bibtex) = fond_bib::acquire::fetch_doi_bibtex(&doi) {
                return Ok((true, bibtex, pages));
            }
        }
        if let Some(isbn) = fond_doc::find_isbn(text) {
            match fond_bib::acquire::fetch_isbn_yaml(&isbn) {
                Ok(yaml) => return Ok((false, yaml, pages)),
                Err(_) => isbn_seen = Some(isbn),
            }
        }
    }

    let meta = fond_doc::extract_metadata(&pdfium, &bytes).map_err(|e| e.to_string())?;
    if let Some(title) = meta.title {
        let yaml = fond_bib::acquire::minimal_book_yaml(
            &title,
            meta.author.as_deref(),
            isbn_seen.as_deref(),
        )
        .map_err(|e| e.to_string())?;
        return Ok((false, yaml, pages));
    }

    Err("could not identify the PDF (no DOI/ISBN in text, no embedded title)".to_string())
}

fn show_add_epub(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    if state.borrow().library.is_none() {
        toast(widgets, "Open a library first");
        return;
    }
    let filter = gtk4::FileFilter::new();
    filter.set_name(Some("EPUB books"));
    filter.add_pattern("*.epub");
    let filters = gio::ListStore::new::<gtk4::FileFilter>();
    filters.append(&filter);
    let dialog = gtk4::FileDialog::builder()
        .title("Add EPUB")
        .filters(&filters)
        .build();
    let parent = widgets.window.clone();
    let state = state.clone();
    let widgets = widgets.clone();
    dialog.open(Some(&parent), gio::Cancellable::NONE, move |result| {
        if let Ok(file) = result {
            if let Some(path) = file.path() {
                import_epub(&state, &widgets, path);
            }
        }
    });
}

/// Import a dropped EPUB: read its OPF metadata and (off the UI thread, since it may hit the
/// network for an ISBN lookup) build the entry YAML, then add the entry and attach the .epub.
#[allow(deprecated)]
fn import_epub(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, path: PathBuf) {
    toast(widgets, "Reading EPUB…");

    let (sender, receiver) = worker::channel::<Result<String, String>>();
    let worker_path = path.clone();
    std::thread::spawn(move || {
        let _ = sender.send(epub_entry_yaml(&worker_path));
    });

    let state = state.clone();
    let widgets = widgets.clone();
    receiver.attach(move |result| {
        match result {
            Ok(yaml) => {
                let added = {
                    let s = state.borrow();
                    let library = s.library.as_ref().expect("library open");
                    library.add_from_yaml(&yaml)
                };
                match added {
                    Ok(keys) if !keys.is_empty() => {
                        let key = keys[0].clone();
                        let attached = {
                            let s = state.borrow();
                            let library = s.library.as_ref().expect("library open");
                            library.store_attachment(&key, &path, None)
                        };
                        match attached {
                            Ok(_) => toast(&widgets, &format!("Added {key} with its EPUB")),
                            Err(e) => toast(
                                &widgets,
                                &format!(
                                    "Added {key}, but couldn't attach the EPUB: {}",
                                    friendly::bib_error(&e)
                                ),
                            ),
                        }
                        rebuild_index_silent(&state);
                        reload_current(&state, &widgets);
                    }
                    Ok(_) => toast(&widgets, "The record produced no entry"),
                    Err(e) => toast(&widgets, &friendly::bib_error(&e)),
                }
            }
            Err(e) => toast(&widgets, &format!("Couldn't read that EPUB: {e}")),
        }
        glib::ControlFlow::Break
    });
}

/// Worker-thread logic for an EPUB: parse the OPF, enrich via ISBN lookup when one is present
/// (falling back to the OPF fields on failure), else build from the OPF fields directly.
/// Returns the Hayagriva YAML document to add.
fn epub_entry_yaml(path: &std::path::Path) -> Result<String, String> {
    let meta = fond_doc::extract_epub_metadata(path).map_err(|e| e.to_string())?;
    if let Some(isbn) = meta.isbn.as_deref() {
        if let Ok(yaml) = fond_bib::acquire::fetch_isbn_yaml(isbn) {
            return Ok(yaml);
        }
    }
    let title = meta
        .title
        .as_deref()
        .ok_or_else(|| "the EPUB has no title in its metadata".to_string())?;
    fond_bib::acquire::book_yaml(
        title,
        &meta.authors,
        meta.date.as_deref(),
        meta.publisher.as_deref(),
        meta.isbn.as_deref(),
    )
    .map_err(|e| e.to_string())
}

/// Progress messages streamed from the bulk-import worker to the UI.
enum FolderProgress {
    Step {
        done: usize,
        total: usize,
        name: String,
    },
    Done {
        added: usize,
        failed: usize,
    },
}

/// Pick a folder and import every PDF under it (recursively).
fn show_add_folder(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    if state.borrow().library.is_none() {
        toast(widgets, "Open a library first");
        return;
    }
    let dialog = gtk4::FileDialog::builder()
        .title("Add folder of PDFs")
        .build();
    let parent = widgets.window.clone();
    let state = state.clone();
    let widgets = widgets.clone();
    dialog.select_folder(Some(&parent), gio::Cancellable::NONE, move |result| {
        if let Ok(folder) = result {
            if let Some(path) = folder.path() {
                import_pdf_folder(&state, &widgets, path);
            }
        }
    });
}

/// Walk `folder` for `*.pdf` files and identify+add+attach each on one worker thread. The
/// whole batch runs on a cloned `Library` (cheap, `Send`), so entries and blobs are written
/// off the main thread; progress is streamed back for toasts, then the list reloads.
#[allow(deprecated)]
fn import_pdf_folder(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, folder: PathBuf) {
    let library = match state.borrow().library.as_ref() {
        Some(lib) => lib.clone(),
        None => return,
    };

    let pdfs: Vec<PathBuf> = walkdir::WalkDir::new(&folder)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            p.extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case("pdf"))
        })
        .collect();

    if pdfs.is_empty() {
        toast(widgets, "No PDFs found in that folder");
        return;
    }
    toast(widgets, &format!("Importing {} PDFs…", pdfs.len()));

    let (sender, receiver) = worker::channel::<FolderProgress>();
    std::thread::spawn(move || {
        let total = pdfs.len();
        let (mut added, mut failed) = (0usize, 0usize);
        for (i, path) in pdfs.iter().enumerate() {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string();
            let _ = sender.send(FolderProgress::Step {
                done: i,
                total,
                name,
            });
            let ok = identify_pdf(path).and_then(|(is_bibtex, payload, pages)| {
                let keys = if is_bibtex {
                    library.add_bibtex(&payload)
                } else {
                    library.add_from_yaml(&payload)
                }
                .map_err(|e| e.to_string())?;
                let key = keys.into_iter().next().ok_or("no entry produced")?;
                library
                    .store_attachment(&key, path, pages)
                    .map_err(|e| e.to_string())?;
                Ok(())
            });
            match ok {
                Ok(()) => added += 1,
                Err(_) => failed += 1,
            }
        }
        let _ = sender.send(FolderProgress::Done { added, failed });
    });

    let state = state.clone();
    let widgets = widgets.clone();
    receiver.attach(move |msg| match msg {
        FolderProgress::Step { done, total, name } => {
            toast(&widgets, &format!("[{}/{}] {name}", done + 1, total));
            glib::ControlFlow::Continue
        }
        FolderProgress::Done { added, failed } => {
            let note = if failed == 0 {
                format!("Imported {added} PDFs")
            } else {
                format!("Imported {added} PDFs, {failed} could not be identified")
            };
            toast(&widgets, &note);
            reload_current(&state, &widgets);
            glib::ControlFlow::Break
        }
    });
}

/// Look up an open-access PDF for a DOI via Unpaywall, download it, and attach it to `key`.
/// The lookup and download run on a worker thread; the attach then happens on the main
/// thread via the open library. `email` is required by the Unpaywall API.
#[allow(deprecated)]
fn find_pdf_unpaywall(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, key: &str, doi: &str) {
    toast(widgets, "Looking for an open-access PDF…");
    let email = fond_vault::Identity::from_git_config()
        .map(|id| id.email)
        .unwrap_or_else(|_| "anonymous@kartoteka.app".to_string());
    let doi = doi.to_string();

    // Ok(bytes, filename) on success.
    let (sender, receiver) = worker::channel::<Result<(Vec<u8>, String), String>>();
    std::thread::spawn(move || {
        let _ = sender.send(unpaywall_download(&doi, &email));
    });

    let state = state.clone();
    let widgets = widgets.clone();
    let key = key.to_string();
    receiver.attach(move |result| {
        match result {
            Ok((bytes, filename)) => {
                // Write to a temp file so store_attachment can hash+copy it.
                let tmp = std::env::temp_dir().join(&filename);
                let attached = std::fs::write(&tmp, &bytes)
                    .map_err(|e| e.to_string())
                    .and_then(|_| {
                        let s = state.borrow();
                        let library = s.library.as_ref().expect("library open");
                        library
                            .store_attachment(&key, &tmp, None)
                            .map_err(|e| e.to_string())
                    });
                let _ = std::fs::remove_file(&tmp);
                match attached {
                    Ok(_) => {
                        toast(&widgets, "Attached an open-access PDF");
                        reload_current(&state, &widgets);
                    }
                    Err(e) => toast(&widgets, &format!("Download ok, attach failed: {e}")),
                }
            }
            Err(e) => toast(&widgets, &format!("No open-access PDF found: {e}")),
        }
        glib::ControlFlow::Break
    });
}

/// Query Unpaywall for a DOI's best OA location and download the PDF bytes.
fn unpaywall_download(doi: &str, email: &str) -> Result<(Vec<u8>, String), String> {
    let api = format!(
        "https://api.unpaywall.org/v2/{}?email={}",
        urlencode(doi),
        urlencode(email)
    );
    let client = reqwest::blocking::Client::builder()
        .user_agent("Kartoteka")
        .build()
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = client
        .get(&api)
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;

    let url = json
        .get("best_oa_location")
        .and_then(|l| l.get("url_for_pdf"))
        .and_then(|u| u.as_str())
        .filter(|u| !u.is_empty())
        .ok_or("no open-access PDF is listed for this DOI")?;

    let bytes = client
        .get(url)
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .bytes()
        .map_err(|e| e.to_string())?
        .to_vec();

    if bytes.len() < 5 || &bytes[..5] != b"%PDF-" {
        return Err("the linked file was not a PDF".to_string());
    }
    let filename = format!("{}.pdf", doi.replace('/', "_"));
    Ok((bytes, filename))
}

/// "Add from URL": paste a web page URL, scrape its citation `<meta>` tags into an entry,
/// and grab a linked PDF if the page advertises one.
fn show_add_url_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    if state.borrow().library.is_none() {
        toast(widgets, "Open a library first");
        return;
    }

    let dialog = adw::Window::new();
    dialog.set_title(Some("Add from URL"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(460, -1);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let add = gtk4::Button::with_label("Add");
    add.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&add);
    view.add_top_bar(&header);

    let content = gtk4::Box::new(Orientation::Vertical, 8);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);
    let url = gtk4::Entry::builder()
        .placeholder_text("https://…")
        .activates_default(true)
        .build();
    content.append(&labeled("Page URL", &url));
    let hint = gtk4::Label::new(Some(
        "Reads citation metadata (Highwire / Dublin Core / Open Graph) and attaches a linked PDF when one is offered.",
    ));
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    hint.set_wrap(true);
    hint.set_xalign(0.0);
    content.append(&hint);
    view.set_content(Some(&content));
    dialog.set_content(Some(&view));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        let url = url.clone();
        add.connect_clicked(move |_| {
            let u = url.text().trim().to_string();
            if !(u.starts_with("http://") || u.starts_with("https://")) {
                toast(&widgets, "Enter a full http(s) URL");
                return;
            }
            add_from_url(&state, &widgets, u);
            dialog.close();
        });
    }
    dialog.present();
    url.grab_focus();
}

/// Scrape result: the entry YAML plus an optional downloaded PDF `(bytes, filename)`.
type ScrapeResult = Result<(String, Option<(Vec<u8>, String)>), String>;

/// Fetch `url`, scrape its citation metadata on a worker thread, then create the entry and
/// attach any PDF the page advertised. All network I/O is off the main thread.
#[allow(deprecated)]
fn add_from_url(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, url: String) {
    toast(widgets, "Fetching page…");

    let (sender, receiver) = worker::channel::<ScrapeResult>();
    std::thread::spawn(move || {
        let _ = sender.send(scrape_url(&url));
    });

    let state = state.clone();
    let widgets = widgets.clone();
    receiver.attach(move |result| {
        match result {
            Ok((yaml, pdf)) => {
                let added = {
                    let s = state.borrow();
                    s.library
                        .as_ref()
                        .expect("library open")
                        .add_from_yaml(&yaml)
                };
                match added {
                    Ok(keys) if !keys.is_empty() => {
                        let key = keys[0].clone();
                        if let Some((bytes, filename)) = pdf {
                            let tmp = std::env::temp_dir().join(&filename);
                            let attached = std::fs::write(&tmp, &bytes)
                                .map_err(|e| e.to_string())
                                .and_then(|_| {
                                    let s = state.borrow();
                                    s.library
                                        .as_ref()
                                        .expect("library open")
                                        .store_attachment(&key, &tmp, None)
                                        .map_err(|e| e.to_string())
                                });
                            let _ = std::fs::remove_file(&tmp);
                            match attached {
                                Ok(_) => toast(&widgets, &format!("Added {key} with its PDF")),
                                Err(e) => {
                                    toast(&widgets, &format!("Added {key}, attach failed: {e}"))
                                }
                            }
                        } else {
                            toast(&widgets, &format!("Added {key}"));
                        }
                        reload_current(&state, &widgets);
                    }
                    Ok(_) => toast(&widgets, "The page produced no entry"),
                    Err(e) => toast(&widgets, &friendly::bib_error(&e)),
                }
            }
            Err(e) => toast(
                &widgets,
                &format!("Couldn't get a reference from that page ({e})."),
            ),
        }
        glib::ControlFlow::Break
    });
}

/// Worker body for [`add_from_url`]: fetch the HTML, scrape metadata into an entry YAML, and
/// download a linked PDF (verifying the `%PDF-` magic) if one is advertised.
fn scrape_url(url: &str) -> ScrapeResult {
    let client = reqwest::blocking::Client::builder()
        .user_agent("Kartoteka")
        .build()
        .map_err(|e| e.to_string())?;
    let html = client
        .get(url)
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .map_err(|e| e.to_string())?;

    let meta = crate::webmeta::WebMeta::from_html(&html);
    if !meta.is_usable() {
        return Err("no citation metadata found on that page".to_string());
    }
    let authors = meta.authors.join("; ");
    let yaml = build_entry_yaml(&NewItemFields {
        ty: &meta.entry_type,
        title: &meta.title,
        authors: &authors,
        date: &meta.date,
        container: &meta.container,
        publisher: &meta.publisher,
        doi: &meta.doi,
        isbn: &meta.isbn,
        url,
        volume: &meta.volume,
        issue: &meta.issue,
        pages: &meta.pages,
        language: &meta.language,
    });

    let pdf = if meta.pdf_url.is_empty() {
        None
    } else {
        let pdf_url = crate::webmeta::resolve_url(url, &meta.pdf_url);
        client
            .get(&pdf_url)
            .send()
            .ok()
            .and_then(|r| r.error_for_status().ok())
            .and_then(|r| r.bytes().ok())
            .map(|b| b.to_vec())
            .filter(|b| b.len() >= 5 && &b[..5] == b"%PDF-")
            .map(|bytes| {
                let name = if !meta.doi.is_empty() {
                    format!("{}.pdf", meta.doi.replace('/', "_"))
                } else {
                    "download.pdf".to_string()
                };
                (bytes, name)
            })
    };
    Ok((yaml, pdf))
}

/// The item types the manual "New item" form offers: (display label, Hayagriva `type`).
const ITEM_TYPES: &[(&str, &str)] = &[
    ("Book", "book"),
    ("Journal article", "article"),
    ("Book chapter", "chapter"),
    ("Conference paper", "conference"),
    ("Report", "report"),
    ("Thesis", "thesis"),
    ("Manuscript", "manuscript"),
    ("Web page", "web"),
    ("Blog post", "blog"),
    ("Newspaper article", "newspaper"),
    ("Anthology", "anthology"),
    ("Periodical", "periodical"),
    ("Miscellaneous", "misc"),
];

/// Quote and escape a scalar for a double-quoted YAML value.
fn yaml_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Fields for a one-entry Hayagriva YAML snippet, from either the manual "New item" form or
/// a URL scrape. `date` accepts any precision Hayagriva's date parser does (`YYYY`,
/// `YYYY-MM`, `YYYY-MM-DD`); every other field is a plain string, empty meaning absent.
#[derive(Default)]
struct NewItemFields<'a> {
    ty: &'a str,
    title: &'a str,
    /// Split on `;`/newlines into individual names.
    authors: &'a str,
    date: &'a str,
    container: &'a str,
    publisher: &'a str,
    doi: &'a str,
    isbn: &'a str,
    url: &'a str,
    volume: &'a str,
    issue: &'a str,
    /// `firstpage-lastpage`, or just one page number.
    pages: &'a str,
    /// An ISO 639 language code (e.g. `en`).
    language: &'a str,
}

/// Build a one-entry Hayagriva YAML snippet. The placeholder key is replaced with a
/// generated one by `add_from_yaml`.
fn build_entry_yaml(f: &NewItemFields) -> String {
    let mut out = String::from("new-item:\n");
    out.push_str(&format!("  type: {}\n", f.ty));
    if !f.title.trim().is_empty() {
        out.push_str(&format!("  title: {}\n", yaml_quote(f.title.trim())));
    }
    let names: Vec<&str> = f
        .authors
        .split([';', '\n'])
        .map(|n| n.trim())
        .filter(|n| !n.is_empty())
        .collect();
    if !names.is_empty() {
        out.push_str("  author:\n");
        for name in names {
            out.push_str(&format!("    - {}\n", yaml_quote(name)));
        }
    }
    if !f.date.trim().is_empty() {
        out.push_str(&format!("  date: {}\n", f.date.trim()));
    }
    if !f.publisher.trim().is_empty() {
        out.push_str(&format!(
            "  publisher: {}\n",
            yaml_quote(f.publisher.trim())
        ));
    }
    if !f.url.trim().is_empty() {
        out.push_str(&format!("  url: {}\n", yaml_quote(f.url.trim())));
    }
    let doi = f.doi.trim();
    let isbn = f.isbn.trim();
    if !doi.is_empty() || !isbn.is_empty() {
        out.push_str("  serial-number:\n");
        if !doi.is_empty() {
            out.push_str(&format!("    doi: {}\n", yaml_quote(doi)));
        }
        if !isbn.is_empty() {
            out.push_str(&format!("    isbn: {}\n", yaml_quote(isbn)));
        }
    }
    if !f.container.trim().is_empty() {
        let parent_ty = match f.ty {
            "chapter" | "anthology" => "anthology",
            "conference" => "proceedings",
            _ => "periodical",
        };
        out.push_str("  parent:\n");
        out.push_str(&format!("    type: {parent_ty}\n"));
        out.push_str(&format!("    title: {}\n", yaml_quote(f.container.trim())));
    }
    if !f.volume.trim().is_empty() {
        out.push_str(&format!("  volume: {}\n", f.volume.trim()));
    }
    if !f.issue.trim().is_empty() {
        out.push_str(&format!("  issue: {}\n", f.issue.trim()));
    }
    if !f.pages.trim().is_empty() {
        out.push_str(&format!("  page-range: {}\n", yaml_quote(f.pages.trim())));
    }
    if !f.language.trim().is_empty() {
        out.push_str(&format!("  language: {}\n", f.language.trim()));
    }
    out
}

/// Copy a Typst citation (`@key`) for `key` to the clipboard.
fn copy_citation(widgets: &Rc<Widgets>, key: &str) {
    let citation = format!("@{key}");
    widgets.window.clipboard().set_text(&citation);
    toast(widgets, &format!("Copied {citation}"));
}

/// Cite-while-you-write picker (Ctrl+K): search the library and copy a Typst `@key`
/// citation to the clipboard, ready to paste into a document. Row-activate or Enter copies
/// the highlighted entry; the dialog stays open so several citations can be grabbed in turn.
fn show_cite_picker(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    let entries: Vec<(String, String, String)> = {
        let s = state.borrow();
        if s.library.is_none() {
            toast(widgets, "Open a library first");
            return;
        }
        s.entries
            .iter()
            .map(|e| {
                let label = if e.title.is_empty() {
                    e.key.clone()
                } else {
                    e.title.clone()
                };
                let sub = match (e.author.is_empty(), e.year.is_empty()) {
                    (false, false) => format!("{} · {}", e.author, e.year),
                    (false, true) => e.author.clone(),
                    (true, false) => e.year.clone(),
                    (true, true) => e.key.clone(),
                };
                (e.key.clone(), label, sub)
            })
            .collect()
    };

    let dialog = adw::Window::new();
    dialog.set_title(Some("Cite"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(460, 460);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    let search = gtk4::SearchEntry::new();
    search.set_placeholder_text(Some("Search to cite (@key → clipboard)"));
    search.set_width_chars(32);
    header.set_title_widget(Some(&search));
    view.add_top_bar(&header);

    let listbox = gtk4::ListBox::new();
    listbox.set_selection_mode(gtk4::SelectionMode::Single);
    listbox.add_css_class("fond-list");
    let scroll = gtk4::ScrolledWindow::new();
    scroll.add_css_class("fond-ground");
    scroll.set_child(Some(&listbox));
    scroll.set_vexpand(true);
    view.set_content(Some(&scroll));
    dialog.set_content(Some(&view));

    // Each row carries its citation key via widget data, so the filter can rebuild rows
    // freely and activation always knows which key to copy.
    let entries = Rc::new(entries);
    let rebuild = {
        let listbox = listbox.clone();
        let entries = entries.clone();
        Rc::new(move |query: &str| {
            while let Some(child) = listbox.first_child() {
                listbox.remove(&child);
            }
            let q = query.to_lowercase();
            for (key, label, sub) in entries.iter() {
                if !q.is_empty()
                    && !label.to_lowercase().contains(&q)
                    && !sub.to_lowercase().contains(&q)
                    && !key.to_lowercase().contains(&q)
                {
                    continue;
                }
                let vbox = gtk4::Box::new(Orientation::Vertical, 2);
                vbox.set_margin_top(6);
                vbox.set_margin_bottom(6);
                vbox.set_margin_start(8);
                vbox.set_margin_end(8);
                let title = gtk4::Label::new(Some(label));
                title.set_halign(gtk4::Align::Start);
                title.set_xalign(0.0);
                title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                title.add_css_class("fond-row-title");
                let meta = gtk4::Label::new(Some(sub));
                meta.set_halign(gtk4::Align::Start);
                meta.set_xalign(0.0);
                meta.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                meta.add_css_class("fond-row-meta");
                vbox.append(&title);
                vbox.append(&meta);
                let row = gtk4::ListBoxRow::new();
                row.add_css_class("fond-row");
                row.set_child(Some(&vbox));
                unsafe { row.set_data("cite-key", key.clone()) };
                listbox.append(&row);
            }
            if let Some(first) = listbox.row_at_index(0) {
                listbox.select_row(Some(&first));
            }
        })
    };
    rebuild("");

    {
        let rebuild = rebuild.clone();
        search.connect_search_changed(move |e| rebuild(&e.text()));
    }

    // Enter in the search box activates the selected row.
    {
        let listbox = listbox.clone();
        search.connect_activate(move |_| {
            if let Some(row) = listbox.selected_row() {
                row.activate();
            }
        });
    }

    // Row-activate copies the citation.
    {
        let widgets = widgets.clone();
        listbox.connect_row_activated(move |_, row| {
            let key = unsafe { row.data::<String>("cite-key") };
            if let Some(key) = key {
                let key = unsafe { key.as_ref() };
                copy_citation(&widgets, key);
            }
        });
    }

    dialog.present();
    search.grab_focus();
}

/// Manual entry form: pick a type and fill the common fields, then create the entry.
fn show_new_item_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    if state.borrow().library.is_none() {
        toast(widgets, "Open a library first");
        return;
    }

    let dialog = adw::Window::new();
    dialog.set_title(Some("New item"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(480, -1);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let create = gtk4::Button::with_label("Create");
    create.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&create);
    view.add_top_bar(&header);

    let content = gtk4::Box::new(Orientation::Vertical, 8);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);

    let type_labels: Vec<&str> = ITEM_TYPES.iter().map(|(label, _)| *label).collect();
    let type_drop = gtk4::DropDown::from_strings(&type_labels);
    let title = gtk4::Entry::new();
    let authors = gtk4::Entry::builder()
        .placeholder_text("Last, First; Last, First")
        .build();
    let year = gtk4::Entry::new();
    let container = gtk4::Entry::builder()
        .placeholder_text("Journal / book title")
        .build();
    let publisher = gtk4::Entry::new();
    let doi = gtk4::Entry::new();
    let isbn = gtk4::Entry::new();
    let url = gtk4::Entry::new();

    content.append(&labeled("Type", &type_drop));
    content.append(&labeled("Title", &title));
    content.append(&labeled("Author(s)", &authors));
    content.append(&labeled("Year", &year));
    content.append(&labeled("Journal / book", &container));
    content.append(&labeled("Publisher", &publisher));
    content.append(&labeled("DOI", &doi));
    content.append(&labeled("ISBN", &isbn));
    content.append(&labeled("URL", &url));

    let scroller = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .max_content_height(560)
        .propagate_natural_height(true)
        .child(&content)
        .build();
    view.set_content(Some(&scroller));
    dialog.set_content(Some(&view));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        create.connect_clicked(move |_| {
            if title.text().trim().is_empty() {
                toast(&widgets, "A title is required");
                return;
            }
            let ty = ITEM_TYPES[type_drop.selected() as usize].1;
            let yaml = build_entry_yaml(&NewItemFields {
                ty,
                title: &title.text(),
                authors: &authors.text(),
                date: &year.text(),
                container: &container.text(),
                publisher: &publisher.text(),
                doi: &doi.text(),
                isbn: &isbn.text(),
                url: &url.text(),
                ..Default::default()
            });
            let added = {
                let s = state.borrow();
                let library = s.library.as_ref().expect("library open");
                library.add_from_yaml(&yaml)
            };
            match added {
                Ok(keys) if !keys.is_empty() => {
                    toast(&widgets, &format!("Created {}", keys[0]));
                    reload_current(&state, &widgets);
                    dialog.close();
                }
                Ok(_) => toast(&widgets, "No entry was created"),
                Err(e) => toast(&widgets, &friendly::bib_error(&e)),
            }
        });
    }

    dialog.present();
}

/// Create a new "book part" entry (a chapter/section) from an existing book/anthology,
/// `source_key`: the source's own fields become the new entry's `parent:` block (see
/// `fond_bib::entry::book_part_yaml`), so citing the part also correctly credits the book
/// without copying its data — editing the book later can be pulled into the part with
/// "Refresh from source book…" instead of the two silently drifting apart the way a plain
/// duplicate-then-retype workflow would.
fn show_create_book_part_dialog(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    source_key: String,
) {
    let source_title = {
        let s = state.borrow();
        let Some(library) = s.library.as_ref() else {
            toast(widgets, "Open a library first");
            return;
        };
        match library.load_entry(&source_key) {
            Ok(parsed) => {
                bibentry::title_string(&parsed.entry).unwrap_or_else(|| source_key.clone())
            }
            Err(e) => {
                toast(widgets, &friendly::bib_error(&e));
                return;
            }
        }
    };

    let dialog = adw::Window::new();
    dialog.set_title(Some("Create book part"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(460, -1);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let create = gtk4::Button::with_label("Create");
    create.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&create);
    view.add_top_bar(&header);

    let content = gtk4::Box::new(Orientation::Vertical, 10);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let intro = gtk4::Label::new(Some(&format!(
        "A new entry citing its own title/author/pages, crediting \u{201c}{source_title}\u{201d} \
         as the book it's from.",
    )));
    intro.set_wrap(true);
    intro.set_xalign(0.0);
    intro.add_css_class("dim-label");
    content.append(&intro);

    let title_entry = gtk4::Entry::new();
    let authors_entry = gtk4::Entry::builder()
        .placeholder_text("Last, First; Last, First")
        .build();
    let pages_entry = gtk4::Entry::builder().placeholder_text("45-67").build();
    content.append(&labeled("Chapter title", &title_entry));
    content.append(&labeled("Chapter author(s)", &authors_entry));
    content.append(&labeled("Pages", &pages_entry));

    // Whether the source's own author(s) become the new part's editor (the common case —
    // an edited anthology is usually catalogued with the editor filling Kartoteka's one
    // "Author(s)" field, since there's no separate editor field on the book form) or stay
    // as its author (a single/co-authored book being split into named sections).
    let role_row = gtk4::Box::new(Orientation::Vertical, 4);
    let role_label = gtk4::Label::new(Some("The book's listed author(s) are its:"));
    role_label.set_xalign(0.0);
    role_label.add_css_class("caption-heading");
    role_row.append(&role_label);
    let role_drop =
        gtk4::DropDown::from_strings(&["Editor(s) of this collection", "Author(s) of this book"]);
    role_row.append(&role_drop);
    content.append(&role_row);

    view.set_content(Some(&content));
    dialog.set_content(Some(&view));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        create.connect_clicked(move |_| {
            let title = title_entry.text().trim().to_string();
            if title.is_empty() {
                toast(&widgets, "The chapter needs a title");
                return;
            }
            let role = match role_drop.selected() {
                0 => fond_bib::entry::ParentRole::Editor,
                _ => fond_bib::entry::ParentRole::Author,
            };
            let role_str = match role {
                fond_bib::entry::ParentRole::Editor => "editor",
                fond_bib::entry::ParentRole::Author => "author",
            };
            let result = {
                let s = state.borrow();
                s.library
                    .as_ref()
                    .map(|lib| -> fond_bib::Result<Vec<String>> {
                        let source = lib.load_entry(&source_key)?;
                        let yaml = fond_bib::entry::book_part_yaml(
                            &source.entry,
                            role,
                            "chapter",
                            &title,
                            &authors_entry.text(),
                            &pages_entry.text(),
                        )?;
                        lib.add_from_yaml(&yaml)
                    })
            };
            match result {
                Some(Ok(keys)) if !keys.is_empty() => {
                    let new_key = keys[0].clone();
                    let note_result = {
                        let s = state.borrow();
                        s.library.as_ref().map(|lib| {
                            let mut note =
                                lib.load_note(&new_key).ok().flatten().unwrap_or_default();
                            note.frontmatter.derived_from_book = Some(source_key.clone());
                            note.frontmatter.derived_from_role = Some(role_str.to_string());
                            lib.write_note(&new_key, &note)
                        })
                    };
                    if let Some(Err(e)) = note_result {
                        toast(&widgets, &friendly::bib_error(&e));
                    }
                    toast(&widgets, &format!("Created {new_key}"));
                    rebuild_index_silent(&state);
                    reload_current(&state, &widgets);
                    select_key(&state, &widgets, &new_key);
                    dialog.close();
                }
                Some(Ok(_)) => toast(&widgets, "No entry was created"),
                Some(Err(e)) => toast(&widgets, &friendly::bib_error(&e)),
                None => {}
            }
        });
    }

    dialog.present();
}

/// "Refresh from source book…": re-derive a book part's `parent:` block from its source
/// book's *current* fields (see `fond_bib::entry::refresh_book_part_parent`), leaving the
/// part's own title/author/pages untouched. No confirmation dialog — same one-step feel as
/// any other inline edit, and it's non-destructive (only the `parent:` block changes).
fn refresh_book_part(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    key: String,
    source_key: String,
) {
    let role = {
        let s = state.borrow();
        let Some(library) = s.library.as_ref() else {
            toast(widgets, "Open a library first");
            return;
        };
        let role_str = library
            .load_note(&key)
            .ok()
            .flatten()
            .and_then(|n| n.frontmatter.derived_from_role);
        match role_str.as_deref() {
            Some("author") => fond_bib::entry::ParentRole::Author,
            _ => fond_bib::entry::ParentRole::Editor,
        }
    };
    let result = {
        let s = state.borrow();
        s.library.as_ref().map(|lib| -> fond_bib::Result<()> {
            let source = lib.load_entry(&source_key)?;
            let part = lib.load_entry(&key)?;
            let part_yaml = fond_bib::entry::serialize_entry_as(&part.entry, &part.key)?;
            let refreshed =
                fond_bib::entry::refresh_book_part_parent(&part_yaml, &source.entry, role)?;
            let reparsed = fond_bib::entry::parse_single(&refreshed, &lib.entry_path(&key))?;
            lib.write_entry(&reparsed.entry)?;
            Ok(())
        })
    };
    match result {
        Some(Ok(())) => {
            rebuild_index_silent(state);
            reload_current(state, widgets);
            select_key(state, widgets, &key);
            toast(widgets, "Refreshed from source book");
        }
        Some(Err(e)) => toast(widgets, &friendly::bib_error(&e)),
        None => {}
    }
}

/// A row with a caption, a value label, and a "Choose…" button that opens a file picker
/// and writes the chosen path into `slot` (updating the label and calling `on_change`).
fn file_pick_row(
    window: &adw::ApplicationWindow,
    caption: &str,
    button_label: &str,
    slot: Rc<RefCell<Option<PathBuf>>>,
    on_change: Rc<dyn Fn()>,
) -> gtk4::Box {
    let row = gtk4::Box::new(Orientation::Horizontal, 8);
    let name = gtk4::Label::new(Some(caption));
    name.set_xalign(0.0);
    name.set_width_chars(14);
    name.set_halign(gtk4::Align::Start);
    let value = gtk4::Label::new(Some("none"));
    value.add_css_class("dim-label");
    value.set_xalign(0.0);
    value.set_hexpand(true);
    value.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
    let button = gtk4::Button::with_label(button_label);

    {
        let window = window.clone();
        let value = value.clone();
        button.connect_clicked(move |_| {
            let dialog = gtk4::FileDialog::builder().title("Choose file").build();
            let value = value.clone();
            let slot = slot.clone();
            let on_change = on_change.clone();
            dialog.open(Some(&window), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        value.set_text(
                            path.file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("selected"),
                        );
                        *slot.borrow_mut() = Some(path);
                        on_change();
                    }
                }
            });
        });
    }

    row.append(&name);
    row.append(&value);
    row.append(&button);
    row
}

/// Import from a BetterBibTeX `.bib` (required) and optionally a Zotero `zotero.sqlite`.
/// The import runs on a worker thread (a `Library` is just a path, so it is `Send`).
#[allow(deprecated)]
fn show_import_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    if state.borrow().library.is_none() {
        toast(widgets, "Open a library first");
        return;
    }

    let dialog = adw::Window::new();
    dialog.set_title(Some("Import"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(500, -1);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let import = gtk4::Button::with_label("Import");
    import.add_css_class("suggested-action");
    import.set_sensitive(false);
    header.pack_start(&cancel);
    header.pack_end(&import);
    view.add_top_bar(&header);

    let content = gtk4::Box::new(Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let bib_path: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let zotero_path: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));

    let enable_import: Rc<dyn Fn()> = {
        let import = import.clone();
        let bib_path = bib_path.clone();
        Rc::new(move || import.set_sensitive(bib_path.borrow().is_some()))
    };
    let noop: Rc<dyn Fn()> = Rc::new(|| {});

    content.append(&file_pick_row(
        &widgets.window,
        "BibTeX (.bib)",
        "Choose…",
        bib_path.clone(),
        enable_import.clone(),
    ));
    content.append(&file_pick_row(
        &widgets.window,
        "Zotero (optional)",
        "Choose…",
        zotero_path.clone(),
        noop,
    ));

    let overwrite_row = gtk4::Box::new(Orientation::Horizontal, 8);
    let overwrite_label = gtk4::Label::new(Some("Overwrite existing keys"));
    overwrite_label.set_xalign(0.0);
    overwrite_label.set_hexpand(true);
    overwrite_label.set_halign(gtk4::Align::Start);
    let overwrite = gtk4::Switch::new();
    overwrite.set_halign(gtk4::Align::End);
    overwrite_row.append(&overwrite_label);
    overwrite_row.append(&overwrite);
    content.append(&overwrite_row);

    let spinner = gtk4::Spinner::new();
    content.append(&spinner);

    view.set_content(Some(&content));
    dialog.set_content(Some(&view));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }

    {
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        let import = import.clone();
        let spinner = spinner.clone();
        import.connect_clicked(move |import| {
            let Some(bib) = bib_path.borrow().clone() else {
                return;
            };
            let source = match std::fs::read_to_string(&bib) {
                Ok(s) => s,
                Err(e) => {
                    toast(&widgets, &format!("Could not read .bib: {e}"));
                    return;
                }
            };
            let opts = fond_bib::ImportOptions {
                overwrite: overwrite.is_active(),
                copy_attachments: true,
                attachment_base: bib.parent().map(|p| p.to_path_buf()),
                zotero_db: zotero_path.borrow().clone(),
            };
            let library = state.borrow().library.clone().expect("library open");

            import.set_sensitive(false);
            spinner.start();

            let (sender, receiver) = worker::channel::<Result<fond_bib::ImportReport, String>>();
            std::thread::spawn(move || {
                let _ = sender.send(
                    library
                        .import_bibtex(&source, &opts)
                        .map_err(|e| e.to_string()),
                );
            });

            let state = state.clone();
            let widgets = widgets.clone();
            let dialog = dialog.clone();
            let import = import.clone();
            let spinner = spinner.clone();
            receiver.attach(move |result| {
                spinner.stop();
                match result {
                    Ok(report) => {
                        let mut msg = format!("Imported {} entries", report.imported.len());
                        if !report.collections_created.is_empty() {
                            msg.push_str(&format!(
                                ", {} collections",
                                report.collections_created.len()
                            ));
                        }
                        if !report.skipped_key_collisions.is_empty() {
                            msg.push_str(&format!(
                                ", {} skipped",
                                report.skipped_key_collisions.len()
                            ));
                        }
                        toast(&widgets, &msg);
                        dialog.close();
                        reload_current(&state, &widgets);
                    }
                    Err(e) => {
                        toast(&widgets, &format!("Import failed: {e}"));
                        import.set_sensitive(true);
                    }
                }
                glib::ControlFlow::Break
            });
        });
    }

    dialog.present();
}

/// Commit the library to git (a local snapshot backup). Initialises the repo if needed.
/// Attachments and `.kartoteka/` are gitignored, so only the plain records are committed.
/// The plain, no-git-required backup option: pick a destination folder and copy the whole
/// library into a timestamped subfolder there. "Back up (git commit)…" below is more
/// powerful (versioned history, optional GitHub push) but needs git set up first — this is
/// the one-click fallback for anyone who just wants a safety copy without learning git.
fn show_save_copy_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    let root = state
        .borrow()
        .library
        .as_ref()
        .map(|l| l.root().to_path_buf());
    let Some(root) = root else {
        toast(widgets, "Open a library first");
        return;
    };

    let dialog = gtk4::FileDialog::builder()
        .title("Choose where to save a copy")
        .build();
    let widgets = widgets.clone();
    let parent = widgets.window.clone();
    dialog.select_folder(Some(&parent), gio::Cancellable::NONE, move |result| {
        if let Ok(folder) = result {
            if let Some(dest_parent) = folder.path() {
                save_library_copy(&widgets, root.clone(), dest_parent);
            }
        }
    });
}

/// Copy `root` into a new `<name>-backup-<timestamp>` folder under `dest_parent`, off the UI
/// thread (a library's PDFs can make this slow). Skips `.git` and `.kartoteka` — version
/// control internals and the disposable search/metadata cache, neither of which belong in a
/// plain copy.
#[allow(deprecated)]
fn save_library_copy(widgets: &Rc<Widgets>, root: PathBuf, dest_parent: PathBuf) {
    let lib_name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "library".to_string());
    let stamp = glib::DateTime::now_local()
        .ok()
        .and_then(|d| d.format("%Y-%m-%d-%H%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let dest = dest_parent.join(format!("{lib_name}-backup-{stamp}"));

    toast(widgets, "Saving a copy…");
    let (sender, receiver) = worker::channel::<Result<PathBuf, String>>();
    let worker_dest = dest.clone();
    std::thread::spawn(move || {
        let result = copy_library_dir(&root, &worker_dest)
            .map(|_| worker_dest)
            .map_err(|e| e.to_string());
        let _ = sender.send(result);
    });

    let widgets = widgets.clone();
    receiver.attach(move |result| {
        match result {
            Ok(dest) => toast(&widgets, &format!("Saved a copy to {}", dest.display())),
            Err(e) => toast(&widgets, &format!("Couldn't save a copy: {e}")),
        }
        glib::ControlFlow::Break
    });
}

/// Recursively copy `src` into `dest`, skipping `.git` and `.kartoteka` — a fresh backup
/// snapshot doesn't need version-control internals or the disposable search/metadata cache.
fn copy_library_dir(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    copy_dir_filtered(src, dest, true)
}

/// Recursively copy every file of `src` into `dest`, `.git` and `.kartoteka` included —
/// used for relocating a library (`move_library`), where those need to survive the move
/// intact (git history, the search index that'd otherwise just be rebuilt anyway).
fn copy_dir_all(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    copy_dir_filtered(src, dest, false)
}

fn copy_dir_filtered(
    src: &std::path::Path,
    dest: &std::path::Path,
    skip_git_and_cache: bool,
) -> std::io::Result<()> {
    for entry in walkdir::WalkDir::new(src).into_iter().filter_entry(|e| {
        !skip_git_and_cache || !matches!(e.file_name().to_str(), Some(".git" | ".kartoteka"))
    }) {
        let entry = entry.map_err(std::io::Error::other)?;
        let rel = entry
            .path()
            .strip_prefix(src)
            .expect("walkdir yields paths under src");
        let target = dest.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Move the currently-open library to a different folder: pick a new parent, relocate
/// everything there (git history and search index included — this is the same library,
/// just living somewhere else), and repoint config/UI at the new path.
fn show_move_library_dialog(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    config: &Rc<RefCell<Config>>,
) {
    let root = state
        .borrow()
        .library
        .as_ref()
        .map(|l| l.root().to_path_buf());
    let Some(root) = root else {
        toast(widgets, "Open a library first");
        return;
    };

    let dialog = gtk4::FileDialog::builder()
        .title("Choose the new location")
        .build();
    let state = state.clone();
    let widgets = widgets.clone();
    let config = config.clone();
    let parent = widgets.window.clone();
    dialog.select_folder(Some(&parent), gio::Cancellable::NONE, move |result| {
        if let Ok(folder) = result {
            if let Some(new_parent) = folder.path() {
                move_library(&state, &widgets, &config, root.clone(), new_parent);
            }
        }
    });
}

#[allow(deprecated)]
fn move_library(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    config: &Rc<RefCell<Config>>,
    root: PathBuf,
    new_parent: PathBuf,
) {
    let Some(name) = root.file_name().map(|n| n.to_os_string()) else {
        return;
    };
    let new_root = new_parent.join(&name);
    if new_root == root {
        toast(widgets, "That's already where this library is");
        return;
    }
    if new_root.exists() {
        toast(
            widgets,
            &format!(
                "\"{}\" already has a folder named \"{}\"",
                new_parent.display(),
                name.to_string_lossy()
            ),
        );
        return;
    }

    toast(widgets, "Moving library…");
    let (sender, receiver) = worker::channel::<Result<(), String>>();
    let worker_root = root.clone();
    let worker_new_root = new_root.clone();
    std::thread::spawn(move || {
        // A plain rename is instant and preserves everything, but only works within the same
        // filesystem — falls back to a full copy-then-remove across filesystems/devices.
        let result = std::fs::rename(&worker_root, &worker_new_root).or_else(|_| {
            copy_dir_all(&worker_root, &worker_new_root)?;
            std::fs::remove_dir_all(&worker_root)
        });
        let _ = sender.send(result.map_err(|e| e.to_string()));
    });

    let state = state.clone();
    let widgets = widgets.clone();
    let config = config.clone();
    receiver.attach(move |result| {
        match result {
            Ok(()) => {
                config.borrow_mut().library_path = Some(new_root.clone());
                config.borrow().save();
                open_library(&state, &widgets, new_root.clone());
                toast(
                    &widgets,
                    &format!("Moved library to {}", new_root.display()),
                );
            }
            Err(e) => toast(&widgets, &format!("Couldn't move the library: {e}")),
        }
        glib::ControlFlow::Break
    });
}

/// A four-step wizard-container helper: a plain vertical `Box` with the same margins on
/// every step page, so `show_backup_wizard`'s pages all line up without repeating the margin
/// calls four times.
fn wizard_step_box() -> gtk4::Box {
    let b = gtk4::Box::new(Orientation::Vertical, 12);
    b.set_margin_top(18);
    b.set_margin_bottom(18);
    b.set_margin_start(18);
    b.set_margin_end(18);
    b
}

/// Guided "set up backup" flow — sign in to GitHub (skipped if already signed in), choose a
/// repository name/visibility and commit message, then commit and push in one sequence,
/// finally offering to turn on automatic backups. This is a single entry point wrapping the
/// same underlying calls the four separate "Sign in to GitHub…" / "Back up (git commit)…" /
/// "Automatic backups…" menu items already use
/// (`github::request_device_code`/`poll_for_access_token`, `secret_store::save_github_token`,
/// `fond_vault::Vault::init`/`stage_all`/`commit`, `github::create_repo`, `Vault::set_remote`,
/// `Vault::push_github`, `start_auto_backup_timer`) — just sequenced into one dialog instead
/// of four menu actions a user has to find and run in the right order themselves. The four
/// existing items are left in place unchanged, as power-user shortcuts once backup is already
/// set up (e.g. a one-off commit with a custom message).
#[allow(deprecated)]
fn show_backup_wizard(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    config: &Rc<RefCell<Config>>,
    timer: &Rc<RefCell<Option<glib::SourceId>>>,
) {
    let root = state
        .borrow()
        .library
        .as_ref()
        .map(|l| l.root().to_path_buf());
    let Some(root) = root else {
        toast(widgets, "Open a library first");
        return;
    };
    if !github::is_configured() {
        toast(widgets, "GitHub sign-in isn't configured yet");
        return;
    }

    let dialog = adw::Window::new();
    dialog.set_title(Some("Set up backup"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(440, -1);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    header.pack_start(&cancel);
    view.add_top_bar(&header);
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }

    // Cancels an in-flight device-code poll if the wizard is closed mid-sign-in — same
    // mechanism `present_device_dialog` uses.
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let cancelled = cancelled.clone();
        dialog.connect_close_request(move |_| {
            cancelled.store(true, Ordering::Relaxed);
            glib::Propagation::Proceed
        });
    }

    let stack = gtk4::Stack::new();
    stack.set_transition_type(gtk4::StackTransitionType::SlideLeftRight);

    // ---- Step 1: sign in (skipped, after a quick username check, if already signed in) ----
    let signin_page = wizard_step_box();
    let signin_status = gtk4::Label::new(Some("Checking GitHub sign-in…"));
    signin_status.set_wrap(true);
    signin_status.set_xalign(0.0);
    let signin_code = gtk4::Label::new(None);
    signin_code.add_css_class("title-1");
    signin_code.set_selectable(true);
    signin_code.set_visible(false);
    let signin_link =
        gtk4::LinkButton::with_label("https://github.com/login/device", "Open GitHub");
    signin_link.set_visible(false);
    let signin_spinner = gtk4::Spinner::new();
    signin_spinner.start();
    signin_page.append(&signin_status);
    signin_page.append(&signin_code);
    signin_page.append(&signin_link);
    signin_page.append(&signin_spinner);
    stack.add_named(&signin_page, Some("signin"));

    // ---- Step 2: repository name/visibility + commit message ----
    let setup_page = wizard_step_box();
    let setup_intro = gtk4::Label::new(None);
    setup_intro.set_wrap(true);
    setup_intro.set_xalign(0.0);
    let repo_name_default = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("kartoteka-library")
        .to_string();
    let repo_name_entry = gtk4::Entry::builder().text(&repo_name_default).build();
    let private_row = gtk4::Box::new(Orientation::Horizontal, 8);
    let private_label = gtk4::Label::new(Some("Private repository"));
    private_label.set_xalign(0.0);
    private_label.set_hexpand(true);
    private_label.set_halign(gtk4::Align::Start);
    let private_switch = gtk4::Switch::new();
    private_switch.set_halign(gtk4::Align::End);
    private_switch.set_active(true);
    private_row.append(&private_label);
    private_row.append(&private_switch);
    let default_msg = glib::DateTime::now_local()
        .ok()
        .and_then(|d| d.format("Backup %Y-%m-%d %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Backup".to_string());
    let message_entry = gtk4::Entry::builder()
        .text(&default_msg)
        .activates_default(true)
        .build();
    let start_button = gtk4::Button::with_label("Back up now");
    start_button.add_css_class("suggested-action");
    start_button.set_halign(gtk4::Align::End);
    setup_page.append(&setup_intro);
    setup_page.append(&gtk4::Label::new(Some("Repository name")));
    setup_page.append(&repo_name_entry);
    setup_page.append(&private_row);
    setup_page.append(&gtk4::Label::new(Some("Commit message")));
    setup_page.append(&message_entry);
    setup_page.append(&start_button);
    stack.add_named(&setup_page, Some("setup"));

    // ---- Step 3: progress ----
    let progress_page = wizard_step_box();
    let progress_label = gtk4::Label::new(Some("Backing up…"));
    progress_label.set_wrap(true);
    progress_label.set_xalign(0.0);
    let progress_spinner = gtk4::Spinner::new();
    progress_spinner.start();
    progress_page.append(&progress_label);
    progress_page.append(&progress_spinner);
    stack.add_named(&progress_page, Some("progress"));

    // ---- Step 4: done, offer automatic backups ----
    let done_page = wizard_step_box();
    let done_label = gtk4::Label::new(Some("Backed up and pushed to GitHub."));
    done_label.set_wrap(true);
    done_label.set_xalign(0.0);
    let auto_row = gtk4::Box::new(Orientation::Horizontal, 8);
    let auto_label = gtk4::Label::new(Some("Keep backing up automatically from now on"));
    auto_label.set_wrap(true);
    auto_label.set_xalign(0.0);
    auto_label.set_hexpand(true);
    auto_label.set_halign(gtk4::Align::Start);
    let auto_switch = gtk4::Switch::new();
    auto_switch.set_halign(gtk4::Align::End);
    auto_switch.set_active(true);
    auto_row.append(&auto_label);
    auto_row.append(&auto_switch);
    let interval_labels: Vec<&str> = AUTO_BACKUP_INTERVALS.iter().map(|(l, _)| *l).collect();
    let interval_drop = gtk4::DropDown::from_strings(&interval_labels);
    interval_drop.set_selected(1); // "Every 30 minutes" — same default as `show_auto_backup_dialog`
    let finish_button = gtk4::Button::with_label("Finish");
    finish_button.add_css_class("suggested-action");
    finish_button.set_halign(gtk4::Align::End);
    done_page.append(&done_label);
    done_page.append(&auto_row);
    done_page.append(&labeled("Interval", &interval_drop));
    done_page.append(&finish_button);
    stack.add_named(&done_page, Some("done"));

    view.set_content(Some(&stack));
    dialog.set_content(Some(&view));

    // Step 2 -> 3 -> 4: commit (if there are changes), create the GitHub repo on first backup
    // (same auto-creation logic as `push_to_github`), then push.
    {
        let widgets = widgets.clone();
        let stack = stack.clone();
        let progress_label = progress_label.clone();
        let dialog = dialog.clone();
        let root = root.clone();
        start_button.connect_clicked(move |_| {
            let repo_name = repo_name_entry.text().trim().to_string();
            if repo_name.is_empty() {
                toast(&widgets, "Enter a repository name");
                return;
            }
            let private = private_switch.is_active();
            let message = message_entry.text().to_string();
            stack.set_visible_child_name("progress");
            progress_label.set_text("Committing and pushing…");

            let root_thread = root.clone();
            let (sender, receiver) = worker::channel::<Result<(), String>>();
            std::thread::spawn(move || {
                let result = (|| -> Result<(), String> {
                    let vault = fond_vault::Vault::open(&root_thread)
                        .or_else(|_| fond_vault::Vault::init(&root_thread))
                        .map_err(|e| e.to_string())?;
                    let identity =
                        fond_vault::Identity::from_git_config().map_err(|e| e.to_string())?;
                    let status = vault.status().map_err(|e| e.to_string())?;
                    if !status.is_clean() {
                        vault.stage_all().map_err(|e| e.to_string())?;
                        vault
                            .commit(&message, &identity)
                            .map_err(|e| e.to_string())?;
                    }
                    let Some(token) = secret_store::load_github_token() else {
                        return Err("not signed in to GitHub".to_string());
                    };
                    if vault.remote_url("origin").is_none() {
                        let clone_url = github::create_repo(&token, &repo_name, private)
                            .map_err(|e| e.to_string())?;
                        vault
                            .set_remote("origin", &clone_url)
                            .map_err(|e| e.to_string())?;
                    }
                    vault
                        .push_github("origin", &token)
                        .map_err(|e| e.to_string())
                })();
                let _ = sender.send(result);
            });

            let widgets = widgets.clone();
            let stack = stack.clone();
            let dialog = dialog.clone();
            receiver.attach(move |result| {
                match result {
                    Ok(()) => stack.set_visible_child_name("done"),
                    Err(e) => {
                        toast(&widgets, &format!("Backup failed: {e}"));
                        dialog.close();
                    }
                }
                glib::ControlFlow::Break
            });
        });
    }

    // Step 4 -> close: persist the automatic-backup choice and (re)start the shared timer,
    // same as `show_auto_backup_dialog`'s own Save button.
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let config = config.clone();
        let timer = timer.clone();
        let dialog = dialog.clone();
        finish_button.connect_clicked(move |_| {
            let minutes = AUTO_BACKUP_INTERVALS
                .get(interval_drop.selected() as usize)
                .map(|(_, m)| *m)
                .unwrap_or(30);
            {
                let mut c = config.borrow_mut();
                c.auto_backup_enabled = auto_switch.is_active();
                c.auto_backup_interval_mins = minutes;
                c.save();
            }
            start_auto_backup_timer(&state, &widgets, &config, &timer);
            toast(&widgets, "Backup set up");
            dialog.close();
        });
    }

    dialog.present();

    // Kick off step 1: already signed in? Just confirm the token still works and skip
    // straight to step 2. Otherwise run the device flow inline in the "signin" page —
    // the same calls `present_device_dialog` makes, just landing in this wizard's own page
    // instead of a second popup window.
    stack.set_visible_child_name("signin");
    if let Some(token) = secret_store::load_github_token() {
        signin_status.set_text("Confirming GitHub sign-in…");
        let (sender, receiver) = worker::channel::<Result<String, String>>();
        std::thread::spawn(move || {
            let _ = sender.send(github::fetch_username(&token).map_err(|e| e.to_string()));
        });
        let widgets = widgets.clone();
        let stack = stack.clone();
        let setup_intro = setup_intro.clone();
        let dialog = dialog.clone();
        receiver.attach(move |result| {
            match result {
                Ok(username) => {
                    setup_intro.set_text(&format!(
                        "Signed in as {username}. Choose a name for the GitHub repository \
                         that will hold your library:"
                    ));
                    stack.set_visible_child_name("setup");
                }
                Err(e) => {
                    toast(
                        &widgets,
                        &format!(
                            "GitHub sign-in has expired ({e}) — sign in again from the \
                             hamburger menu"
                        ),
                    );
                    dialog.close();
                }
            }
            glib::ControlFlow::Break
        });
    } else {
        signin_status.set_text("Requesting a sign-in code from GitHub…");
        let (sender, receiver) = worker::channel::<Result<github::DeviceCodeResponse, String>>();
        std::thread::spawn(move || {
            let _ = sender
                .send(github::request_device_code(github::CLIENT_ID).map_err(|e| e.to_string()));
        });
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        let cancelled = cancelled.clone();
        let signin_status = signin_status.clone();
        let signin_code = signin_code.clone();
        let signin_link = signin_link.clone();
        let stack = stack.clone();
        let setup_intro = setup_intro.clone();
        receiver.attach(move |result| {
            match result {
                Ok(device) => {
                    signin_status.set_text("Open the page below and enter this code:");
                    signin_code.set_text(&device.user_code);
                    signin_code.set_visible(true);
                    signin_link.set_uri(&device.verification_uri);
                    signin_link.set_label("Open GitHub");
                    signin_link.set_visible(true);

                    let (sender2, receiver2) =
                        worker::channel::<Result<(String, String), String>>();
                    {
                        let cancelled = cancelled.clone();
                        std::thread::spawn(move || {
                            let result = github::poll_for_access_token(
                                github::CLIENT_ID,
                                &device,
                                &cancelled,
                            )
                            .and_then(|token| {
                                github::fetch_username(&token).map(|user| (token, user))
                            })
                            .map_err(|e| e.to_string());
                            let _ = sender2.send(result);
                        });
                    }
                    let widgets = widgets.clone();
                    let dialog = dialog.clone();
                    let stack = stack.clone();
                    let setup_intro = setup_intro.clone();
                    receiver2.attach(move |result| {
                        match result {
                            Ok((token, username)) => {
                                if let Err(e) = secret_store::save_github_token(&token) {
                                    toast(
                                        &widgets,
                                        &format!("Signed in, but couldn't store token: {e}"),
                                    );
                                }
                                setup_intro.set_text(&format!(
                                    "Signed in as {username}. Choose a name for the GitHub \
                                     repository that will hold your library:"
                                ));
                                stack.set_visible_child_name("setup");
                            }
                            Err(e) if e.contains("cancelled") => {}
                            Err(e) => {
                                toast(&widgets, &format!("Sign-in failed: {e}"));
                                dialog.close();
                            }
                        }
                        glib::ControlFlow::Break
                    });
                }
                Err(e) => {
                    toast(&widgets, &format!("GitHub error: {e}"));
                    dialog.close();
                }
            }
            glib::ControlFlow::Break
        });
    }
}

fn show_backup_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    let root = state
        .borrow()
        .library
        .as_ref()
        .map(|l| l.root().to_path_buf());
    let Some(root) = root else {
        toast(widgets, "Open a library first");
        return;
    };

    let dialog = adw::Window::new();
    dialog.set_title(Some("Back up library"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(440, -1);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let commit = gtk4::Button::with_label("Commit");
    commit.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&commit);
    view.add_top_bar(&header);

    let content = gtk4::Box::new(Orientation::Vertical, 8);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    let default_msg = glib::DateTime::now_local()
        .ok()
        .and_then(|d| d.format("Backup %Y-%m-%d %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Backup".to_string());
    let entry = gtk4::Entry::builder()
        .text(&default_msg)
        .activates_default(true)
        .build();
    content.append(&entry);

    let push_row = gtk4::Box::new(Orientation::Horizontal, 8);
    let push_label = gtk4::Label::new(Some("Push to GitHub after commit"));
    push_label.set_xalign(0.0);
    push_label.set_hexpand(true);
    push_label.set_halign(gtk4::Align::Start);
    let push_switch = gtk4::Switch::new();
    push_switch.set_halign(gtk4::Align::End);
    push_switch.set_active(secret_store::load_github_token().is_some());
    push_row.append(&push_label);
    push_row.append(&push_switch);
    content.append(&push_row);

    view.set_content(Some(&content));
    dialog.set_content(Some(&view));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        let entry = entry.clone();
        let push_switch = push_switch.clone();
        commit.connect_clicked(move |_| {
            let message = entry.text().to_string();
            let vault = fond_vault::Vault::open(&root).or_else(|_| fond_vault::Vault::init(&root));
            let result = vault.and_then(|v| {
                let identity = fond_vault::Identity::from_git_config()?;
                v.stage_all()?;
                v.commit(&message, &identity)
            });
            match result {
                Ok(oid) => {
                    toast(
                        &widgets,
                        &format!("Committed {}", &oid[..oid.len().min(10)]),
                    );
                    if push_switch.is_active() {
                        push_to_github(&widgets, root.clone());
                    }
                }
                Err(e) => toast(&widgets, &friendly::vault_error(&e)),
            }
            dialog.close();
        });
    }

    dialog.present();
}

/// GitHub sign-in via the OAuth device flow. Requests a device code, shows it with a link,
/// and polls for approval on a worker thread; stores the token in the keyring on success.
#[allow(deprecated)]
fn show_github_signin(widgets: &Rc<Widgets>) {
    if !github::is_configured() {
        toast(
            widgets,
            "GitHub sign-in isn't configured yet (set CLIENT_ID in github.rs)",
        );
        return;
    }
    toast(widgets, "Contacting GitHub…");

    let (sender, receiver) = worker::channel::<Result<github::DeviceCodeResponse, String>>();
    std::thread::spawn(move || {
        let _ =
            sender.send(github::request_device_code(github::CLIENT_ID).map_err(|e| e.to_string()));
    });

    let widgets = widgets.clone();
    receiver.attach(move |result| {
        match result {
            Ok(device) => present_device_dialog(&widgets, device),
            Err(e) => toast(&widgets, &format!("GitHub error: {e}")),
        }
        glib::ControlFlow::Break
    });
}

#[allow(deprecated)]
fn present_device_dialog(widgets: &Rc<Widgets>, device: github::DeviceCodeResponse) {
    let dialog = adw::Window::new();
    dialog.set_title(Some("Sign in to GitHub"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(420, -1);

    let view = adw::ToolbarView::new();
    let bare_header = adw::HeaderBar::new();
    bare_header.add_css_class("fond-chrome");
    view.add_top_bar(&bare_header);

    let content = gtk4::Box::new(Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let intro = gtk4::Label::new(Some("Open the page below and enter this code:"));
    intro.set_wrap(true);
    intro.set_xalign(0.0);

    let code = gtk4::Label::new(Some(&device.user_code));
    code.add_css_class("title-1");
    code.set_selectable(true);

    let link = gtk4::LinkButton::with_label(&device.verification_uri, "Open GitHub");

    let waiting = gtk4::Box::new(Orientation::Horizontal, 8);
    let spinner = gtk4::Spinner::new();
    spinner.start();
    let waiting_label = gtk4::Label::new(Some("Waiting for approval…"));
    waiting_label.add_css_class("dim-label");
    waiting.append(&spinner);
    waiting.append(&waiting_label);

    content.append(&intro);
    content.append(&code);
    content.append(&link);
    content.append(&waiting);
    view.set_content(Some(&content));
    dialog.set_content(Some(&view));

    // Cancel polling when the dialog is closed.
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let cancelled = cancelled.clone();
        dialog.connect_close_request(move |_| {
            cancelled.store(true, Ordering::Relaxed);
            glib::Propagation::Proceed
        });
    }

    let (sender, receiver) = worker::channel::<Result<(String, String), String>>();
    {
        let cancelled = cancelled.clone();
        std::thread::spawn(move || {
            let result = github::poll_for_access_token(github::CLIENT_ID, &device, &cancelled)
                .and_then(|token| github::fetch_username(&token).map(|user| (token, user)))
                .map_err(|e| e.to_string());
            let _ = sender.send(result);
        });
    }

    let widgets = widgets.clone();
    let dialog_for_result = dialog.clone();
    receiver.attach(move |result| {
        match result {
            Ok((token, username)) => match secret_store::save_github_token(&token) {
                Ok(()) => toast(&widgets, &format!("Signed in to GitHub as {username}")),
                Err(e) => toast(
                    &widgets,
                    &format!("Signed in, but couldn't store token: {e}"),
                ),
            },
            Err(e) if e.contains("cancelled") => {}
            Err(e) => toast(&widgets, &format!("Sign-in failed: {e}")),
        }
        dialog_for_result.close();
        glib::ControlFlow::Break
    });

    dialog.present();
}

/// Push the library to GitHub over HTTPS with the stored token, creating the repo + remote
/// on first push. Runs on a worker thread (a fresh `Vault` is opened there from the path).
#[allow(deprecated)]
fn push_to_github(widgets: &Rc<Widgets>, root: PathBuf) {
    let Some(token) = secret_store::load_github_token() else {
        toast(
            widgets,
            "Sign in to GitHub first (menu → Sign in to GitHub)",
        );
        return;
    };
    let repo_name = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("kartoteka-library")
        .to_string();
    toast(widgets, "Pushing to GitHub…");

    let (sender, receiver) = worker::channel::<Result<(), String>>();
    std::thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let vault = fond_vault::Vault::open(&root).map_err(|e| e.to_string())?;
            if vault.remote_url("origin").is_none() {
                let clone_url =
                    github::create_repo(&token, &repo_name, true).map_err(|e| e.to_string())?;
                vault
                    .set_remote("origin", &clone_url)
                    .map_err(|e| e.to_string())?;
            }
            vault
                .push_github("origin", &token)
                .map_err(|e| e.to_string())
        })();
        let _ = sender.send(result);
    });

    let widgets = widgets.clone();
    receiver.attach(move |result| {
        match result {
            Ok(()) => toast(&widgets, "Pushed to GitHub"),
            Err(e) => toast(&widgets, &format!("Push failed: {e}")),
        }
        glib::ControlFlow::Break
    });
}

/// Configure and run a one-way WebDAV backup of the library. Credentials are saved (URL +
/// username in config, password in the keyring); the upload runs on a worker thread.
#[allow(deprecated)]
fn show_webdav_dialog(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    config: &Rc<RefCell<Config>>,
) {
    let root = state
        .borrow()
        .library
        .as_ref()
        .map(|l| l.root().to_path_buf());
    let Some(root) = root else {
        toast(widgets, "Open a library first");
        return;
    };

    let dialog = adw::Window::new();
    dialog.set_title(Some("Back up to WebDAV"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(480, -1);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let back_up = gtk4::Button::with_label("Back up");
    back_up.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&back_up);
    view.add_top_bar(&header);

    let content = gtk4::Box::new(Orientation::Vertical, 10);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let url = gtk4::Entry::builder()
        .placeholder_text("https://host/remote.php/dav/files/you/Kartoteka")
        .text(config.borrow().webdav_url.clone().unwrap_or_default())
        .build();
    let username = gtk4::Entry::builder()
        .placeholder_text("username")
        .text(config.borrow().webdav_username.clone().unwrap_or_default())
        .build();
    let password = gtk4::PasswordEntry::builder().show_peek_icon(true).build();
    password.set_text(&secret_store::load_webdav_password().unwrap_or_default());

    content.append(&labeled("WebDAV URL", &url));
    content.append(&labeled("Username", &username));
    content.append(&labeled("Password", &password));
    let spinner = gtk4::Spinner::new();
    content.append(&spinner);
    view.set_content(Some(&content));
    dialog.set_content(Some(&view));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let widgets = widgets.clone();
        let config = config.clone();
        let dialog = dialog.clone();
        let back_up = back_up.clone();
        let spinner = spinner.clone();
        back_up.connect_clicked(move |back_up| {
            let base = url.text().trim().to_string();
            let user = username.text().trim().to_string();
            let pass = password.text().to_string();
            if base.is_empty() {
                toast(&widgets, "Enter a WebDAV URL");
                return;
            }

            // Persist settings (password to keyring).
            {
                let mut c = config.borrow_mut();
                c.webdav_url = Some(base.clone());
                c.webdav_username = Some(user.clone());
                c.save();
            }
            let _ = secret_store::save_webdav_password(&pass);

            back_up.set_sensitive(false);
            spinner.start();

            let root = root.clone();
            let (sender, receiver) = worker::channel::<Result<usize, String>>();
            std::thread::spawn(move || {
                let _ = sender.send(webdav::upload_library(&base, &user, &pass, &root));
            });

            let widgets = widgets.clone();
            let dialog = dialog.clone();
            let back_up = back_up.clone();
            let spinner = spinner.clone();
            receiver.attach(move |result| {
                spinner.stop();
                match result {
                    Ok(n) => {
                        toast(&widgets, &format!("Backed up {n} files to WebDAV"));
                        dialog.close();
                    }
                    Err(e) => {
                        toast(&widgets, &format!("WebDAV backup failed: {e}"));
                        back_up.set_sensitive(true);
                    }
                }
                glib::ControlFlow::Break
            });
        });
    }

    dialog.present();
}

/// A vertical caption + widget pair.
fn labeled(caption: &str, widget: &impl IsA<gtk4::Widget>) -> gtk4::Box {
    let row = gtk4::Box::new(Orientation::Vertical, 4);
    let label = gtk4::Label::new(Some(caption));
    label.add_css_class("dim-label");
    label.set_xalign(0.0);
    label.set_halign(gtk4::Align::Start);
    row.append(&label);
    row.append(widget);
    row
}

/// The automatic-backup interval choices offered in the dialog, and their minute values.
const AUTO_BACKUP_INTERVALS: &[(&str, u32)] = &[
    ("Every 15 minutes", 15),
    ("Every 30 minutes", 30),
    ("Every hour", 60),
    ("Every 4 hours", 240),
];

/// (Re)start the automatic-backup timer from the current config: cancels any existing timer,
/// then — if enabled — schedules a repeating tick. The tick itself checks `state.library` each
/// time, so a single timer covers the window's whole lifetime rather than needing to be
/// restarted whenever a library opens or closes.
fn start_auto_backup_timer(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    config: &Rc<RefCell<Config>>,
    timer: &Rc<RefCell<Option<glib::SourceId>>>,
) {
    if let Some(id) = timer.borrow_mut().take() {
        id.remove();
    }
    let (enabled, minutes) = {
        let c = config.borrow();
        (c.auto_backup_enabled, c.auto_backup_interval_mins.max(1))
    };
    if !enabled {
        return;
    }
    let state = state.clone();
    let widgets = widgets.clone();
    let config = config.clone();
    let id = glib::timeout_add_local(Duration::from_secs(u64::from(minutes) * 60), move || {
        run_auto_backup(&state, &widgets, &config);
        glib::ControlFlow::Continue
    });
    *timer.borrow_mut() = Some(id);
}

/// One automatic-backup tick: commit locally (skipped if nothing changed since the last
/// backup), then push to GitHub if already signed in *and* a remote is already configured
/// (auto-backup never silently creates a new GitHub repo — that first push is still the
/// explicit "Back up (git commit)…" action), then mirror to WebDAV if configured. Runs off the
/// main thread since a commit/push/upload can take a while; failures surface as a toast,
/// success stays silent (routine status, not worth interrupting for).
#[allow(deprecated)]
fn run_auto_backup(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    config: &Rc<RefCell<Config>>,
) {
    let Some(root) = state
        .borrow()
        .library
        .as_ref()
        .map(|l| l.root().to_path_buf())
    else {
        return;
    };

    let github_token = secret_store::load_github_token();
    let webdav_creds = {
        let c = config.borrow();
        match (
            c.webdav_url.clone(),
            c.webdav_username.clone(),
            secret_store::load_webdav_password(),
        ) {
            (Some(url), Some(user), Some(pass)) if !url.is_empty() => Some((url, user, pass)),
            _ => None,
        }
    };
    let message = glib::DateTime::now_local()
        .ok()
        .and_then(|d| d.format("Auto-backup %Y-%m-%d %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Auto-backup".to_string());

    let (sender, receiver) = worker::channel::<Result<(), String>>();
    std::thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let vault = fond_vault::Vault::open(&root)
                .or_else(|_| fond_vault::Vault::init(&root))
                .map_err(|e| e.to_string())?;
            let status = vault.status().map_err(|e| e.to_string())?;
            if status.is_clean() {
                return Ok(());
            }
            let identity = fond_vault::Identity::from_git_config().map_err(|e| e.to_string())?;
            vault.stage_all().map_err(|e| e.to_string())?;
            vault
                .commit(&message, &identity)
                .map_err(|e| e.to_string())?;

            if let Some(token) = github_token {
                if vault.remote_url("origin").is_some() {
                    vault
                        .push_github("origin", &token)
                        .map_err(|e| format!("committed locally, but GitHub push failed: {e}"))?;
                }
            }
            if let Some((url, user, pass)) = webdav_creds {
                webdav::upload_library(&url, &user, &pass, &root)
                    .map_err(|e| format!("committed locally, but WebDAV backup failed: {e}"))?;
            }
            Ok(())
        })();
        let _ = sender.send(result);
    });

    let widgets = widgets.clone();
    receiver.attach(move |result| {
        if let Err(e) = result {
            toast(&widgets, &format!("Automatic backup: {e}"));
        }
        glib::ControlFlow::Break
    });
}

/// Configure automatic backups: a single switch plus an interval picker. Reuses whatever
/// GitHub sign-in / WebDAV credentials are already set up elsewhere — there is nothing else to
/// configure here, by design ("very very easy").
#[allow(deprecated)]
fn show_auto_backup_dialog(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    config: &Rc<RefCell<Config>>,
    timer: &Rc<RefCell<Option<glib::SourceId>>>,
) {
    let dialog = adw::Window::new();
    dialog.set_title(Some("Automatic backups"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(420, -1);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let save = gtk4::Button::with_label("Save");
    save.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&save);
    view.add_top_bar(&header);

    let content = gtk4::Box::new(Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let intro = gtk4::Label::new(Some(
        "While a library is open, commit any changes on this schedule. If already signed in \
         to GitHub with a repo configured, also push; if WebDAV is set up, also mirror there.",
    ));
    intro.set_wrap(true);
    intro.set_xalign(0.0);
    content.append(&intro);

    let enabled_row = gtk4::Box::new(Orientation::Horizontal, 8);
    let enabled_label = gtk4::Label::new(Some("Enable automatic backups"));
    enabled_label.set_xalign(0.0);
    enabled_label.set_hexpand(true);
    enabled_label.set_halign(gtk4::Align::Start);
    let enabled_switch = gtk4::Switch::new();
    enabled_switch.set_halign(gtk4::Align::End);
    enabled_switch.set_active(config.borrow().auto_backup_enabled);
    enabled_row.append(&enabled_label);
    enabled_row.append(&enabled_switch);
    content.append(&enabled_row);

    let interval_labels: Vec<&str> = AUTO_BACKUP_INTERVALS.iter().map(|(l, _)| *l).collect();
    let interval_drop = gtk4::DropDown::from_strings(&interval_labels);
    let current_minutes = {
        let m = config.borrow().auto_backup_interval_mins;
        if m == 0 {
            30
        } else {
            m
        }
    };
    let selected = AUTO_BACKUP_INTERVALS
        .iter()
        .position(|(_, m)| *m == current_minutes)
        .unwrap_or(1);
    interval_drop.set_selected(selected as u32);
    content.append(&labeled("Interval", &interval_drop));

    view.set_content(Some(&content));
    dialog.set_content(Some(&view));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let config = config.clone();
        let timer = timer.clone();
        let dialog = dialog.clone();
        let enabled_switch = enabled_switch.clone();
        let interval_drop = interval_drop.clone();
        save.connect_clicked(move |_| {
            let minutes = AUTO_BACKUP_INTERVALS
                .get(interval_drop.selected() as usize)
                .map(|(_, m)| *m)
                .unwrap_or(30);
            {
                let mut c = config.borrow_mut();
                c.auto_backup_enabled = enabled_switch.is_active();
                c.auto_backup_interval_mins = minutes;
                c.save();
            }
            start_auto_backup_timer(&state, &widgets, &config, &timer);
            toast(
                &widgets,
                if enabled_switch.is_active() {
                    "Automatic backups on"
                } else {
                    "Automatic backups off"
                },
            );
            dialog.close();
        });
    }

    dialog.present();
}

fn reload_current(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    let path = state
        .borrow()
        .library
        .as_ref()
        .map(|l| l.root().to_path_buf());
    if let Some(path) = path {
        open_library(state, widgets, path);
    }
}

/// Select the entry with citation key `key` in the list, revealing it first (clearing the
/// search and collection filter) if it is currently filtered out. No-op with a toast if the
/// key is not in the library.
/// Select the row for `key` in the (possibly sorted) spreadsheet, if it's currently shown, and
/// refresh the detail pane for it. Returns whether it was found.
fn select_visible_key(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, key: &str) -> bool {
    for i in 0..widgets.selection.n_items() {
        if let Some(row) = widgets.selection.item(i).and_downcast::<EntryRow>() {
            if row.key() == key {
                widgets.selection.set_selected(i);
                show_detail(state, widgets, row.idx());
                return true;
            }
        }
    }
    false
}

/// Select the entry with citation key `key` in the list, revealing it first (clearing the
/// search and collection filter) if it is currently filtered out. No-op with a toast if the
/// key is not in the library.
fn select_key(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, key: &str) {
    if select_visible_key(state, widgets, key) {
        return;
    }

    if !state.borrow().key_to_index.contains_key(key) {
        toast(widgets, "That entry is not in this library");
        return;
    }

    // Filtered out — reset the view so it becomes visible.
    {
        let mut s = state.borrow_mut();
        s.collection_filter = None;
        s.query.clear();
    }
    widgets.search.set_text("");
    refresh_list(state, widgets);
    select_visible_key(state, widgets, key);
}

/// Modal dialog to acquire a reference by DOI / arXiv / ISBN. The network lookup runs on a
/// worker thread; the result is applied on the main thread so the UI never blocks.
// The glib main-context channel is deprecated in favour of async-channel; it remains the
// simplest thread→main-loop bridge here and is still supported. Migrate when the UI adopts
// async futures.
#[allow(deprecated)]
fn show_acquire_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    if state.borrow().library.is_none() {
        toast(widgets, "Open a library first");
        return;
    }

    let dialog = adw::Window::new();
    dialog.set_title(Some("Acquire reference"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(460, -1);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let add = gtk4::Button::with_label("Add");
    add.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&add);
    view.add_top_bar(&header);

    let content = gtk4::Box::new(Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let kinds = gtk4::StringList::new(&["DOI", "arXiv", "ISBN"]);
    let dropdown = gtk4::DropDown::builder().model(&kinds).build();
    let entry = gtk4::Entry::builder()
        .placeholder_text("e.g. 10.1000/xyz")
        .activates_default(true)
        .hexpand(true)
        .build();
    // Plain-language hint for whichever identifier kind is selected — "DOI"/"arXiv"/"ISBN"
    // mean nothing to most people on sight, and the dropdown alone doesn't explain them.
    let hint = gtk4::Label::new(None);
    hint.set_wrap(true);
    hint.set_xalign(0.0);
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    let update_hint: Rc<dyn Fn(AcquireKind)> = Rc::new({
        let hint = hint.clone();
        let entry = entry.clone();
        move |kind: AcquireKind| {
            let (text, placeholder) = match kind {
                AcquireKind::Doi => (
                    "A DOI is a permanent ID most journal articles have — often printed near \
                     the abstract or in the URL, like 10.1000/xyz.",
                    "e.g. 10.1000/xyz",
                ),
                AcquireKind::Arxiv => (
                    "For preprints from arxiv.org — the ID in the paper's URL, like 2101.00001.",
                    "e.g. 2101.00001",
                ),
                AcquireKind::Isbn => (
                    "The number under the barcode on the back of a book (10 or 13 digits).",
                    "e.g. 9780140449136",
                ),
            };
            hint.set_text(text);
            entry.set_placeholder_text(Some(placeholder));
        }
    });
    update_hint(AcquireKind::Doi);
    {
        let update_hint = update_hint.clone();
        dropdown.connect_selected_notify(move |d| {
            update_hint(match d.selected() {
                0 => AcquireKind::Doi,
                1 => AcquireKind::Arxiv,
                _ => AcquireKind::Isbn,
            });
        });
    }

    let spinner = gtk4::Spinner::new();
    spinner.set_halign(gtk4::Align::End);

    content.append(&dropdown);
    content.append(&entry);
    content.append(&hint);
    content.append(&spinner);
    view.set_content(Some(&content));
    dialog.set_content(Some(&view));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }

    {
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        let entry = entry.clone();
        let dropdown = dropdown.clone();
        let spinner = spinner.clone();
        let add = add.clone();
        add.connect_clicked(move |add| {
            let identifier = entry.text().trim().to_string();
            if identifier.is_empty() {
                return;
            }
            let kind = match dropdown.selected() {
                0 => AcquireKind::Doi,
                1 => AcquireKind::Arxiv,
                _ => AcquireKind::Isbn,
            };

            add.set_sensitive(false);
            entry.set_sensitive(false);
            spinner.start();

            // (is_bibtex, payload) on success; error string otherwise.
            let (sender, receiver) = worker::channel::<Result<(bool, String), String>>();
            std::thread::spawn(move || {
                let result = match kind {
                    AcquireKind::Doi => {
                        fond_bib::acquire::fetch_doi_bibtex(&identifier).map(|s| (true, s))
                    }
                    AcquireKind::Arxiv => {
                        fond_bib::acquire::fetch_arxiv_bibtex(&identifier).map(|s| (true, s))
                    }
                    AcquireKind::Isbn => {
                        fond_bib::acquire::fetch_isbn_yaml(&identifier).map(|s| (false, s))
                    }
                }
                .map_err(|e| e.to_string());
                let _ = sender.send(result);
            });

            let state = state.clone();
            let widgets = widgets.clone();
            let dialog = dialog.clone();
            let entry = entry.clone();
            let spinner = spinner.clone();
            let add = add.clone();
            receiver.attach(move |result| {
                spinner.stop();
                match result {
                    Ok((is_bibtex, payload)) => {
                        let added = {
                            let s = state.borrow();
                            let library = s.library.as_ref().expect("library open");
                            if is_bibtex {
                                library.add_bibtex(&payload)
                            } else {
                                library.add_from_yaml(&payload)
                            }
                        };
                        match added {
                            Ok(keys) => {
                                toast(&widgets, &format!("Added {}", keys.join(", ")));
                                dialog.close();
                                reload_current(&state, &widgets);
                            }
                            Err(e) => {
                                toast(&widgets, &friendly::bib_error(&e));
                                add.set_sensitive(true);
                                entry.set_sensitive(true);
                            }
                        }
                    }
                    Err(e) => {
                        toast(
                            &widgets,
                            &format!("Couldn't find that reference online — double-check the identifier and try again ({e})."),
                        );
                        add.set_sensitive(true);
                        entry.set_sensitive(true);
                    }
                }
                glib::ControlFlow::Break
            });
        });
    }

    dialog.present();
}

fn open_library(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, path: PathBuf) {
    let library = match Library::open(&path) {
        Ok(lib) => lib,
        Err(e) => {
            toast(widgets, &friendly::bib_error(&e));
            return;
        }
    };

    // Recorded here rather than at each caller (folder picker, new-library dialog, "Move
    // library…", startup restore) — a single point that only fires once the library has
    // actually opened successfully.
    widgets.config.borrow_mut().record_recent_library(&path);
    widgets.config.borrow().save();

    let defs = library.load_custom_field_defs().unwrap_or_default();
    let config = widgets.config.borrow().clone();
    sync_custom_field_columns(
        &widgets.column_view,
        &widgets.custom_columns,
        &defs,
        &config,
    );

    let mut entries = Vec::new();
    match library.keys_sorted() {
        Ok(keys) => {
            for key in keys {
                if let Ok(parsed) = library.load_entry(&key) {
                    let note = library.load_note(&key).ok().flatten();
                    let (has_pdf, has_epub) = attachment_presence(&library, note.as_ref());
                    let tags = note
                        .as_ref()
                        .map(|n| n.frontmatter.tags.join(", "))
                        .unwrap_or_default();
                    let status = note
                        .as_ref()
                        .and_then(|n| n.frontmatter.read_status)
                        .map(|s| match s {
                            fond_bib::ReadStatus::Unread => "unread",
                            fond_bib::ReadStatus::Reading => "reading",
                            fond_bib::ReadStatus::Read => "read",
                        })
                        .unwrap_or_default()
                        .to_string();
                    let custom_fields = note
                        .as_ref()
                        .map(|n| n.frontmatter.custom_fields.clone())
                        .unwrap_or_default();
                    entries.push(EntrySummary {
                        author: bibentry::author_names(&parsed.entry),
                        year: bibentry::year(&parsed.entry)
                            .map(|y| y.to_string())
                            .unwrap_or_default(),
                        title: bibentry::title_string(&parsed.entry).unwrap_or_default(),
                        key,
                        has_pdf,
                        has_epub,
                        tags,
                        status,
                        custom_fields,
                        entry_type: format!("{:?}", parsed.entry.entry_type()).to_lowercase(),
                        isbn: parsed.entry.isbn().unwrap_or_default().to_string(),
                    });
                }
            }
        }
        Err(e) => {
            toast(widgets, &friendly::bib_error(&e));
            return;
        }
    }

    let count = entries.len();
    let key_to_index: HashMap<String, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.key.clone(), i))
        .collect();

    // Search index: open an existing one if present (cheap), else build it once. Rebuilding
    // on every open would re-index the whole library at each launch — the menu's "Reindex
    // search" refreshes it on demand. Search falls back to a substring filter if unavailable.
    let index_dir = library.root().join(".kartoteka").join("index");
    // Open an existing index if present (cheap). An index left by an older build can have an
    // out-of-date schema (e.g. pre-nodes, missing `kind`) — `open` reports that as an error
    // rather than crashing, and we rebuild once from the authoritative files. A missing index
    // also falls through to the one-time build. Search falls back to a substring filter if all
    // of this fails.
    let index = match fond_index::SearchIndex::open(&index_dir) {
        Ok(idx) => Some(idx),
        Err(_) => {
            match fond_index::SearchIndex::rebuild(&library, &index_dir, |_| None, |_| None) {
                Ok(idx) => Some(idx),
                Err(e) => {
                    toast(widgets, &format!("Search index unavailable: {e}"));
                    None
                }
            }
        }
    };

    let saved_searches = load_saved_searches(&path);
    {
        let mut s = state.borrow_mut();
        s.library = Some(library);
        s.entries = entries;
        s.key_to_index = key_to_index;
        s.index = index;
        s.query.clear();
        s.collection_filter = None;
        s.saved_searches = saved_searches;
    }
    widgets.subtitle.set_subtitle(&format!(
        "{} — {count} entries",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("library")
    ));
    widgets.status_label.set_text(&path.display().to_string());
    widgets.content_stack.set_visible_child_name("library");
    refresh_collections(state, widgets);
    refresh_list(state, widgets);
}

/// Pick an existing folder and open it as a library — the shared behaviour behind both the
/// header's folder icon and the "Open existing library…" button on the first-run page.
fn open_library_picker(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    config: &Rc<RefCell<Config>>,
) {
    let dialog = gtk4::FileDialog::builder().title("Open library").build();
    let state = state.clone();
    let widgets = widgets.clone();
    let config = config.clone();
    let parent = widgets.window.clone();
    dialog.select_folder(Some(&parent), gio::Cancellable::NONE, move |result| {
        if let Ok(folder) = result {
            if let Some(path) = folder.path() {
                config.borrow_mut().library_path = Some(path.clone());
                config.borrow().save();
                open_library(&state, &widgets, path);
            }
        }
    });
}

/// Create a brand-new library: a name and a location (a folder to create it in), so a
/// first-time user doesn't have to go create an empty folder themselves before Kartoteka
/// will let them start. Defaults the location to `~/Documents` when available.
fn show_new_library_dialog(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    config: &Rc<RefCell<Config>>,
) {
    let dialog = adw::Window::new();
    dialog.set_title(Some("New library"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(440, -1);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let create = gtk4::Button::with_label("Create");
    create.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&create);
    view.add_top_bar(&header);

    let content = gtk4::Box::new(Orientation::Vertical, 10);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let intro = gtk4::Label::new(Some(
        "This creates a new folder to hold your library — its references, notes, and PDFs \
         all live together inside it.",
    ));
    intro.set_wrap(true);
    intro.set_xalign(0.0);
    intro.add_css_class("dim-label");
    content.append(&intro);

    let name_entry = gtk4::Entry::builder()
        .text("My Library")
        .activates_default(true)
        .build();
    content.append(&labeled("Name", &name_entry));

    let default_location =
        glib::user_special_dir(glib::UserDirectory::Documents).unwrap_or_else(glib::home_dir);
    let location: Rc<RefCell<PathBuf>> = Rc::new(RefCell::new(default_location.clone()));
    let location_row = gtk4::Box::new(Orientation::Horizontal, 8);
    let location_label = gtk4::Label::new(Some(&default_location.display().to_string()));
    location_label.set_hexpand(true);
    location_label.set_xalign(0.0);
    location_label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
    location_label.add_css_class("dim-label");
    let choose_location = gtk4::Button::with_label("Choose…");
    location_row.append(&location_label);
    location_row.append(&choose_location);
    content.append(&labeled("Location", &location_row));

    {
        let location = location.clone();
        let location_label = location_label.clone();
        let window = widgets.window.clone();
        choose_location.connect_clicked(move |_| {
            let dialog = gtk4::FileDialog::builder()
                .title("Choose a location")
                .build();
            let location = location.clone();
            let location_label = location_label.clone();
            dialog.select_folder(Some(&window), gio::Cancellable::NONE, move |result| {
                if let Ok(folder) = result {
                    if let Some(path) = folder.path() {
                        location_label.set_text(&path.display().to_string());
                        *location.borrow_mut() = path;
                    }
                }
            });
        });
    }

    view.set_content(Some(&content));
    dialog.set_content(Some(&view));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let config = config.clone();
        let dialog = dialog.clone();
        create.connect_clicked(move |_| {
            let name = name_entry.text().trim().to_string();
            if name.is_empty() {
                toast(&widgets, "Give the library a name");
                return;
            }
            let root = location.borrow().join(&name);
            if root.exists() {
                toast(
                    &widgets,
                    &format!(
                        "\"{}\" already exists — pick a different name or location",
                        name
                    ),
                );
                return;
            }
            match fond_bib::Library::init(&root) {
                Ok(_) => {
                    config.borrow_mut().library_path = Some(root.clone());
                    config.borrow().save();
                    open_library(&state, &widgets, root);
                    dialog.close();
                }
                Err(e) => toast(&widgets, &friendly::bib_error(&e)),
            }
        });
    }

    dialog.present();
}

/// Saved searches are stored per library under `.kartoteka/saved-searches.json`.
fn load_saved_searches(root: &std::path::Path) -> Vec<(String, String)> {
    let path = root.join(".kartoteka").join("saved-searches.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_saved_searches(state: &Rc<RefCell<AppState>>) {
    let s = state.borrow();
    let Some(root) = s.library.as_ref().map(|l| l.root().to_path_buf()) else {
        return;
    };
    let dir = root.join(".kartoteka");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_string_pretty(&s.saved_searches) {
        let _ = std::fs::write(dir.join("saved-searches.json"), json);
    }
}

/// Depth-first sidebar order for a set of collections — top-level first, each one
/// immediately followed by its own children, `(slug, name, depth)`. A `parent` that names an
/// unknown slug, or that would form a cycle, is treated as top-level rather than dropping the
/// collection from the sidebar or looping forever — a hand-edited file is the only way either
/// case would happen, and it should still render as *something*.
fn order_collection_tree(
    collections: &[(String, fond_bib::Collection)],
) -> Vec<(String, String, usize)> {
    let known: HashSet<&str> = collections.iter().map(|(slug, _)| slug.as_str()).collect();
    let mut children: HashMap<Option<String>, Vec<usize>> = HashMap::new();
    for (i, (slug, coll)) in collections.iter().enumerate() {
        let parent = coll
            .parent
            .as_ref()
            .filter(|p| known.contains(p.as_str()) && p.as_str() != slug.as_str())
            .cloned();
        children.entry(parent).or_default().push(i);
    }

    fn walk(
        parent: Option<&str>,
        depth: usize,
        collections: &[(String, fond_bib::Collection)],
        children: &HashMap<Option<String>, Vec<usize>>,
        visited: &mut [bool],
        out: &mut Vec<(String, String, usize)>,
    ) {
        let Some(idxs) = children.get(&parent.map(str::to_string)) else {
            return;
        };
        for &i in idxs {
            if visited[i] {
                continue;
            }
            visited[i] = true;
            let (slug, coll) = &collections[i];
            out.push((slug.clone(), coll.name.clone(), depth));
            walk(Some(slug), depth + 1, collections, children, visited, out);
        }
    }

    let mut visited = vec![false; collections.len()];
    let mut out = Vec::with_capacity(collections.len());
    walk(None, 0, collections, &children, &mut visited, &mut out);
    // A collection unreachable from the root (only possible via a cycle with no member
    // marked top-level, e.g. a's parent is b and b's parent is a) still needs to appear
    // somewhere instead of silently vanishing from the sidebar.
    for (i, (slug, coll)) in collections.iter().enumerate() {
        if !visited[i] {
            out.push((slug.clone(), coll.name.clone(), 0));
        }
    }
    out
}

/// Rebuild the collections list: "All entries", each collection (nested under its parent, if
/// any), then saved searches. Collection rows accept a drag of an entry's citation key
/// (dragged from the entries list, see `make_row`) to add that entry to the collection.
fn refresh_collections(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    let slugs = state
        .borrow()
        .library
        .as_ref()
        .and_then(|l| l.collection_slugs().ok())
        .unwrap_or_default();

    let mut loaded: Vec<(String, fond_bib::Collection)> = Vec::new();
    {
        let s = state.borrow();
        if let Some(lib) = s.library.as_ref() {
            for slug in &slugs {
                let coll = lib.load_collection(slug).unwrap_or_default();
                loaded.push((slug.clone(), coll));
            }
        }
    }
    let ordered = order_collection_tree(&loaded);
    state.borrow_mut().collections = ordered.iter().map(|(slug, _, _)| slug.clone()).collect();

    let lb = &widgets.collections_listbox;
    while let Some(child) = lb.first_child() {
        lb.remove(&child);
    }
    lb.append(&collection_row("All entries", "view-list-symbolic", 0));
    for (slug, name, depth) in &ordered {
        let row = collection_row(name, "folder-symbolic", *depth);
        unsafe { row.set_data("collection-slug", slug.clone()) };
        {
            let state = state.clone();
            let widgets = widgets.clone();
            let slug = slug.clone();
            // Accepts two kinds of drag: a bare citation key (dragged from the entries list
            // or Bookshelf grid — adds that entry to this collection) and a
            // `"collection:<slug>"`-prefixed string (dragged from another collection row
            // below — reparents it under this one). Same `STRING` GType for both; the prefix
            // is the only thing disambiguating them, since `ContentProvider::for_value` on
            // the entry side is a bare key with no room to add a type tag without also
            // touching that call site.
            let drop = gtk4::DropTarget::new(glib::types::Type::STRING, gdk::DragAction::COPY);
            drop.connect_drop(move |_, value, _, _| {
                let Ok(text) = value.get::<String>() else {
                    return false;
                };
                let (result, moved_collection) = {
                    let s = state.borrow();
                    let Some(lib) = s.library.as_ref() else {
                        return false;
                    };
                    match text.strip_prefix("collection:") {
                        Some(dragged_slug) if dragged_slug == slug => (Ok(()), true),
                        Some(dragged_slug) => {
                            (lib.reparent_collection(dragged_slug, Some(&slug)), true)
                        }
                        None => (lib.add_to_collection(&slug, &text), false),
                    }
                };
                match result {
                    Ok(()) => {
                        if moved_collection {
                            refresh_collections(&state, &widgets);
                            toast(&widgets, "Moved collection");
                        } else {
                            refresh_list(&state, &widgets);
                            toast(&widgets, "Added to collection");
                        }
                        true
                    }
                    Err(e) => {
                        toast(&widgets, &friendly::bib_error(&e));
                        false
                    }
                }
            });
            row.add_controller(drop);
        }
        {
            // Drag source so a collection can be dropped onto another one above to reparent
            // it (see the `DropTarget` just above) — `MOVE` rather than `COPY` since
            // re-parenting isn't a copy of anything.
            let slug_for_drag = slug.clone();
            let drag = gtk4::DragSource::new();
            drag.set_actions(gdk::DragAction::MOVE);
            drag.connect_prepare(move |_, _, _| {
                Some(gdk::ContentProvider::for_value(
                    &format!("collection:{slug_for_drag}").to_value(),
                ))
            });
            row.add_controller(drag);
        }
        {
            let state = state.clone();
            let widgets = widgets.clone();
            let slug = slug.clone();
            let name = name.clone();
            let row_for_menu = row.clone();
            let click = gtk4::GestureClick::new();
            click.set_button(gdk::BUTTON_SECONDARY);
            click.connect_pressed(move |_gesture, _n, x, y| {
                show_collection_context_menu(&state, &widgets, &row_for_menu, &slug, &name, x, y);
            });
            row.add_controller(click);
        }
        lb.append(&row);
    }
    for (name, _) in &state.borrow().saved_searches {
        let row = collection_row(name, "folder-saved-search-symbolic", 0);
        unsafe { row.set_data("saved-search-name", name.clone()) };
        lb.append(&row);
    }
    // Select "All entries" without triggering a reload loop.
    if let Some(first) = lb.row_at_index(0) {
        lb.select_row(Some(&first));
    }
}

fn collection_row(label: &str, icon: &str, depth: usize) -> gtk4::ListBoxRow {
    let hbox = gtk4::Box::new(Orientation::Horizontal, 8);
    hbox.set_margin_top(5);
    hbox.set_margin_bottom(5);
    hbox.set_margin_start(8 + (depth as i32) * 16);
    hbox.set_margin_end(8);
    let image = gtk4::Image::from_icon_name(icon);
    let text = gtk4::Label::new(Some(label));
    text.add_css_class("fond-row-title");
    text.set_xalign(0.0);
    text.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    hbox.append(&image);
    hbox.append(&text);
    let row = gtk4::ListBoxRow::new();
    row.add_css_class("fond-row");
    row.set_child(Some(&hbox));
    row
}

/// Right-click menu on a real collection row (not "All entries" or a saved search):
/// Edit…, New collection…, Delete…. Same hand-built `popover_menu`/`popover_button`
/// pattern the PDF reader's own right-click menus use (`show_pdf_context_menu`).
fn show_collection_context_menu(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    row: &gtk4::ListBoxRow,
    slug: &str,
    name: &str,
    x: f64,
    y: f64,
) {
    let (popover, rows) = popover_menu(200);
    popover.set_parent(row);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(
        x.round() as i32,
        y.round() as i32,
        1,
        1,
    )));
    popover.set_has_arrow(true);

    let edit_row = popover_button("Edit…", false);
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let popover = popover.clone();
        let slug = slug.to_string();
        edit_row.connect_clicked(move |_| {
            popover.popdown();
            edit_collection_dialog(&state, &widgets, &slug);
        });
    }
    rows.append(&edit_row);

    let new_row = popover_button("New collection…", false);
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let popover = popover.clone();
        new_row.connect_clicked(move |_| {
            popover.popdown();
            new_collection_dialog(&state, &widgets);
        });
    }
    rows.append(&new_row);

    let delete_row = popover_button("Delete…", true);
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let popover = popover.clone();
        let slug = slug.to_string();
        let name = name.to_string();
        delete_row.connect_clicked(move |_| {
            popover.popdown();
            confirm_delete_collection(&state, &widgets, &slug, &name);
        });
    }
    rows.append(&delete_row);

    popover.popup();
}

/// Rename and/or reparent an existing collection — the "beefed up" collection-editing
/// surface, reachable from the sidebar's right-click menu. Reuses `new_collection_dialog`'s
/// parent-dropdown construction, but excludes the collection itself and any of its current
/// descendants (`Library::would_create_cycle` covers both) — `reparent_collection` enforces
/// the same rule server-side, this just keeps the dropdown from offering an option that's
/// guaranteed to be rejected.
fn edit_collection_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, slug: &str) {
    let Some(lib) = state.borrow().library.clone() else {
        return;
    };
    let Ok(current) = lib.load_collection(slug) else {
        toast(widgets, "Could not load that collection");
        return;
    };

    let dialog = adw::Window::new();
    dialog.set_title(Some("Edit collection"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(380, -1);
    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let save = gtk4::Button::with_label("Save");
    save.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&save);
    view.add_top_bar(&header);

    let content = gtk4::Box::new(Orientation::Vertical, 8);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    let entry = gtk4::Entry::builder()
        .placeholder_text("Collection name")
        .text(&current.name)
        .activates_default(true)
        .build();
    content.append(&entry);

    let (parent_slugs, parent_labels, selected_index) = {
        let slugs = lib.collection_slugs().unwrap_or_default();
        let loaded: Vec<(String, fond_bib::Collection)> = slugs
            .into_iter()
            .map(|s| {
                let coll = lib.load_collection(&s).unwrap_or_default();
                (s, coll)
            })
            .collect();
        let ordered = order_collection_tree(&loaded);
        let mut slugs = vec![String::new()];
        let mut labels = vec!["(top level)".to_string()];
        for (s, name, depth) in ordered {
            if s == slug || lib.would_create_cycle(slug, Some(&s)) {
                continue;
            }
            slugs.push(s);
            labels.push(format!("{}{}", "    ".repeat(depth), name));
        }
        let selected = current
            .parent
            .as_deref()
            .and_then(|p| slugs.iter().position(|s| s == p))
            .unwrap_or(0);
        (slugs, labels, selected)
    };
    let parent_label_refs: Vec<&str> = parent_labels.iter().map(String::as_str).collect();
    let parent_drop = gtk4::DropDown::from_strings(&parent_label_refs);
    parent_drop.set_tooltip_text(Some("Parent collection (optional)"));
    parent_drop.set_selected(selected_index as u32);
    content.append(&parent_drop);

    view.set_content(Some(&content));
    dialog.set_content(Some(&view));
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        let slug = slug.to_string();
        save.connect_clicked(move |_| {
            let name = entry.text().trim().to_string();
            if name.is_empty() {
                toast(&widgets, "Name can't be empty");
                return;
            }
            let new_parent = parent_slugs
                .get(parent_drop.selected() as usize)
                .filter(|s| !s.is_empty())
                .cloned();
            let result = {
                let s = state.borrow();
                let Some(lib) = s.library.as_ref() else {
                    return;
                };
                lib.rename_collection(&slug, &name)
                    .and_then(|()| lib.reparent_collection(&slug, new_parent.as_deref()))
            };
            match result {
                Ok(()) => {
                    toast(&widgets, "Collection updated");
                    dialog.close();
                    refresh_collections(&state, &widgets);
                }
                Err(e) => toast(&widgets, &friendly::bib_error(&e)),
            }
        });
    }
    dialog.present();
}

/// Confirm and then delete a collection. Its child collections (if any) are promoted to top
/// level rather than deleted with it (see `Library::delete_collection`) — only the entries'
/// *membership* in this collection is discarded; the entries themselves are untouched.
fn confirm_delete_collection(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    slug: &str,
    name: &str,
) {
    let dialog = adw::MessageDialog::new(
        Some(&widgets.window),
        Some(&format!("Delete “{name}”?")),
        Some(
            "Entries stay in the library and any subcollections move to the top level — only \
             this collection itself is removed.",
        ),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let state = state.clone();
    let widgets = widgets.clone();
    let slug = slug.to_string();
    dialog.connect_response(None, move |dlg, response| {
        dlg.close();
        if response != "delete" {
            return;
        }
        let result = {
            let s = state.borrow();
            s.library.as_ref().map(|lib| lib.delete_collection(&slug))
        };
        match result {
            Some(Ok(())) => {
                toast(&widgets, "Collection deleted");
                refresh_collections(&state, &widgets);
                refresh_list(&state, &widgets);
            }
            Some(Err(e)) => toast(&widgets, &friendly::bib_error(&e)),
            None => {}
        }
    });
    dialog.present();
}

/// Append one card (title/key list + Merge button) per duplicate group to `list`.
fn append_duplicate_cards(
    list: &gtk4::Box,
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    groups: &[Vec<String>],
) {
    for group in groups {
        let card = gtk4::Box::new(Orientation::Vertical, 4);
        card.add_css_class("card");
        card.set_margin_top(2);
        let inner = gtk4::Box::new(Orientation::Vertical, 4);
        inner.set_margin_top(8);
        inner.set_margin_bottom(8);
        inner.set_margin_start(10);
        inner.set_margin_end(10);
        {
            let s = state.borrow();
            let lib = s.library.as_ref().unwrap();
            for key in group {
                let title = lib
                    .load_entry(key)
                    .ok()
                    .and_then(|p| bibentry::title_string(&p.entry))
                    .unwrap_or_default();
                let lbl = gtk4::Label::new(Some(&format!("{title}  ·  {key}")));
                lbl.set_xalign(0.0);
                lbl.set_wrap(true);
                inner.append(&lbl);
            }
        }
        let merge = gtk4::Button::with_label(&format!("Merge into {}", group[0]));
        merge.add_css_class("suggested-action");
        merge.set_halign(gtk4::Align::Start);
        merge.set_margin_top(4);
        {
            let state = state.clone();
            let widgets = widgets.clone();
            let group = group.clone();
            merge.connect_clicked(move |btn| {
                let result = {
                    let s = state.borrow();
                    s.library
                        .as_ref()
                        .map(|lib| lib.merge_group(&group, &group[0]))
                };
                match result {
                    Some(Ok(())) => {
                        toast(&widgets, &format!("Merged into {}", group[0]));
                        btn.set_sensitive(false);
                        btn.set_label("Merged");
                        reload_current(&state, &widgets);
                    }
                    Some(Err(e)) => toast(&widgets, &format!("Merge failed: {e}")),
                    None => {}
                }
            });
        }
        inner.append(&merge);
        card.append(&inner);
        list.append(&card);
    }
}

/// List duplicate groups: exact matches (DOI/ISBN/title+year) plus, separately, "possible"
/// matches from title-similarity alone (`Library::find_duplicates_fuzzy`) — a typo or a
/// differently-punctuated subtitle the exact match misses. Each group gets its own Merge
/// button; there's nothing to "reject" a fuzzy suggestion beyond just not merging it.
fn show_duplicates_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    let (groups, fuzzy_groups) = {
        let s = state.borrow();
        let Some(library) = s.library.as_ref() else {
            toast(widgets, "Open a library first");
            return;
        };
        (
            library.find_duplicates().unwrap_or_default(),
            library.find_duplicates_fuzzy().unwrap_or_default(),
        )
    };
    if groups.is_empty() && fuzzy_groups.is_empty() {
        toast(widgets, "No duplicates found");
        return;
    }

    let dialog = adw::Window::new();
    dialog.set_title(Some(&format!(
        "Duplicates ({})",
        groups.len() + fuzzy_groups.len()
    )));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(520, 520);
    let view = adw::ToolbarView::new();
    let bare_header = adw::HeaderBar::new();
    bare_header.add_css_class("fond-chrome");
    view.add_top_bar(&bare_header);

    let list = gtk4::Box::new(Orientation::Vertical, 12);
    list.set_margin_top(14);
    list.set_margin_bottom(14);
    list.set_margin_start(16);
    list.set_margin_end(16);

    append_duplicate_cards(&list, state, widgets, &groups);

    if !fuzzy_groups.is_empty() {
        let heading = gtk4::Label::new(Some("Possible duplicates"));
        heading.add_css_class("heading");
        heading.set_xalign(0.0);
        heading.set_margin_top(8);
        list.append(&heading);
        let sub = gtk4::Label::new(Some(
            "Titles are similar but didn't match exactly — check before merging.",
        ));
        sub.add_css_class("dim-label");
        sub.add_css_class("caption");
        sub.set_xalign(0.0);
        sub.set_wrap(true);
        list.append(&sub);
        append_duplicate_cards(&list, state, widgets, &fuzzy_groups);
    }

    let scroll = gtk4::ScrolledWindow::new();
    scroll.add_css_class("fond-ground");
    scroll.set_child(Some(&list));
    scroll.set_vexpand(true);
    view.set_content(Some(&scroll));
    dialog.set_content(Some(&view));
    dialog.present();
}

/// Manage tags library-wide: rename (merge) or delete each tag across all notes.
fn show_tags_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    let tags = {
        let s = state.borrow();
        let Some(library) = s.library.as_ref() else {
            toast(widgets, "Open a library first");
            return;
        };
        library.all_tags().unwrap_or_default()
    };
    if tags.is_empty() {
        toast(widgets, "No tags in this library yet");
        return;
    }

    let dialog = adw::Window::new();
    dialog.set_title(Some(&format!("Tags ({})", tags.len())));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(460, 520);
    let view = adw::ToolbarView::new();
    let bare_header = adw::HeaderBar::new();
    bare_header.add_css_class("fond-chrome");
    view.add_top_bar(&bare_header);

    let list = gtk4::ListBox::new();
    list.set_selection_mode(gtk4::SelectionMode::None);
    list.add_css_class("fond-list");
    list.set_margin_top(14);
    list.set_margin_bottom(14);
    list.set_margin_start(16);
    list.set_margin_end(16);

    let last = tags.len().saturating_sub(1);
    for (i, (tag, count)) in tags.iter().enumerate() {
        let hbox = gtk4::Box::new(Orientation::Horizontal, 8);
        hbox.set_margin_start(4);
        hbox.set_margin_end(4);
        let entry = gtk4::Entry::builder().text(tag).hexpand(true).build();
        let count_label = gtk4::Label::new(Some(&format!("{count}")));
        count_label.add_css_class("fond-row-meta");
        let apply = gtk4::Button::from_icon_name("emblem-ok-symbolic");
        apply.set_tooltip_text(Some("Rename / merge (empty = delete)"));
        {
            let state = state.clone();
            let widgets = widgets.clone();
            let original = tag.clone();
            let entry = entry.clone();
            apply.connect_clicked(move |_| {
                let new = entry.text().trim().to_string();
                let result = {
                    let s = state.borrow();
                    s.library
                        .as_ref()
                        .map(|lib| lib.rename_tag(&original, &new))
                };
                match result {
                    Some(Ok(n)) => {
                        toast(&widgets, &format!("Updated {n} entries"));
                        reload_current(&state, &widgets);
                    }
                    Some(Err(e)) => toast(&widgets, &format!("Failed: {e}")),
                    None => {}
                }
            });
        }
        hbox.append(&entry);
        hbox.append(&count_label);
        hbox.append(&apply);
        let row = gtk4::ListBoxRow::new();
        row.set_activatable(false);
        row.add_css_class("fond-card");
        row.add_css_class("fond-row");
        if i == 0 {
            row.add_css_class("fond-card-first");
        }
        if i == last {
            row.add_css_class("fond-card-last");
        }
        row.set_child(Some(&hbox));
        list.append(&row);
    }

    let scroll = gtk4::ScrolledWindow::new();
    scroll.add_css_class("fond-ground");
    scroll.set_child(Some(&list));
    scroll.set_vexpand(true);
    view.set_content(Some(&scroll));
    dialog.set_content(Some(&view));
    dialog.present();
}

/// A field name → its `CustomFieldType` in the fixed order the three-way dropdowns below use.
/// Self-referential slot for a rebuild closure a row's own button needs to call (see
/// `show_custom_fields_dialog`'s `populate_cell`) — same pattern as `RebuildNotesCell`.
type RebuildCell = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

const CUSTOM_FIELD_TYPES: &[(&str, fond_bib::CustomFieldType)] = &[
    ("Text", fond_bib::CustomFieldType::Text),
    ("Number", fond_bib::CustomFieldType::Number),
    ("Tag", fond_bib::CustomFieldType::Tag),
    ("Date", fond_bib::CustomFieldType::Date),
];

/// Manage library-wide custom fields: define a new one (name + type), or remove one that's
/// no longer wanted. A field defined here shows up — initially empty — on every entry's
/// detail pane (see `show_detail`); removing it here removes the row everywhere, but leaves
/// any values already saved sitting harmlessly in each entry's note frontmatter (so
/// recreating a field with the same name brings old values straight back).
fn show_custom_fields_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    if state.borrow().library.is_none() {
        toast(widgets, "Open a library first");
        return;
    }

    let dialog = adw::Window::new();
    dialog.set_title(Some("Custom fields"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(460, 520);
    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    view.add_top_bar(&header);

    let outer = gtk4::Box::new(Orientation::Vertical, 10);
    outer.set_margin_top(14);
    outer.set_margin_bottom(14);
    outer.set_margin_start(16);
    outer.set_margin_end(16);

    let subtitle = gtk4::Label::new(Some(
        "Fields you add here appear on every reference's detail pane. Choose Text for free \
         notes, Number for a numeric value, or Tag for comma-separated values (shown the \
         same way the built-in Tags field is).",
    ));
    subtitle.set_wrap(true);
    subtitle.set_xalign(0.0);
    subtitle.add_css_class("dim-label");
    subtitle.add_css_class("caption");
    outer.append(&subtitle);

    // Add-field row: name, type, add button.
    let add_row = gtk4::Box::new(Orientation::Horizontal, 8);
    let name_entry = gtk4::Entry::builder()
        .placeholder_text("Field name, e.g. Methodology")
        .hexpand(true)
        .build();
    let type_labels: Vec<&str> = CUSTOM_FIELD_TYPES.iter().map(|(l, _)| *l).collect();
    let type_drop = gtk4::DropDown::from_strings(&type_labels);
    let add_button = gtk4::Button::from_icon_name("list-add-symbolic");
    add_button.add_css_class("suggested-action");
    add_button.set_tooltip_text(Some("Add this field"));
    add_row.append(&name_entry);
    add_row.append(&type_drop);
    add_row.append(&add_button);
    outer.append(&add_row);

    let list = gtk4::ListBox::new();
    list.set_selection_mode(gtk4::SelectionMode::None);
    list.add_css_class("fond-list");
    let scroll = gtk4::ScrolledWindow::new();
    scroll.add_css_class("fond-ground");
    scroll.set_child(Some(&list));
    scroll.set_vexpand(true);
    outer.append(&scroll);

    view.set_content(Some(&outer));
    dialog.set_content(Some(&view));

    // Self-referential slot: a row's own delete button needs to trigger a fresh rebuild of
    // the list it lives in, but `populate` isn't done being built (and so can't be cloned
    // into its own row closures) until after this whole `Rc::new` call returns.
    let populate_cell: RebuildCell = Rc::new(RefCell::new(None));

    let populate: Rc<dyn Fn()> = Rc::new({
        let state = state.clone();
        let widgets = widgets.clone();
        let list = list.clone();
        let populate_cell = populate_cell.clone();
        move || {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            let defs = {
                let s = state.borrow();
                s.library
                    .as_ref()
                    .and_then(|lib| lib.load_custom_field_defs().ok())
                    .unwrap_or_default()
            };
            if defs.fields.is_empty() {
                let row = gtk4::ListBoxRow::new();
                row.set_selectable(false);
                row.set_activatable(false);
                let l = gtk4::Label::new(Some("No custom fields yet — add one above"));
                l.add_css_class("dim-label");
                l.set_margin_top(12);
                l.set_margin_bottom(12);
                row.set_child(Some(&l));
                list.append(&row);
                return;
            }
            let last = defs.fields.len().saturating_sub(1);
            for (i, def) in defs.fields.iter().enumerate() {
                let hbox = gtk4::Box::new(Orientation::Horizontal, 8);
                hbox.set_margin_start(4);
                hbox.set_margin_end(4);
                let name_label = gtk4::Label::new(Some(&def.name));
                name_label.set_xalign(0.0);
                name_label.set_hexpand(true);
                let type_label = gtk4::Label::new(Some(
                    CUSTOM_FIELD_TYPES
                        .iter()
                        .find(|(_, t)| *t == def.field_type)
                        .map(|(l, _)| *l)
                        .unwrap_or("?"),
                ));
                type_label.add_css_class("fond-row-meta");
                let delete = gtk4::Button::from_icon_name("user-trash-symbolic");
                delete.add_css_class("flat");
                delete.set_tooltip_text(Some("Remove this field"));
                {
                    let state = state.clone();
                    let widgets = widgets.clone();
                    let name = def.name.clone();
                    let populate_cell = populate_cell.clone();
                    delete.connect_clicked(move |_| {
                        let result = {
                            let s = state.borrow();
                            s.library.as_ref().map(|lib| {
                                let mut defs = lib.load_custom_field_defs().unwrap_or_default();
                                defs.fields.retain(|f| f.name != name);
                                lib.save_custom_field_defs(&defs)
                            })
                        };
                        match result {
                            Some(Ok(_)) => {
                                if let Some(p) = populate_cell.borrow().as_ref() {
                                    p();
                                }
                                reload_current(&state, &widgets);
                            }
                            Some(Err(e)) => toast(&widgets, &friendly::bib_error(&e)),
                            None => {}
                        }
                    });
                }
                hbox.append(&name_label);
                hbox.append(&type_label);
                hbox.append(&delete);
                let row = gtk4::ListBoxRow::new();
                row.set_activatable(false);
                row.add_css_class("fond-card");
                row.add_css_class("fond-row");
                if i == 0 {
                    row.add_css_class("fond-card-first");
                }
                if i == last {
                    row.add_css_class("fond-card-last");
                }
                row.set_child(Some(&hbox));
                list.append(&row);
            }
        }
    });
    *populate_cell.borrow_mut() = Some(populate.clone());
    populate();

    {
        let state = state.clone();
        let widgets = widgets.clone();
        let populate = populate.clone();
        let name_entry = name_entry.clone();
        let type_drop = type_drop.clone();
        add_button.connect_clicked(move |_| {
            let name = name_entry.text().trim().to_string();
            if name.is_empty() {
                toast(&widgets, "Give the field a name");
                return;
            }
            let field_type = CUSTOM_FIELD_TYPES[type_drop.selected() as usize].1;
            let result = {
                let s = state.borrow();
                s.library.as_ref().map(|lib| {
                    let mut defs = lib.load_custom_field_defs().unwrap_or_default();
                    if defs.fields.iter().any(|f| f.name == name) {
                        return Err(format!("\"{name}\" already exists"));
                    }
                    defs.fields.push(fond_bib::CustomFieldDef {
                        name: name.clone(),
                        field_type,
                    });
                    lib.save_custom_field_defs(&defs)
                        .map_err(|e| friendly::bib_error(&e))
                })
            };
            match result {
                Some(Ok(_)) => {
                    name_entry.set_text("");
                    populate();
                    reload_current(&state, &widgets);
                }
                Some(Err(e)) => toast(&widgets, &e),
                None => {}
            }
        });
    }

    dialog.present();
}

/// Show/hide the optional entries-spreadsheet columns: the built-in Tags/Status pair, plus
/// one per current custom field. Toggling saves to config immediately (small, infrequent
/// change — matches the theme toggle's `win.theme` handler) and flips the column's
/// visibility live via `column_by_id`, without needing a reload.
fn show_columns_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    if state.borrow().library.is_none() {
        toast(widgets, "Open a library first");
        return;
    }

    let dialog = adw::Window::new();
    dialog.set_title(Some("Columns"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(340, 420);
    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    view.add_top_bar(&header);

    let outer = gtk4::Box::new(Orientation::Vertical, 10);
    outer.set_margin_top(14);
    outer.set_margin_bottom(14);
    outer.set_margin_start(16);
    outer.set_margin_end(16);

    let subtitle = gtk4::Label::new(Some(
        "Key, Title, Author, Year, and Files always show. Turn on whichever of these you \
         want alongside them.",
    ));
    subtitle.set_wrap(true);
    subtitle.set_xalign(0.0);
    subtitle.add_css_class("dim-label");
    subtitle.add_css_class("caption");
    outer.append(&subtitle);

    let list = gtk4::ListBox::new();
    list.set_selection_mode(gtk4::SelectionMode::None);
    list.add_css_class("fond-list");
    outer.append(&list);

    view.set_content(Some(&outer));
    dialog.set_content(Some(&view));

    let defs = {
        let s = state.borrow();
        s.library
            .as_ref()
            .and_then(|lib| lib.load_custom_field_defs().ok())
            .unwrap_or_default()
    };

    let mut toggles: Vec<(String, String)> = vec![
        ("tags".to_string(), "Tags".to_string()),
        ("status".to_string(), "Status".to_string()),
    ];
    for def in &defs.fields {
        toggles.push((format!("custom:{}", def.name), def.name.clone()));
    }
    let last = toggles.len().saturating_sub(1);

    for (i, (id, label)) in toggles.into_iter().enumerate() {
        let row = gtk4::ListBoxRow::new();
        row.set_activatable(false);
        row.add_css_class("fond-card");
        row.add_css_class("fond-row");
        if i == 0 {
            row.add_css_class("fond-card-first");
        }
        if i == last {
            row.add_css_class("fond-card-last");
        }
        let hbox = gtk4::Box::new(Orientation::Horizontal, 8);
        hbox.set_margin_start(4);
        hbox.set_margin_end(4);
        let check = gtk4::CheckButton::with_label(&label);
        check.set_active(
            widgets
                .config
                .borrow()
                .column_visible
                .get(&id)
                .copied()
                .unwrap_or(false),
        );
        hbox.append(&check);
        row.set_child(Some(&hbox));
        list.append(&row);

        let widgets = widgets.clone();
        check.connect_toggled(move |c| {
            let active = c.is_active();
            widgets
                .config
                .borrow_mut()
                .column_visible
                .insert(id.clone(), active);
            widgets.config.borrow().save();
            if let Some(col) = column_by_id(&widgets.column_view, &id) {
                col.set_visible(active);
            }
        });
    }

    dialog.present();
}

/// Aggregate every note's `tasks:` into one library-wide view — a derived read (and
/// check/uncheck) over data that's authoritative per-entry (`notes/<key>.md`), matching
/// `fond_bib::note`'s own doc comment: "a global task view is a derived aggregation over
/// these." Undone tasks first (nearest due date first, no-due-date last), then done tasks.
fn show_global_tasks_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    struct GlobalTask {
        key: String,
        title: String,
        index: usize,
        task: fond_bib::Task,
    }
    let mut items: Vec<GlobalTask> = {
        let s = state.borrow();
        let Some(library) = s.library.as_ref() else {
            toast(widgets, "Open a library first");
            return;
        };
        let mut items = Vec::new();
        for e in &s.entries {
            let Ok(Some(note)) = library.load_note(&e.key) else {
                continue;
            };
            let title = if e.title.is_empty() {
                e.key.clone()
            } else {
                e.title.clone()
            };
            for (index, task) in note.frontmatter.tasks.into_iter().enumerate() {
                items.push(GlobalTask {
                    key: e.key.clone(),
                    title: title.clone(),
                    index,
                    task,
                });
            }
        }
        items
    };
    if items.is_empty() {
        toast(widgets, "No tasks anywhere in this library yet");
        return;
    }
    items.sort_by(|a, b| {
        a.task
            .done
            .cmp(&b.task.done)
            .then_with(|| a.task.due.cmp(&b.task.due))
    });

    let dialog = adw::Window::new();
    dialog.set_title(Some(&format!("Tasks ({})", items.len())));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(560, 600);
    let view = adw::ToolbarView::new();
    let bare_header = adw::HeaderBar::new();
    bare_header.add_css_class("fond-chrome");
    view.add_top_bar(&bare_header);

    let list = gtk4::ListBox::new();
    list.set_selection_mode(gtk4::SelectionMode::None);
    list.add_css_class("fond-list");
    list.set_margin_top(12);
    list.set_margin_bottom(12);
    list.set_margin_start(12);
    list.set_margin_end(12);

    let last = items.len().saturating_sub(1);
    for (i, item) in items.into_iter().enumerate() {
        let hbox = gtk4::Box::new(Orientation::Horizontal, 8);
        hbox.set_margin_top(6);
        hbox.set_margin_bottom(6);
        hbox.set_margin_start(8);
        hbox.set_margin_end(8);

        let done = gtk4::CheckButton::new();
        done.set_active(item.task.done);
        hbox.append(&done);

        let text = gtk4::Box::new(Orientation::Vertical, 0);
        text.set_hexpand(true);
        let task_label = gtk4::Label::new(Some(&item.task.text));
        task_label.add_css_class("fond-row-title");
        task_label.set_xalign(0.0);
        task_label.set_halign(gtk4::Align::Start);
        task_label.set_wrap(true);
        let meta_text = match &item.task.due {
            Some(due) => format!("{} · due {}", item.title, due),
            None => item.title.clone(),
        };
        let meta_label = gtk4::Label::new(Some(&meta_text));
        meta_label.add_css_class("fond-row-meta");
        meta_label.set_xalign(0.0);
        meta_label.set_halign(gtk4::Align::Start);
        text.append(&task_label);
        text.append(&meta_label);
        hbox.append(&text);

        let goto = gtk4::Button::with_label("Go to entry");
        {
            let state = state.clone();
            let widgets = widgets.clone();
            let dialog_weak = dialog.downgrade();
            let key = item.key.clone();
            goto.connect_clicked(move |_| {
                select_key(&state, &widgets, &key);
                if let Some(d) = dialog_weak.upgrade() {
                    d.close();
                }
            });
        }
        hbox.append(&goto);

        let row = gtk4::ListBoxRow::new();
        row.set_activatable(false);
        row.add_css_class("fond-card");
        row.add_css_class("fond-row");
        if i == 0 {
            row.add_css_class("fond-card-first");
        }
        if i == last {
            row.add_css_class("fond-card-last");
        }
        row.set_child(Some(&hbox));
        list.append(&row);

        // Toggling saves straight back to that task's own note — this view is a lens over
        // authoritative per-entry data, not a copy of it.
        {
            let state = state.clone();
            let widgets = widgets.clone();
            let key = item.key.clone();
            let index = item.index;
            done.connect_toggled(move |c| {
                let result = {
                    let s = state.borrow();
                    s.library.as_ref().map(|lib| {
                        let mut note = lib.load_note(&key).ok().flatten().unwrap_or_default();
                        if let Some(t) = note.frontmatter.tasks.get_mut(index) {
                            t.done = c.is_active();
                        }
                        lib.write_note(&key, &note)
                    })
                };
                if let Some(Err(e)) = result {
                    toast(&widgets, &friendly::bib_error(&e));
                }
            });
        }
    }

    let scroll = gtk4::ScrolledWindow::new();
    scroll.add_css_class("fond-ground");
    scroll.set_child(Some(&list));
    scroll.set_vexpand(true);
    view.set_content(Some(&scroll));
    dialog.set_content(Some(&view));
    dialog.present();
}

/// Offer an entry's `ai/<key>.yml` keywords as tags to add. Checking a keyword and saving
/// appends it to the note's `tags:` — a one-directional, user-triggered write; nothing here
/// ever touches the AI sidecar itself (`docs/M2-SPEC.md` §4's boundary rule).
fn show_promote_keywords_dialog(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    key: &str,
    keywords: Vec<String>,
) {
    let existing_tags: Vec<String> = {
        let s = state.borrow();
        s.library
            .as_ref()
            .and_then(|lib| lib.load_note(key).ok().flatten())
            .map(|n| n.frontmatter.tags)
            .unwrap_or_default()
    };

    let dialog = adw::Window::new();
    dialog.set_title(Some("AI keywords"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(380, -1);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let add = gtk4::Button::with_label("Add as tags");
    add.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&add);
    view.add_top_bar(&header);

    let list = gtk4::ListBox::new();
    list.set_selection_mode(gtk4::SelectionMode::None);
    list.add_css_class("fond-list");
    list.set_margin_top(12);
    list.set_margin_bottom(12);
    list.set_margin_start(12);
    list.set_margin_end(12);

    let checks: Vec<(String, gtk4::CheckButton)> = keywords
        .iter()
        .map(|kw| {
            let already = existing_tags.iter().any(|t| t == kw);
            let hbox = gtk4::Box::new(Orientation::Horizontal, 8);
            hbox.set_margin_top(4);
            hbox.set_margin_bottom(4);
            hbox.set_margin_start(8);
            hbox.set_margin_end(8);
            let check = gtk4::CheckButton::with_label(kw);
            check.set_active(!already);
            check.set_sensitive(!already);
            hbox.append(&check);
            if already {
                let note = gtk4::Label::new(Some("already a tag"));
                note.add_css_class("fond-row-meta");
                hbox.append(&note);
            }
            let row = gtk4::ListBoxRow::new();
            row.add_css_class("fond-row");
            row.set_activatable(false);
            row.set_child(Some(&hbox));
            list.append(&row);
            (kw.clone(), check)
        })
        .collect();

    let scroll = gtk4::ScrolledWindow::new();
    scroll.add_css_class("fond-ground");
    scroll.set_child(Some(&list));
    view.set_content(Some(&scroll));
    dialog.set_content(Some(&view));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        let key = key.to_string();
        add.connect_clicked(move |_| {
            let chosen: Vec<String> = checks
                .iter()
                .filter(|(_, c)| c.is_active() && c.is_sensitive())
                .map(|(kw, _)| kw.clone())
                .collect();
            if chosen.is_empty() {
                dialog.close();
                return;
            }
            let result = {
                let s = state.borrow();
                s.library.as_ref().map(|lib| {
                    let mut note = lib.load_note(&key).ok().flatten().unwrap_or_default();
                    for kw in &chosen {
                        if !note.frontmatter.tags.iter().any(|t| t == kw) {
                            note.frontmatter.tags.push(kw.clone());
                        }
                    }
                    lib.write_note(&key, &note)
                })
            };
            match result {
                Some(Ok(_)) => {
                    toast(&widgets, &format!("Added {} tag(s)", chosen.len()));
                    dialog.close();
                    refresh_detail(&state, &widgets);
                }
                Some(Err(e)) => toast(&widgets, &friendly::bib_error(&e)),
                None => {}
            }
        });
    }

    dialog.present();
}

fn open_uri(window: &adw::ApplicationWindow, uri: &str) {
    let launcher = gtk4::UriLauncher::new(uri);
    launcher.launch(Some(window), gio::Cancellable::NONE, |_| {});
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Toggle an entry's membership in each collection via checkboxes.
fn membership_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, key: &str) {
    let slugs = state
        .borrow()
        .library
        .as_ref()
        .and_then(|l| l.collection_slugs().ok())
        .unwrap_or_default();
    if slugs.is_empty() {
        toast(
            widgets,
            "No collections yet — create one with the + above the list",
        );
        return;
    }

    let dialog = adw::Window::new();
    dialog.set_title(Some("Collections"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(360, -1);
    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let save = gtk4::Button::with_label("Save");
    save.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&save);
    view.add_top_bar(&header);

    let content = gtk4::Box::new(Orientation::Vertical, 6);
    content.set_margin_top(14);
    content.set_margin_bottom(14);
    content.set_margin_start(16);
    content.set_margin_end(16);

    let mut checks: Vec<(String, gtk4::CheckButton)> = Vec::new();
    {
        let s = state.borrow();
        let lib = s.library.as_ref().unwrap();
        let loaded: Vec<(String, fond_bib::Collection)> = slugs
            .iter()
            .map(|slug| (slug.clone(), lib.load_collection(slug).unwrap_or_default()))
            .collect();
        let keys_by_slug: HashMap<&str, &Vec<String>> = loaded
            .iter()
            .map(|(slug, coll)| (slug.as_str(), &coll.keys))
            .collect();
        for (slug, name, depth) in order_collection_tree(&loaded) {
            let label = format!(
                "{}{}",
                "    ".repeat(depth),
                if name.is_empty() { &slug } else { &name }
            );
            let check = gtk4::CheckButton::with_label(&label);
            check.set_active(
                keys_by_slug
                    .get(slug.as_str())
                    .is_some_and(|keys| keys.iter().any(|k| k == key)),
            );
            content.append(&check);
            checks.push((slug, check));
        }
    }
    view.set_content(Some(&content));
    dialog.set_content(Some(&view));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        let key = key.to_string();
        save.connect_clicked(move |_| {
            {
                let s = state.borrow();
                let lib = s.library.as_ref().unwrap();
                for (slug, check) in &checks {
                    let result = if check.is_active() {
                        lib.add_to_collection(slug, &key)
                    } else {
                        lib.remove_from_collection(slug, &key)
                    };
                    let _ = result;
                }
            }
            toast(&widgets, "Collections updated");
            dialog.close();
            refresh_list(&state, &widgets);
        });
    }
    dialog.present();
}

/// Edit the typed relationships from `key` to other entries **and knowledge-graph nodes**. A
/// searchable, checkable list of every other entry plus every node; each checked row carries
/// a predicate dropdown. On Save the chosen forward edges are written via
/// `Library::set_relations`, which maintains the inverse edge on each target automatically —
/// on whichever file type (note or node) the target resolves to.
///
/// Scope: this dialog models **one predicate per target** (the common case) and manages only
/// typed `relations` — legacy untyped `related` is lifted separately by
/// `migrate_related_to_relations`. Each row's predicate dropdown is curated to the target's
/// kind via `Predicate::forward_choices_for` (a person node offers Related/Influenced, a work
/// offers cites/critiques/…); it stays advisory — if a target's current forward edge already
/// uses a predicate outside that curated set, it's appended so Save round-trips it.
fn relations_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, key: &str) {
    use fond_bib::{Predicate, TargetKind};

    /// One pickable target: an entry or a node, with the predicate list appropriate to it.
    struct RowSpec {
        id: String,
        label: String,
        sub: String,
        kind: TargetKind,
    }
    // `rows` = every entry (except this one) then every node; `current` = target -> current
    // forward predicate (one-predicate-per-target model).
    let (rows, current): (Vec<RowSpec>, std::collections::HashMap<String, Predicate>) = {
        let s = state.borrow();
        let Some(lib) = s.library.as_ref() else {
            toast(widgets, "Open a library first");
            return;
        };
        let mut current: std::collections::HashMap<String, Predicate> =
            std::collections::HashMap::new();
        for r in lib.forward_relations(key).unwrap_or_default() {
            current.entry(r.target).or_insert(r.predicate);
        }
        // Entries (kind = Work).
        let mut rows: Vec<RowSpec> = s
            .entries
            .iter()
            .filter(|e| e.key != key)
            .map(|e| {
                let label = if e.title.is_empty() {
                    e.key.clone()
                } else {
                    e.title.clone()
                };
                let sub = match (e.author.is_empty(), e.year.is_empty()) {
                    (false, false) => format!("{} · {}", e.author, e.year),
                    (false, true) => e.author.clone(),
                    (true, false) => e.year.clone(),
                    (true, true) => e.key.clone(),
                };
                RowSpec {
                    id: e.key.clone(),
                    label,
                    sub,
                    kind: TargetKind::Work,
                }
            })
            .collect();
        // Nodes (kind from the node type).
        for slug in lib.node_slugs().unwrap_or_default() {
            if slug == key {
                continue;
            }
            if let Ok(node) = lib.load_node(&slug) {
                let fm = node.frontmatter;
                let label = if fm.label.is_empty() {
                    slug.clone()
                } else {
                    fm.label.clone()
                };
                rows.push(RowSpec {
                    sub: format!("{} · {}", node_type_label(fm.node_type), slug),
                    id: slug,
                    label,
                    kind: TargetKind::from(fm.node_type),
                });
            }
        }
        (rows, current)
    };

    if rows.is_empty() {
        toast(widgets, "Nothing else to relate to");
        return;
    }

    let dialog = adw::Window::new();
    dialog.set_title(Some("Relations"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(520, 480);
    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    let save = gtk4::Button::with_label("Save");
    save.add_css_class("suggested-action");
    let search = gtk4::SearchEntry::new();
    search.set_placeholder_text(Some("Filter entries and nodes"));
    search.set_width_chars(28);
    header.set_title_widget(Some(&search));
    header.pack_end(&save);
    view.add_top_bar(&header);

    let listbox = gtk4::ListBox::new();
    listbox.set_selection_mode(gtk4::SelectionMode::None);
    listbox.add_css_class("fond-list");
    let scroll = gtk4::ScrolledWindow::new();
    scroll.add_css_class("fond-ground");
    scroll.set_child(Some(&listbox));
    scroll.set_vexpand(true);
    view.set_content(Some(&scroll));
    dialog.set_content(Some(&view));

    // Build every row up front (checkbox/predicate state survives filtering, which only
    // toggles row visibility). Each row keeps its own predicate `options` (curated by target
    // kind), so the correct predicate can be recovered from the dropdown index on Save.
    struct RelRow {
        key: String,
        check: gtk4::CheckButton,
        predicate: gtk4::DropDown,
        options: Vec<Predicate>,
        row: gtk4::ListBoxRow,
        hay: String,
    }
    let rel_rows: Rc<Vec<RelRow>> = Rc::new(
        rows.into_iter()
            .map(|spec| {
                let RowSpec {
                    id: k,
                    label,
                    sub,
                    kind,
                } = spec;

                // Domain-appropriate predicates for this target kind, plus any current
                // out-of-set predicate so a hand-authored edge round-trips.
                let mut options = Predicate::forward_choices_for(kind);
                if let Some(p) = current.get(&k) {
                    if !options.contains(p) {
                        options.push(*p);
                    }
                }
                let option_labels: Vec<&str> = options.iter().map(|p| p.label()).collect();

                let check = gtk4::CheckButton::new();
                let checked = current.contains_key(&k);
                check.set_active(checked);

                let predicate = gtk4::DropDown::from_strings(&option_labels);
                predicate.set_valign(gtk4::Align::Center);
                // Preselect the target's current predicate (default `Related` = index 0).
                if let Some(p) = current.get(&k) {
                    if let Some(idx) = options.iter().position(|o| o == p) {
                        predicate.set_selected(idx as u32);
                    }
                }
                predicate.set_sensitive(checked);
                // The predicate only matters when the row is checked.
                {
                    let predicate = predicate.clone();
                    check.connect_toggled(move |c| predicate.set_sensitive(c.is_active()));
                }

                let text = gtk4::Box::new(Orientation::Vertical, 0);
                text.set_hexpand(true);
                let t = gtk4::Label::new(Some(&label));
                t.set_halign(gtk4::Align::Start);
                t.set_xalign(0.0);
                t.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                t.add_css_class("fond-row-title");
                let m = gtk4::Label::new(Some(&sub));
                m.set_halign(gtk4::Align::Start);
                m.set_xalign(0.0);
                m.add_css_class("fond-row-meta");
                text.append(&t);
                text.append(&m);
                let hbox = gtk4::Box::new(Orientation::Horizontal, 8);
                hbox.set_margin_top(4);
                hbox.set_margin_bottom(4);
                hbox.set_margin_start(8);
                hbox.set_margin_end(8);
                hbox.append(&check);
                hbox.append(&text);
                hbox.append(&predicate);
                let row = gtk4::ListBoxRow::new();
                row.add_css_class("fond-row");
                row.set_child(Some(&hbox));
                row.set_activatable(false);
                listbox.append(&row);
                let hay = format!("{label} {sub} {k}").to_lowercase();
                RelRow {
                    key: k,
                    check,
                    predicate,
                    options,
                    row,
                    hay,
                }
            })
            .collect(),
    );

    {
        let rel_rows = rel_rows.clone();
        search.connect_search_changed(move |e| {
            let q = e.text().to_lowercase();
            for r in rel_rows.iter() {
                r.row.set_visible(q.is_empty() || r.hay.contains(&q));
            }
        });
    }

    {
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        let key = key.to_string();
        let rel_rows = rel_rows.clone();
        save.connect_clicked(move |_| {
            let forward: Vec<fond_bib::Relation> = rel_rows
                .iter()
                .filter(|r| r.check.is_active())
                .map(|r| {
                    let idx = r.predicate.selected() as usize;
                    let predicate = r.options.get(idx).copied().unwrap_or(Predicate::Related);
                    fond_bib::Relation::forward(predicate, r.key.clone())
                })
                .collect();
            let result = {
                let s = state.borrow();
                s.library.as_ref().unwrap().set_relations(&key, &forward)
            };
            match result {
                Ok(()) => {
                    toast(&widgets, "Relations updated");
                    dialog.close();
                    reload_current(&state, &widgets);
                }
                Err(e) => toast(&widgets, &friendly::bib_error(&e)),
            }
        });
    }

    dialog.present();
    search.grab_focus();
}

/// One node in the relations-map prototype: an entry (`"work"`) or a node, classified by
/// `fond_bib::NodeType` so the graph can colour/label it distinctly. `label` is resolved the
/// same way `target_display` does for the plain backlinks panel in `show_detail`.
#[derive(serde::Serialize)]
struct GraphNode {
    id: String,
    label: String,
    kind: &'static str,
}

#[derive(serde::Serialize)]
struct GraphEdge {
    from: String,
    to: String,
    label: &'static str,
}

#[derive(serde::Serialize, Default)]
struct GraphPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    center: Option<String>,
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
}

/// Resolve `id` (an entry key or a node slug) to its graph label and kind. Falls back to the
/// bare id for a dangling target (same "still show *something*" fallback `target_display` uses).
fn graph_node_kind(lib: &Library, id: &str) -> (String, &'static str) {
    if let Ok(parsed) = lib.load_entry(id) {
        let title = bibentry::title_string(&parsed.entry).unwrap_or_default();
        return (
            if title.is_empty() {
                id.to_string()
            } else {
                title
            },
            "work",
        );
    }
    if let Ok(node) = lib.load_node(id) {
        let kind = match node.frontmatter.node_type {
            fond_bib::NodeType::Person => "person",
            fond_bib::NodeType::School => "school",
            fond_bib::NodeType::Concept => "concept",
            fond_bib::NodeType::Event => "event",
            fond_bib::NodeType::Place => "place",
            fond_bib::NodeType::WorkUncataloged => "work",
        };
        let label = if node.frontmatter.label.is_empty() {
            id.to_string()
        } else {
            node.frontmatter.label
        };
        return (label, kind);
    }
    (id.to_string(), "other")
}

/// Every relation recorded on `id`'s own note — forward *and* inverse together, which is
/// exactly the point: an inverse edge is Kartoteka's maintained backlink (if A cites B, B's
/// note carries the inverse `cited-by → A` edge), so this one call already gives `id`'s full
/// local neighbourhood, not just what it points at. One edge per target (a target related two
/// ways is rare and not worth two overlapping lines in a v0 map).
fn graph_expand(lib: &Library, id: &str) -> GraphPatch {
    let mut patch = GraphPatch::default();
    let mut seen = std::collections::HashSet::new();
    for r in lib.relations(id).unwrap_or_default() {
        if !seen.insert(r.target.clone()) {
            continue;
        }
        let (label, kind) = graph_node_kind(lib, &r.target);
        patch.nodes.push(GraphNode {
            id: r.target.clone(),
            label,
            kind,
        });
        patch.edges.push(GraphEdge {
            from: id.to_string(),
            to: r.target,
            label: r.predicate.label(),
        });
    }
    patch
}

/// Build a graph of the *whole* library's forward relations (skipping Kartoteka-maintained
/// inverse edges — each is just the mirror of some other item's forward edge, so including
/// both would draw every connection twice) — capped the same way `graph_expand`'s node cap
/// works, just enforced here instead of relying on the JS-side `MAX_NODES` truncation, so the
/// most-connected library-wide entry point can't ever pull in more nodes than a reasonable
/// force layout still reads as a map rather than a hairball.
const LIBRARY_GRAPH_NODE_CAP: usize = 150;

fn build_library_graph(lib: &Library) -> GraphPatch {
    let mut patch = GraphPatch::default();
    let mut node_ids: HashSet<String> = HashSet::new();
    let mut edge_pairs: HashSet<(String, String)> = HashSet::new();

    let mut ids: Vec<String> = lib.keys_sorted().unwrap_or_default();
    ids.extend(lib.node_slugs().unwrap_or_default());

    'outer: for id in &ids {
        for r in lib.relations(id).unwrap_or_default() {
            if r.inverse {
                continue;
            }
            if node_ids.len() >= LIBRARY_GRAPH_NODE_CAP
                && !node_ids.contains(id)
                && !node_ids.contains(&r.target)
            {
                continue;
            }
            if node_ids.insert(id.clone()) {
                let (label, kind) = graph_node_kind(lib, id);
                patch.nodes.push(GraphNode {
                    id: id.clone(),
                    label,
                    kind,
                });
            }
            if node_ids.insert(r.target.clone()) {
                let (label, kind) = graph_node_kind(lib, &r.target);
                patch.nodes.push(GraphNode {
                    id: r.target.clone(),
                    label,
                    kind,
                });
            }
            if edge_pairs.insert((id.clone(), r.target.clone())) {
                patch.edges.push(GraphEdge {
                    from: id.clone(),
                    to: r.target.clone(),
                    label: r.predicate.label(),
                });
            }
            if node_ids.len() >= LIBRARY_GRAPH_NODE_CAP {
                continue 'outer;
            }
        }
    }
    patch
}

/// (id, display label, count), sorted by count descending — the shape both analytics rankings
/// below share.
type GraphRanking = Vec<(String, String, usize)>;

/// Top-`n` entries by how many relation edges touch them (`most_connected`) and, separately,
/// by how many forward `Cites` edges name them as the cited work (`most_cited`) — the two
/// library-wide analytics the relations map's sidebar shows alongside the graph itself.
/// Both walk only forward (user-authored) relations, same as `build_library_graph`, so an
/// edge isn't double-counted through its maintained inverse. Computed straight from each
/// item's own relations rather than from `build_library_graph`'s (possibly capped) patch, so
/// a library bigger than the graph's node cap still gets accurate rankings.
fn library_graph_analytics(lib: &Library) -> (GraphRanking, GraphRanking) {
    let mut ids: Vec<String> = lib.keys_sorted().unwrap_or_default();
    ids.extend(lib.node_slugs().unwrap_or_default());

    let mut degree: HashMap<String, usize> = HashMap::new();
    let mut cited: HashMap<String, usize> = HashMap::new();
    for id in &ids {
        for r in lib.relations(id).unwrap_or_default() {
            if r.inverse {
                continue;
            }
            *degree.entry(id.clone()).or_insert(0) += 1;
            *degree.entry(r.target.clone()).or_insert(0) += 1;
            if r.predicate == fond_bib::Predicate::Cites {
                *cited.entry(r.target.clone()).or_insert(0) += 1;
            }
        }
    }

    let rank = |counts: HashMap<String, usize>| -> Vec<(String, String, usize)> {
        let mut v: Vec<(String, String, usize)> = counts
            .into_iter()
            .map(|(id, n)| {
                let (label, _) = graph_node_kind(lib, &id);
                (id, label, n)
            })
            .collect();
        v.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
        v.truncate(10);
        v
    };
    (rank(degree), rank(cited))
}

/// **Prototype.** An entry-centered map of its relations: force-directed, pan/zoomable,
/// click a node to pull *its* connections in too (expanding outward — the center never
/// moves). Read-only for now — no editing relations from here, no navigating into an entry;
/// just exploring the shape of what's connected to what. Rendered in a `WebView` (Canvas 2D
/// plus a small hand-written force simulation) rather than hand-built with `Cairo`/
/// `GtkDrawingArea` — graph layout and hit-testing are things a browser already does well,
/// and this reuses the same `WebView`-embedding and Rust↔JS bridge pattern the EPUB reader
/// established, just with the message flowing JS→Rust via `UserContentManager` (new to this
/// codebase) instead of only Rust→JS.
fn show_relations_graph(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, key: &str) {
    let (center_label, initial) = {
        let s = state.borrow();
        let Some(lib) = s.library.as_ref() else {
            toast(widgets, "Open a library first");
            return;
        };
        let (label, kind) = graph_node_kind(lib, key);
        let mut patch = graph_expand(lib, key);
        patch.center = Some(key.to_string());
        patch.nodes.insert(
            0,
            GraphNode {
                id: key.to_string(),
                label: label.clone(),
                kind,
            },
        );
        (label, patch)
    };

    let dialog = adw::Window::new();
    dialog.set_title(Some(&format!("Relations map: {center_label}")));
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(900, 700);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    let hint = gtk4::Label::new(Some("Prototype — click to expand, double-click to open"));
    hint.add_css_class("dim-label");
    header.set_title_widget(Some(&hint));
    let reset_button = gtk4::Button::from_icon_name("view-refresh-symbolic");
    reset_button.set_tooltip_text(Some("Reset to just this entry's direct connections"));
    header.pack_start(&reset_button);
    view.add_top_bar(&header);

    let web_view = webkit6::WebView::new();
    web_view.set_vexpand(true);
    web_view.set_hexpand(true);

    if let Some(ucm) = webkit6::prelude::WebViewExt::user_content_manager(&web_view) {
        ucm.register_script_message_handler("kartoteka", None);
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        let view_for_reply = web_view.clone();
        ucm.connect_script_message_received(Some("kartoteka"), move |_, js_value| {
            let raw = js_value.to_str();
            let Ok(msg) = serde_json::from_str::<serde_json::Value>(&raw) else {
                return;
            };
            if let Some(id) = msg.get("expand").and_then(|v| v.as_str()) {
                let patch = {
                    let s = state.borrow();
                    match s.library.as_ref() {
                        Some(lib) => graph_expand(lib, id),
                        None => return,
                    }
                };
                let json = serde_json::to_string(&patch).unwrap_or_else(|_| "{}".to_string());
                view_for_reply.evaluate_javascript(
                    &format!("mergeGraph({json})"),
                    None,
                    None,
                    gio::Cancellable::NONE,
                    |_| {},
                );
            } else if let Some(id) = msg.get("open").and_then(|v| v.as_str()) {
                let is_entry = state.borrow().key_to_index.contains_key(id);
                dialog.close();
                if is_entry {
                    select_key(&state, &widgets, id);
                } else {
                    show_node_editor(&state, &widgets, Some(id.to_string()), Rc::new(|| {}));
                }
            }
        });
    }

    {
        let initial_json = serde_json::to_string(&initial).unwrap_or_else(|_| "{}".to_string());
        web_view.connect_load_changed(move |view, event| {
            if event == webkit6::LoadEvent::Finished {
                view.evaluate_javascript(
                    &format!("initGraph({initial_json})"),
                    None,
                    None,
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }
        });
    }
    {
        let view_for_reset = web_view.clone();
        reset_button.connect_clicked(move |_| {
            view_for_reset.evaluate_javascript(
                "resetGraph()",
                None,
                None,
                gio::Cancellable::NONE,
                |_| {},
            );
        });
    }
    web_view.load_html(RELATIONS_GRAPH_HTML, None);

    view.set_content(Some(&web_view));
    dialog.set_content(Some(&view));
    dialog.present();
}

/// One "Most connected"/"Most cited" list in the whole-library map's sidebar — plain
/// read-only rows (name, count); no click-to-navigate, unlike the graph itself.
fn append_analytics_section(
    container: &gtk4::Box,
    heading: &str,
    items: &[(String, String, usize)],
) {
    let head = gtk4::Label::new(Some(heading));
    head.add_css_class("heading");
    head.set_xalign(0.0);
    head.set_margin_top(6);
    container.append(&head);
    if items.is_empty() {
        let l = gtk4::Label::new(Some("None yet"));
        l.add_css_class("dim-label");
        l.set_xalign(0.0);
        container.append(&l);
        return;
    }
    for (_, label, n) in items {
        let row = gtk4::Box::new(Orientation::Horizontal, 6);
        let name = gtk4::Label::new(Some(label));
        name.set_xalign(0.0);
        name.set_hexpand(true);
        name.set_wrap(true);
        name.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        let count = gtk4::Label::new(Some(&n.to_string()));
        count.add_css_class("dim-label");
        count.add_css_class("fond-row-meta");
        row.append(&name);
        row.append(&count);
        container.append(&row);
    }
}

/// **Prototype**, same as `show_relations_graph` but seeded with the whole library's forward
/// relations at once (`build_library_graph`) instead of one entry's neighbourhood — a bird's-
/// eye view of everything connected to everything, plus a sidebar of the two library-wide
/// analytics (`library_graph_analytics`): most-connected and most-cited. Node clicks still
/// expand further (useful once the library exceeds `LIBRARY_GRAPH_NODE_CAP` and the map only
/// shows a capped subset) and double-click still opens, via the identical message-handler
/// wiring `show_relations_graph` uses — the JS side treats a whole-library seed exactly like
/// any other patch, `center` just stays unset so no node is pinned.
fn show_library_graph(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    let (patch, most_connected, most_cited) = {
        let s = state.borrow();
        let Some(lib) = s.library.as_ref() else {
            toast(widgets, "Open a library first");
            return;
        };
        let (mc, mci) = library_graph_analytics(lib);
        (build_library_graph(lib), mc, mci)
    };
    if patch.nodes.is_empty() {
        toast(widgets, "No relations recorded yet");
        return;
    }

    let dialog = adw::Window::new();
    dialog.set_title(Some("Relations map — whole library"));
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(1050, 700);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    let hint = gtk4::Label::new(Some("Prototype — click to expand, double-click to open"));
    hint.add_css_class("dim-label");
    header.set_title_widget(Some(&hint));
    view.add_top_bar(&header);

    let sidebar = gtk4::Box::new(Orientation::Vertical, 10);
    sidebar.set_margin_top(12);
    sidebar.set_margin_bottom(12);
    sidebar.set_margin_start(12);
    sidebar.set_margin_end(12);
    append_analytics_section(&sidebar, "Most connected", &most_connected);
    append_analytics_section(&sidebar, "Most cited", &most_cited);
    let sidebar_scroll = gtk4::ScrolledWindow::new();
    sidebar_scroll.set_child(Some(&sidebar));
    sidebar_scroll.set_width_request(220);
    sidebar_scroll.add_css_class("fond-sidebar");

    let web_view = webkit6::WebView::new();
    web_view.set_vexpand(true);
    web_view.set_hexpand(true);

    if let Some(ucm) = webkit6::prelude::WebViewExt::user_content_manager(&web_view) {
        ucm.register_script_message_handler("kartoteka", None);
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        let view_for_reply = web_view.clone();
        ucm.connect_script_message_received(Some("kartoteka"), move |_, js_value| {
            let raw = js_value.to_str();
            let Ok(msg) = serde_json::from_str::<serde_json::Value>(&raw) else {
                return;
            };
            if let Some(id) = msg.get("expand").and_then(|v| v.as_str()) {
                let patch = {
                    let s = state.borrow();
                    match s.library.as_ref() {
                        Some(lib) => graph_expand(lib, id),
                        None => return,
                    }
                };
                let json = serde_json::to_string(&patch).unwrap_or_else(|_| "{}".to_string());
                view_for_reply.evaluate_javascript(
                    &format!("mergeGraph({json})"),
                    None,
                    None,
                    gio::Cancellable::NONE,
                    |_| {},
                );
            } else if let Some(id) = msg.get("open").and_then(|v| v.as_str()) {
                let is_entry = state.borrow().key_to_index.contains_key(id);
                dialog.close();
                if is_entry {
                    select_key(&state, &widgets, id);
                } else {
                    show_node_editor(&state, &widgets, Some(id.to_string()), Rc::new(|| {}));
                }
            }
        });
    }

    {
        let initial_json = serde_json::to_string(&patch).unwrap_or_else(|_| "{}".to_string());
        web_view.connect_load_changed(move |view, event| {
            if event == webkit6::LoadEvent::Finished {
                view.evaluate_javascript(
                    &format!("initGraph({initial_json})"),
                    None,
                    None,
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }
        });
    }
    web_view.load_html(RELATIONS_GRAPH_HTML, None);

    let paned = gtk4::Paned::new(Orientation::Horizontal);
    paned.set_start_child(Some(&sidebar_scroll));
    paned.set_end_child(Some(&web_view));
    paned.set_resize_start_child(false);
    paned.set_position(220);

    view.set_content(Some(&paned));
    dialog.set_content(Some(&view));
    dialog.present();
}

/// Self-contained HTML/JS for the relations-map prototype: no external resources (offline,
/// same as everything else in Kartoteka), a small hand-written force simulation (no need to
/// vendor d3-force for the node counts a one-entry-deep, click-to-expand map produces),
/// Canvas 2D rendering, and pan (drag empty space) / zoom (scroll). `initGraph`/`mergeGraph`/
/// `resetGraph` are called from Rust; a node click posts `{"expand": "<id>"}` back via
/// `window.webkit.messageHandlers.kartoteka`, a double-click posts `{"open": "<id>"}`.
/// Colours are CSS custom properties (one definition per light/dark, read into JS via
/// `getComputedStyle` rather than a parallel `dark ? … : …` table) so the canvas and the
/// legend can never drift out of sync with each other.
const RELATIONS_GRAPH_HTML: &str = r##"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<style>
  :root {
    --bg: #fafafa; --panel: rgba(255,255,255,0.88); --fg: #2e2e2e; --dim: #8a8a8a;
    --edge: rgba(0,0,0,0.25);
    --c-work: #3d78c2; --c-person: #4a9e4a; --c-school: #c4922a;
    --c-concept: #a35bc2; --c-event: #c26a48; --c-place: #3a9d9d; --c-other: #777777;
  }
  @media (prefers-color-scheme: dark) {
    :root {
      --bg: #1e1e1e; --panel: rgba(35,35,35,0.88); --fg: #e3e3e3; --dim: #9a9a9a;
      --edge: rgba(255,255,255,0.3);
      --c-work: #5aa0e6; --c-person: #7fc97f; --c-school: #e0b04a;
      --c-concept: #c98adb; --c-event: #e08a6a; --c-place: #6ac9c9; --c-other: #999999;
    }
  }
  html, body { margin: 0; padding: 0; overflow: hidden; background: var(--bg); }
  canvas { display: block; cursor: grab; }
  .panel {
    position: fixed; background: var(--panel); color: var(--fg);
    border: 1px solid var(--edge); border-radius: 8px; font: 11px sans-serif;
  }
  .legend { left: 10px; bottom: 10px; padding: 8px 10px; }
  .legend .row { display: flex; align-items: center; gap: 6px; margin: 2px 0; }
  .legend .dot { width: 10px; height: 10px; border-radius: 50%; display: inline-block; flex: none; }
  .legend .hint { margin-top: 6px; color: var(--dim); max-width: 160px; }
  .banner {
    top: 12px; left: 50%; transform: translateX(-50%); padding: 6px 14px;
    opacity: 0; transition: opacity 0.25s; pointer-events: none;
  }
  .banner.show { opacity: 1; }
</style>
</head>
<body>
<canvas id="c"></canvas>
<div class="panel legend">
  <div class="row"><span class="dot" style="background:var(--c-work)"></span>Work</div>
  <div class="row"><span class="dot" style="background:var(--c-person)"></span>Person</div>
  <div class="row"><span class="dot" style="background:var(--c-school)"></span>School</div>
  <div class="row"><span class="dot" style="background:var(--c-concept)"></span>Concept</div>
  <div class="row"><span class="dot" style="background:var(--c-event)"></span>Event</div>
  <div class="row"><span class="dot" style="background:var(--c-place)"></span>Place</div>
  <div class="hint">Click: expand · double-click: open · right-click: remove</div>
</div>
<div class="panel banner" id="banner"></div>
<script>
(function() {
  var canvas = document.getElementById('c');
  var ctx = canvas.getContext('2d');
  function resize() { canvas.width = window.innerWidth; canvas.height = window.innerHeight; }
  window.addEventListener('resize', resize);
  resize();

  var style = getComputedStyle(document.documentElement);
  function cssVar(name) { return style.getPropertyValue(name).trim(); }
  var fg = cssVar('--fg'), dim = cssVar('--dim'), edgeColor = cssVar('--edge');
  var kindColor = {
    work: cssVar('--c-work'), person: cssVar('--c-person'), school: cssVar('--c-school'),
    concept: cssVar('--c-concept'), event: cssVar('--c-event'), place: cssVar('--c-place'),
    other: cssVar('--c-other')
  };

  var MAX_NODES = 80;

  var nodes = new Map(); // id -> {id,label,kind,x,y,vx,vy,pinned,loading}
  var edges = []; // {from,to,label}
  var centerId = null;
  var initialData = null;
  var offsetX = 0, offsetY = 0, scale = 1;

  function postMsg(obj) {
    if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.kartoteka) {
      window.webkit.messageHandlers.kartoteka.postMessage(JSON.stringify(obj));
    }
  }

  var bannerTimer = null;
  function showBanner(text) {
    var b = document.getElementById('banner');
    b.textContent = text;
    b.classList.add('show');
    if (bannerTimer) clearTimeout(bannerTimer);
    bannerTimer = setTimeout(function() { b.classList.remove('show'); }, 2500);
  }

  function addNode(n) {
    if (nodes.has(n.id)) return true;
    if (nodes.size >= MAX_NODES) return false;
    var angle = Math.random() * Math.PI * 2;
    var r = 120 + Math.random() * 60;
    var cx = centerId && nodes.has(centerId) ? nodes.get(centerId).x : canvas.width / 2;
    var cy = centerId && nodes.has(centerId) ? nodes.get(centerId).y : canvas.height / 2;
    nodes.set(n.id, {
      id: n.id, label: n.label, kind: n.kind,
      x: cx + Math.cos(angle) * r, y: cy + Math.sin(angle) * r,
      vx: 0, vy: 0, pinned: false, loading: false
    });
    return true;
  }

  window.initGraph = function(data) {
    initialData = data;
    nodes.clear();
    edges = [];
    centerId = data.center || null;
    (data.nodes || []).forEach(function(n) {
      if (n.id === centerId) {
        nodes.set(n.id, {
          id: n.id, label: n.label, kind: n.kind,
          x: canvas.width / 2, y: canvas.height / 2, vx: 0, vy: 0, pinned: true, loading: false
        });
      } else {
        addNode(n);
      }
    });
    (data.edges || []).forEach(function(e) { edges.push(e); });
  };

  window.resetGraph = function() {
    if (initialData) window.initGraph(initialData);
  };

  window.mergeGraph = function(data) {
    var capped = false;
    (data.nodes || []).forEach(function(n) {
      if (!addNode(n)) capped = true;
    });
    (data.edges || []).forEach(function(e) {
      if (!nodes.has(e.from) || !nodes.has(e.to)) return;
      var exists = edges.some(function(x) {
        return (x.from === e.from && x.to === e.to) || (x.from === e.to && x.to === e.from);
      });
      if (!exists) edges.push(e);
    });
    // Only one expand request is ever in flight at a time in this prototype, so clearing
    // every "loading" spinner on any merge is enough — no need to track which node it was.
    nodes.forEach(function(nd) { nd.loading = false; });
    if (capped) {
      showBanner('Map capped at ' + MAX_NODES + ' nodes — right-click a node to remove it');
    }
  };

  // ---- physics: simple repulsion + spring edges + weak centering ----
  function step() {
    var arr = Array.from(nodes.values());
    var REPEL = 2600, SPRING = 0.02, REST = 90, DAMP = 0.85, CENTER_PULL = 0.0025;
    for (var i = 0; i < arr.length; i++) {
      for (var j = i + 1; j < arr.length; j++) {
        var a = arr[i], b = arr[j];
        var dx = a.x - b.x, dy = a.y - b.y;
        var d2 = dx * dx + dy * dy + 0.01;
        var f = REPEL / d2;
        var d = Math.sqrt(d2);
        var fx = (dx / d) * f, fy = (dy / d) * f;
        if (!a.pinned) { a.vx += fx; a.vy += fy; }
        if (!b.pinned) { b.vx -= fx; b.vy -= fy; }
      }
    }
    edges.forEach(function(e) {
      var a = nodes.get(e.from), b = nodes.get(e.to);
      if (!a || !b) return;
      var dx = b.x - a.x, dy = b.y - a.y;
      var d = Math.sqrt(dx * dx + dy * dy) || 0.01;
      var f = (d - REST) * SPRING;
      var fx = (dx / d) * f, fy = (dy / d) * f;
      if (!a.pinned) { a.vx += fx; a.vy += fy; }
      if (!b.pinned) { b.vx -= fx; b.vy -= fy; }
    });
    var cx = canvas.width / 2, cy = canvas.height / 2;
    arr.forEach(function(n) {
      if (n.pinned) { n.x = cx; n.y = cy; return; }
      n.vx += (cx - n.x) * CENTER_PULL;
      n.vy += (cy - n.y) * CENTER_PULL;
      n.vx *= DAMP; n.vy *= DAMP;
      n.x += n.vx; n.y += n.vy;
    });
  }

  function nodeRadius(n) { return n.id === centerId ? 22 : 14; }

  // Draws the edge line short of `b`'s own circle, plus a small filled arrowhead touching
  // it — direction is meaningful here (the predicate label is phrased from `a`'s side, e.g.
  // "Cites"/"Critiqued by"), so an undirected line was losing information the label alone
  // didn't fully make up for.
  function drawEdge(a, b, label) {
    var dx = b.x - a.x, dy = b.y - a.y;
    var d = Math.sqrt(dx * dx + dy * dy) || 0.01;
    var ux = dx / d, uy = dy / d;
    var rTo = nodeRadius(b) + 3;
    var tipX = b.x - ux * rTo, tipY = b.y - uy * rTo;

    ctx.strokeStyle = edgeColor;
    ctx.beginPath();
    ctx.moveTo(a.x + ux * (nodeRadius(a) + 1), a.y + uy * (nodeRadius(a) + 1));
    ctx.lineTo(tipX, tipY);
    ctx.stroke();

    var size = 6;
    var baseX = tipX - ux * size, baseY = tipY - uy * size;
    ctx.beginPath();
    ctx.moveTo(tipX, tipY);
    ctx.lineTo(baseX - uy * size * 0.5, baseY + ux * size * 0.5);
    ctx.lineTo(baseX + uy * size * 0.5, baseY - ux * size * 0.5);
    ctx.closePath();
    ctx.fillStyle = edgeColor;
    ctx.fill();

    ctx.fillStyle = dim;
    ctx.font = '11px sans-serif';
    ctx.textAlign = 'center';
    ctx.fillText(label, (a.x + b.x) / 2, (a.y + b.y) / 2 - 4);
  }

  function draw() {
    ctx.save();
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.translate(offsetX, offsetY);
    ctx.scale(scale, scale);

    edges.forEach(function(e) {
      var a = nodes.get(e.from), b = nodes.get(e.to);
      if (!a || !b) return;
      drawEdge(a, b, e.label);
    });

    nodes.forEach(function(n) {
      var r = nodeRadius(n);
      ctx.beginPath();
      ctx.arc(n.x, n.y, r, 0, Math.PI * 2);
      ctx.fillStyle = kindColor[n.kind] || kindColor.other;
      ctx.fill();
      if (n.id === centerId) {
        ctx.lineWidth = 2;
        ctx.strokeStyle = fg;
        ctx.stroke();
      }
      if (n.loading) {
        ctx.lineWidth = 2;
        ctx.strokeStyle = fg;
        ctx.beginPath();
        ctx.arc(n.x, n.y, r + 4, (Date.now() / 200) % (Math.PI * 2), (Date.now() / 200) % (Math.PI * 2) + 1.5);
        ctx.stroke();
      }
      ctx.fillStyle = fg;
      ctx.font = n.id === centerId ? 'bold 12px sans-serif' : '12px sans-serif';
      ctx.textAlign = 'center';
      ctx.fillText(n.label, n.x, n.y + r + 14);
    });
    ctx.restore();
  }

  function tick() {
    step();
    draw();
    requestAnimationFrame(tick);
  }
  requestAnimationFrame(tick);

  // ---- interaction ----
  // click: expand · double-click (self-timed, not the native `dblclick` event, so its
  // window lines up exactly with the expand delay below rather than trusting the browser's
  // own threshold to agree with ours): open · right-click: remove (not the center) · drag
  // empty space: pan · drag a node: reposition (and pin) it · scroll: zoom.
  function toWorld(px, py) {
    return { x: (px - offsetX) / scale, y: (py - offsetY) / scale };
  }
  function hitNode(px, py) {
    var w = toWorld(px, py);
    var hit = null;
    nodes.forEach(function(n) {
      var r = nodeRadius(n) + 4;
      var dx = w.x - n.x, dy = w.y - n.y;
      if (dx * dx + dy * dy <= r * r) hit = n;
    });
    return hit;
  }

  var dragging = false, dragStart = null, draggedNode = null, dragMoved = false;
  canvas.addEventListener('mousedown', function(ev) {
    var n = hitNode(ev.offsetX, ev.offsetY);
    dragMoved = false;
    if (n && n.id !== centerId) {
      draggedNode = n;
      n.pinned = true;
    } else {
      dragging = true;
      dragStart = { x: ev.offsetX - offsetX, y: ev.offsetY - offsetY };
    }
  });
  canvas.addEventListener('mousemove', function(ev) {
    if (draggedNode) {
      dragMoved = true;
      var w = toWorld(ev.offsetX, ev.offsetY);
      draggedNode.x = w.x; draggedNode.y = w.y;
    } else if (dragging) {
      dragMoved = true;
      offsetX = ev.offsetX - dragStart.x;
      offsetY = ev.offsetY - dragStart.y;
    }
  });
  window.addEventListener('mouseup', function() {
    if (draggedNode) {
      // A plain click (no real drag) on a node unpins it again — only a drag the user
      // actually performed leaves it pinned where they put it.
      if (!dragMoved) draggedNode.pinned = false;
      draggedNode = null;
    }
    dragging = false;
  });

  var pendingClick = null; // {node, timer}
  var CLICK_DELAY = 300;
  canvas.addEventListener('click', function(ev) {
    if (dragMoved) return;
    var n = hitNode(ev.offsetX, ev.offsetY);
    if (!n) return;
    if (pendingClick && pendingClick.node === n) {
      clearTimeout(pendingClick.timer);
      pendingClick = null;
      postMsg({ open: n.id });
      return;
    }
    if (pendingClick) clearTimeout(pendingClick.timer);
    pendingClick = {
      node: n,
      timer: setTimeout(function() {
        pendingClick = null;
        if (n.loading) return;
        n.loading = true;
        postMsg({ expand: n.id });
      }, CLICK_DELAY)
    };
  });
  canvas.addEventListener('contextmenu', function(ev) {
    ev.preventDefault();
    var n = hitNode(ev.offsetX, ev.offsetY);
    if (!n || n.id === centerId) return;
    nodes.delete(n.id);
    edges = edges.filter(function(e) { return e.from !== n.id && e.to !== n.id; });
  });
  canvas.addEventListener('wheel', function(ev) {
    ev.preventDefault();
    var delta = ev.deltaY > 0 ? 0.9 : 1.1;
    scale = Math.max(0.2, Math.min(3, scale * delta));
  }, { passive: false });
})();
</script>
</body>
</html>"##;

/// Save the current search query as a named saved search (a virtual collection).
fn save_search_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    let query = state.borrow().query.trim().to_string();
    if query.is_empty() {
        toast(widgets, "Type a search first, then save it");
        return;
    }
    let dialog = adw::Window::new();
    dialog.set_title(Some("Save search"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(380, -1);
    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let save = gtk4::Button::with_label("Save");
    save.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&save);
    view.add_top_bar(&header);
    let content = gtk4::Box::new(Orientation::Vertical, 8);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);
    let hint = gtk4::Label::new(Some(&format!("Query: {query}")));
    hint.add_css_class("dim-label");
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    let entry = gtk4::Entry::builder()
        .placeholder_text("Name for this saved search")
        .activates_default(true)
        .build();
    content.append(&entry);
    content.append(&hint);
    view.set_content(Some(&content));
    dialog.set_content(Some(&view));
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        save.connect_clicked(move |_| {
            let name = entry.text().trim().to_string();
            if name.is_empty() {
                return;
            }
            state
                .borrow_mut()
                .saved_searches
                .push((name, query.clone()));
            save_saved_searches(&state);
            toast(&widgets, "Saved search added");
            dialog.close();
            refresh_collections(&state, &widgets);
        });
    }
    dialog.present();
}

/// Prompt for a name and create a new (empty) collection.
fn new_collection_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    if state.borrow().library.is_none() {
        toast(widgets, "Open a library first");
        return;
    }
    let dialog = adw::Window::new();
    dialog.set_title(Some("New collection"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(380, -1);
    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let create = gtk4::Button::with_label("Create");
    create.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&create);
    view.add_top_bar(&header);
    let content = gtk4::Box::new(Orientation::Vertical, 8);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    let entry = gtk4::Entry::builder()
        .placeholder_text("Collection name")
        .activates_default(true)
        .build();
    content.append(&entry);

    // "(top level)" plus every existing collection, indented to match the sidebar tree, so a
    // new collection can be created directly as a child instead of only ever landing at the
    // top and needing a later edit to nest it.
    let (parent_slugs, parent_labels) = {
        let s = state.borrow();
        let lib = s.library.as_ref().expect("library open");
        let slugs = lib.collection_slugs().unwrap_or_default();
        let loaded: Vec<(String, fond_bib::Collection)> = slugs
            .into_iter()
            .map(|slug| {
                let coll = lib.load_collection(&slug).unwrap_or_default();
                (slug, coll)
            })
            .collect();
        let ordered = order_collection_tree(&loaded);
        let mut slugs = vec![String::new()];
        let mut labels = vec!["(top level)".to_string()];
        for (slug, name, depth) in ordered {
            slugs.push(slug);
            labels.push(format!("{}{}", "    ".repeat(depth), name));
        }
        (slugs, labels)
    };
    let parent_label_refs: Vec<&str> = parent_labels.iter().map(String::as_str).collect();
    let parent_drop = gtk4::DropDown::from_strings(&parent_label_refs);
    parent_drop.set_tooltip_text(Some("Parent collection (optional)"));
    content.append(&parent_drop);

    view.set_content(Some(&content));
    dialog.set_content(Some(&view));
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        create.connect_clicked(move |_| {
            let name = entry.text().trim().to_string();
            if name.is_empty() {
                return;
            }
            let parent = parent_slugs
                .get(parent_drop.selected() as usize)
                .filter(|s| !s.is_empty())
                .cloned();
            let result = {
                let s = state.borrow();
                let lib = s.library.as_ref().expect("library open");
                lib.create_collection(&name, parent.as_deref())
            };
            match result {
                Ok(_) => {
                    toast(&widgets, &format!("Created collection “{name}”"));
                    dialog.close();
                    refresh_collections(&state, &widgets);
                }
                Err(e) => toast(&widgets, &friendly::bib_error(&e)),
            }
        });
    }
    dialog.present();
}

fn refresh_list(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    // Recompute the visible set: empty query → all; otherwise the tantivy index (field
    // scoping: author: title: tag: type: year:), falling back to a substring match if the
    // index is absent or the query doesn't parse.
    {
        let mut s = state.borrow_mut();
        let query = s.query.trim().to_string();

        // Base set: entries in the active collection (in collection order), or all.
        let base: Vec<usize> = match &s.collection_filter {
            None => (0..s.entries.len()).collect(),
            Some(slug) => s
                .library
                .as_ref()
                .and_then(|lib| lib.load_collection(slug).ok())
                .map(|coll| {
                    coll.keys
                        .iter()
                        .filter_map(|k| s.key_to_index.get(k).copied())
                        .collect()
                })
                .unwrap_or_default(),
        };

        let visible: Vec<usize> = if query.is_empty() {
            base
        } else {
            let base_set: std::collections::HashSet<usize> = base.iter().copied().collect();
            let matched: Vec<usize> = match s
                .index
                .as_ref()
                .and_then(|idx| idx.search(&query, 2000).ok())
            {
                Some(hits) => hits
                    .iter()
                    .filter_map(|h| s.key_to_index.get(&h.key).copied())
                    .collect(),
                None => {
                    let q = query.to_lowercase();
                    s.entries
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| {
                            e.title.to_lowercase().contains(&q)
                                || e.author.to_lowercase().contains(&q)
                                || e.key.to_lowercase().contains(&q)
                        })
                        .map(|(i, _)| i)
                        .collect()
                }
            };
            matched
                .into_iter()
                .filter(|i| base_set.contains(i))
                .collect()
        };
        s.visible = visible;
    }

    // Refill the spreadsheet's backing store — display order only (matches the old load/
    // relevance order); the `ColumnView`'s own sorter, not this order, decides what's shown
    // on screen, and survives the refill untouched.
    widgets.store.remove_all();
    let has_rows = {
        let s = state.borrow();
        for &idx in &s.visible {
            widgets.store.append(&EntryRow::new(idx, &s.entries[idx]));
        }
        !s.visible.is_empty()
    };

    // Select the top row (in current sorted order) so the detail pane always reflects the
    // current list.
    if has_rows {
        widgets.selection.set_selected(0);
        if let Some(row) = widgets.selection.selected_item().and_downcast::<EntryRow>() {
            show_detail(state, widgets, row.idx());
        }
    } else {
        clear_box(&widgets.detail);
        show_empty_list_hint(state, widgets);
    }
}

/// A friendly stand-in for the (otherwise blank) detail pane when the spreadsheet has no
/// rows to select — distinguishing "this library has nothing in it yet" (a first-time
/// user's likely next question is "how do I add something?") from "nothing matches the
/// current search or collection" (a different, much smaller problem).
fn show_empty_list_hint(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    let library_is_empty = state.borrow().entries.is_empty();

    let page = adw::StatusPage::new();
    page.set_vexpand(true);
    if library_is_empty {
        page.set_icon_name(Some("list-add-symbolic"));
        page.set_title("This library is empty");
        page.set_description(Some(
            "Add a reference by DOI/ISBN, drop a PDF onto the window, or fill in the details \
             yourself.",
        ));
        let buttons = gtk4::Box::new(Orientation::Horizontal, 8);
        buttons.set_halign(gtk4::Align::Center);
        let acquire = gtk4::Button::with_label("Acquire…");
        acquire.add_css_class("suggested-action");
        acquire.add_css_class("pill");
        let new_item = gtk4::Button::with_label("New item…");
        new_item.add_css_class("pill");
        buttons.append(&acquire);
        buttons.append(&new_item);
        page.set_child(Some(&buttons));
        {
            let state = state.clone();
            let widgets = widgets.clone();
            acquire.connect_clicked(move |_| show_acquire_dialog(&state, &widgets));
        }
        {
            let state = state.clone();
            let widgets = widgets.clone();
            new_item.connect_clicked(move |_| show_new_item_dialog(&state, &widgets));
        }
    } else {
        page.set_icon_name(Some("edit-find-symbolic"));
        page.set_title("No matches");
        page.set_description(Some(
            "Nothing here matches your search or the selected collection.",
        ));
    }

    widgets.detail.append(&page);
}

/// Layout for one `ColumnViewColumn`: header text, whether it should expand to fill leftover
/// space, and a fixed width (ignored when `expand` is set).
struct ColumnSpec {
    title: String,
    expand: bool,
    width: i32,
}

/// Add a plain read-only text column bound to one `EntryRow` field. Editing lives entirely
/// in the detail pane on the right now (see `show_detail`) — the spreadsheet is for
/// scanning and sorting, not for typing into; a stray click that used to open an inline
/// editor here now just selects the row like every other column already does.
fn add_text_column(
    column_view: &gtk4::ColumnView,
    spec: ColumnSpec,
    get: fn(&EntryRow) -> String,
) -> gtk4::ColumnViewColumn {
    add_text_column_with(column_view, spec, Rc::new(get))
}

/// Same as `add_text_column`, but takes a boxed closure rather than a bare fn pointer, so a
/// column can close over data that doesn't exist until runtime — e.g. one custom field's
/// name, for the per-field optional columns `sync_custom_field_columns` adds.
fn add_text_column_with(
    column_view: &gtk4::ColumnView,
    spec: ColumnSpec,
    get: Rc<dyn Fn(&EntryRow) -> String>,
) -> gtk4::ColumnViewColumn {
    let factory = gtk4::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
            return;
        };
        let label = gtk4::Label::new(None);
        label.set_xalign(0.0);
        label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        item.set_child(Some(&label));
    });
    {
        let get = get.clone();
        factory.connect_bind(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
                return;
            };
            if let (Some(row), Some(label)) = (
                item.item().and_downcast::<EntryRow>(),
                item.child().and_downcast::<gtk4::Label>(),
            ) {
                label.set_text(&get(&row));
                // So a right-click anywhere on the row (`row_key_at`, wired in `build()`)
                // can resolve which entry was clicked regardless of which column it landed
                // on — `GtkColumnView`'s row widget is a private type with no public way to
                // ask "what item is this", so each cell carries its own key instead.
                unsafe { label.set_data("row-key", row.key()) };
            }
        });
    }
    let column = gtk4::ColumnViewColumn::new(Some(&spec.title), Some(factory));
    column.set_expand(spec.expand);
    if spec.width > 0 {
        column.set_fixed_width(spec.width);
    }
    // An expanding column (currently just Title) fills whatever space the fixed-width
    // columns around it don't use, so it shouldn't also be independently draggable — the two
    // sizing modes fight each other: `GtkColumnView` keeps recomputing an expand column's
    // width from leftover space on every relayout, so a manual drag on *its* border gets
    // overridden mid-drag and reads as the column moving opposite to the pointer. Only the
    // fixed columns are user-resizable; resizing one of those changes how much room is left
    // for Title to expand into, which is the correct (and non-fighting) way to affect it.
    column.set_resizable(!spec.expand);
    let sorter = gtk4::CustomSorter::new(move |a, b| {
        let a = a
            .downcast_ref::<EntryRow>()
            .map(|r| get(r))
            .unwrap_or_default();
        let b = b
            .downcast_ref::<EntryRow>()
            .map(|r| get(r))
            .unwrap_or_default();
        a.to_lowercase().cmp(&b.to_lowercase()).into()
    });
    column.set_sorter(Some(&sorter));
    column_view.append_column(&column);
    column
}

/// Build the entries spreadsheet: a `ColumnView` over a `SortListModel`/`SingleSelection`
/// wrapping a `gio::ListStore<EntryRow>`. Columns: citation key (read-only — it's also the
/// on-disk filename), title/author/year (read-only — edit those in the detail pane), and a
/// compact PDF/EPUB availability indicator. Clicking a column header sorts by it,
/// spreadsheet-style; the `ListStore` itself stays in `AppState.visible` order and is only
/// ever cleared/refilled by `refresh_list` — sort order lives entirely in the `ColumnView`'s
/// own sorter, so it survives a refill (a re-filter or a reload after an edit) without
/// needing to be reapplied.
fn build_entries_column_view() -> (
    gtk4::ColumnView,
    gio::ListStore,
    gtk4::SingleSelection,
    gtk4::SortListModel,
) {
    let store = gio::ListStore::new::<EntryRow>();

    let column_view = gtk4::ColumnView::new(None::<gtk4::SingleSelection>);
    column_view.add_css_class("fond-list");
    column_view.set_show_row_separators(true);
    // Drag a column header to reorder it — native GTK4 column-view behaviour, no extra
    // wiring needed. `build()` restores the saved order/visibility after this function
    // returns and wires up persisting further changes (see `column_by_id`/`reorder_columns`).
    column_view.set_reorderable(true);

    // Citation key: monospace, read-only, and the drag source for adding an entry to a
    // collection (see `refresh_collections`'s `DropTarget`) — same drag behaviour the old
    // card row offered, moved onto the one column that can't be accidentally entered into
    // edit mode by the drag gesture's initial click.
    let key_factory = gtk4::SignalListItemFactory::new();
    key_factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
            return;
        };
        let label = gtk4::Label::new(None);
        label.set_xalign(0.0);
        label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        label.add_css_class("monospace");
        label.add_css_class("dim-label");
        let drag = gtk4::DragSource::new();
        drag.set_actions(gdk::DragAction::COPY);
        {
            let item = item.clone();
            drag.connect_prepare(move |_, _, _| {
                item.item()
                    .and_downcast::<EntryRow>()
                    .map(|r| gdk::ContentProvider::for_value(&r.key().to_value()))
            });
        }
        label.add_controller(drag);
        item.set_child(Some(&label));
    });
    key_factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
            return;
        };
        if let (Some(row), Some(label)) = (
            item.item().and_downcast::<EntryRow>(),
            item.child().and_downcast::<gtk4::Label>(),
        ) {
            label.set_text(&row.key());
            label.set_tooltip_text(Some(
                "Citation key — a short ID for this reference, used to cite it in Typst \
                 documents. Also its file name on disk.",
            ));
            unsafe { label.set_data("row-key", row.key()) };
        }
    });
    let key_column = gtk4::ColumnViewColumn::new(Some("Key"), Some(key_factory));
    key_column.set_id(Some("key"));
    key_column.set_fixed_width(150);
    key_column.set_resizable(true);
    let key_sorter = gtk4::CustomSorter::new(move |a, b| {
        let a = a
            .downcast_ref::<EntryRow>()
            .map(EntryRow::key)
            .unwrap_or_default();
        let b = b
            .downcast_ref::<EntryRow>()
            .map(EntryRow::key)
            .unwrap_or_default();
        a.cmp(&b).into()
    });
    key_column.set_sorter(Some(&key_sorter));
    column_view.append_column(&key_column);

    add_text_column(
        &column_view,
        ColumnSpec {
            title: "Title".into(),
            expand: true,
            width: 0,
        },
        EntryRow::title,
    )
    .set_id(Some("title"));
    add_text_column(
        &column_view,
        ColumnSpec {
            title: "Author".into(),
            expand: false,
            width: 180,
        },
        EntryRow::author,
    )
    .set_id(Some("author"));
    add_text_column(
        &column_view,
        ColumnSpec {
            title: "Year".into(),
            expand: false,
            width: 70,
        },
        EntryRow::year,
    )
    .set_id(Some("year"));
    // Tags/status: like the built-in fields above but off by default (most libraries won't
    // want every optional column cluttering the sheet at once) — toggled on via the Columns
    // dialog (`win.columns`), same mechanism as per-library custom-field columns
    // (`sync_custom_field_columns`).
    let tags_column = add_text_column(
        &column_view,
        ColumnSpec {
            title: "Tags".into(),
            expand: false,
            width: 160,
        },
        EntryRow::tags,
    );
    tags_column.set_id(Some("tags"));
    tags_column.set_visible(false);
    let status_column = add_text_column(
        &column_view,
        ColumnSpec {
            title: "Status".into(),
            expand: false,
            width: 90,
        },
        EntryRow::status,
    );
    status_column.set_id(Some("status"));
    status_column.set_visible(false);

    // Formats: a compact, read-only PDF/EPUB availability indicator — the same "PDF"/"EPUB"
    // language the detail pane's own attachment rows and Read button use.
    let formats_factory = gtk4::SignalListItemFactory::new();
    formats_factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
            return;
        };
        let b = gtk4::Box::new(Orientation::Horizontal, 4);
        b.set_halign(gtk4::Align::Start);
        item.set_child(Some(&b));
    });
    formats_factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
            return;
        };
        let (Some(row), Some(b)) = (
            item.item().and_downcast::<EntryRow>(),
            item.child().and_downcast::<gtk4::Box>(),
        ) else {
            return;
        };
        while let Some(child) = b.first_child() {
            b.remove(&child);
        }
        unsafe { b.set_data("row-key", row.key()) };
        if row.has_pdf() {
            let icon = gtk4::Image::from_icon_name("x-office-document-symbolic");
            icon.set_pixel_size(14);
            icon.add_css_class("dim-label");
            icon.set_tooltip_text(Some("PDF available"));
            b.append(&icon);
        }
        if row.has_epub() {
            // Adwaita has no dedicated e-book glyph — a plain document icon distinguishable
            // from the PDF one (`x-office-document-symbolic`) is the closest available,
            // backed up by the tooltip and the detail pane's own labelled attachment rows.
            let icon = gtk4::Image::from_icon_name("text-x-generic-symbolic");
            icon.set_pixel_size(14);
            icon.add_css_class("dim-label");
            icon.set_tooltip_text(Some("EPUB available"));
            b.append(&icon);
        }
    });
    let formats_column = gtk4::ColumnViewColumn::new(Some("Files"), Some(formats_factory));
    formats_column.set_id(Some("files"));
    formats_column.set_fixed_width(60);
    column_view.append_column(&formats_column);

    let sort_model = gtk4::SortListModel::new(Some(store.clone()), column_view.sorter());
    let selection = gtk4::SingleSelection::new(Some(sort_model.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    column_view.set_model(Some(&selection));

    (column_view, store, selection, sort_model)
}

fn column_by_id(column_view: &gtk4::ColumnView, id: &str) -> Option<gtk4::ColumnViewColumn> {
    let columns = column_view.columns();
    for i in 0..columns.n_items() {
        let col = columns.item(i).and_downcast::<gtk4::ColumnViewColumn>()?;
        if col.id().as_deref() == Some(id) {
            return Some(col);
        }
    }
    None
}

/// Restore a saved left-to-right column order: walk `order`'s ids in sequence and push each
/// one (that still exists) to the end of the column view — after the whole list, the columns
/// mentioned in `order` end up in that order, with anything not mentioned (e.g. a custom
/// field added since the config was last saved) left in its original relative position,
/// trailing after them.
fn reorder_columns(column_view: &gtk4::ColumnView, order: &[String]) {
    for id in order {
        if let Some(col) = column_by_id(column_view, id) {
            column_view.remove_column(&col);
            column_view.append_column(&col);
        }
    }
}

/// Apply saved visibility to the two built-in optional columns (Tags/Status). Per-library
/// custom-field columns get their visibility set at creation time instead, in
/// `sync_custom_field_columns`.
fn apply_column_visibility(column_view: &gtk4::ColumnView, config: &Config) {
    for id in ["tags", "status"] {
        if let Some(col) = column_by_id(column_view, id) {
            col.set_visible(config.column_visible.get(id).copied().unwrap_or(false));
        }
    }
}

/// Rebuild the per-library custom-field spreadsheet columns to match `defs` — removing
/// whatever the previous library (or previous custom-fields edit) had added, in
/// `existing`, and appending a fresh column per current definition. Off by default, same as
/// Tags/Status, unless the config says otherwise. Called on every library open and again
/// after the Custom Fields dialog saves, so renames/additions/removals show up without
/// requiring a reopen.
fn sync_custom_field_columns(
    column_view: &gtk4::ColumnView,
    existing: &Rc<RefCell<Vec<gtk4::ColumnViewColumn>>>,
    defs: &fond_bib::CustomFieldDefs,
    config: &Config,
) {
    for col in existing.borrow_mut().drain(..) {
        column_view.remove_column(&col);
    }
    for def in &defs.fields {
        let id = format!("custom:{}", def.name);
        let name = def.name.clone();
        let column = add_text_column_with(
            column_view,
            ColumnSpec {
                title: def.name.clone(),
                expand: false,
                width: 120,
            },
            Rc::new(move |row: &EntryRow| row.custom_field(&name)),
        );
        column.set_id(Some(&id));
        column.set_visible(config.column_visible.get(&id).copied().unwrap_or(false));
        existing.borrow_mut().push(column);
    }
    reorder_columns(column_view, &config.column_order);
}

/// Prepend a checkbox column for bulk-select mode (see the header's "Select multiple" toggle
/// and the bulk-action bar). Hidden by default — the caller shows/hides it alongside the bar.
///
/// The checkbox's `toggled` handler is wired once per `ListItem` in `connect_setup`, not per
/// bind: `ListItem`s are recycled as rows scroll in/out, so a handler wired in `connect_bind`
/// would stack a new copy on the same long-lived `CheckButton` every recycle. Instead the
/// setup-time handler reads `item.item()` (the *currently* bound row) at click time — same
/// idiom the key column already uses for its drag source.
fn add_bulk_select_column(
    column_view: &gtk4::ColumnView,
    state: &Rc<RefCell<AppState>>,
    on_change: Rc<dyn Fn()>,
) -> gtk4::ColumnViewColumn {
    let factory = gtk4::SignalListItemFactory::new();
    {
        let state = state.clone();
        factory.connect_setup(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
                return;
            };
            let check = gtk4::CheckButton::new();
            {
                let item = item.clone();
                let state = state.clone();
                let on_change = on_change.clone();
                check.connect_toggled(move |c| {
                    let Some(row) = item.item().and_downcast::<EntryRow>() else {
                        return;
                    };
                    let key = row.key();
                    if c.is_active() {
                        state.borrow_mut().bulk_selected.insert(key);
                    } else {
                        state.borrow_mut().bulk_selected.remove(&key);
                    }
                    on_change();
                });
            }
            item.set_child(Some(&check));
        });
    }
    {
        let state = state.clone();
        factory.connect_bind(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
                return;
            };
            if let (Some(row), Some(check)) = (
                item.item().and_downcast::<EntryRow>(),
                item.child().and_downcast::<gtk4::CheckButton>(),
            ) {
                check.set_active(state.borrow().bulk_selected.contains(&row.key()));
            }
        });
    }
    let column = gtk4::ColumnViewColumn::new(None, Some(factory));
    column.set_id(Some("select"));
    column.set_fixed_width(32);
    column.set_resizable(false);
    column_view.insert_column(0, &column);
    column
}

fn show_detail(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, entry_idx: usize) {
    let s = state.borrow();
    let Some(library) = s.library.as_ref() else {
        return;
    };
    let Some(summary) = s.entries.get(entry_idx) else {
        return;
    };
    let key = summary.key.clone();

    let b = &widgets.detail;
    clear_box(b);

    // Load the note once (used for the action row, fields, and prose below).
    let note = library.load_note(&key).ok().flatten();

    // `title_text` stays a plain string (not the editable widget's live value) — it's what
    // several action-button closures below capture for window titles/tooltips, computed
    // once at render time same as before.
    let title_text = if summary.title.is_empty() {
        key.as_str()
    } else {
        summary.title.as_str()
    };

    // Title, editable in place: a bare `Entry` styled to read like the heading it replaces
    // (no dialog needed to fix a typo in a title). Author/year, previously a read-only
    // byline here, are folded into the inline citation-fields form below instead, next to
    // the rest of the bibliographic fields they belong with.
    let current_fields = library
        .load_entry(&key)
        .ok()
        .map(|p| fond_bib::entry::read_fields(&p.entry))
        .unwrap_or_default();

    let title_entry = gtk4::Entry::new();
    title_entry.set_text(if current_fields.title.is_empty() {
        &key
    } else {
        &current_fields.title
    });
    title_entry.add_css_class("title-2");
    title_entry.add_css_class("fond-inline-title");
    title_entry.set_has_frame(false);
    title_entry.set_hexpand(true);
    b.append(&title_entry);

    // Type choices: the shared ITEM_TYPES list, plus the entry's own type appended if it is
    // something not in that list (so an exotic type round-trips instead of being silently
    // changed) — same fallback `show_citation_editor` used.
    let mut type_choices: Vec<(String, String)> = ITEM_TYPES
        .iter()
        .map(|(l, t)| (l.to_string(), t.to_string()))
        .collect();
    if !current_fields.entry_type.is_empty()
        && !type_choices
            .iter()
            .any(|(_, t)| t == &current_fields.entry_type)
    {
        type_choices.push((
            current_fields.entry_type.clone(),
            current_fields.entry_type.clone(),
        ));
    }
    let type_labels: Vec<&str> = type_choices.iter().map(|(l, _)| l.as_str()).collect();
    let type_drop = gtk4::DropDown::from_strings(&type_labels);
    type_drop.set_selected(
        type_choices
            .iter()
            .position(|(_, t)| t == &current_fields.entry_type)
            .unwrap_or(0) as u32,
    );

    let authors_entry = gtk4::Entry::builder()
        .text(current_fields.authors.replace('\n', "; "))
        .placeholder_text("Last, First; Last, First")
        .build();
    let year_entry = gtk4::Entry::builder().text(&current_fields.year).build();
    let publisher_entry = gtk4::Entry::builder()
        .text(&current_fields.publisher)
        .build();
    let doi_entry = gtk4::Entry::builder().text(&current_fields.doi).build();
    let isbn_entry = gtk4::Entry::builder().text(&current_fields.isbn).build();

    // One save path for the whole citation-fields form: rebuilds a full `EntryFields` from
    // every widget's *current* value (not just whichever one triggered the save) and hands
    // it to `Library::edit_fields`, which diffs against the entry's on-disk state itself and
    // writes only what changed. Skips the write (and the reload it would trigger) entirely
    // when nothing actually differs from `current_fields` — every field commits on its own
    // focus-out/Enter, so tabbing through an unedited form must not fire a write per field.
    let save_citation: Rc<dyn Fn()> = {
        let state = state.clone();
        let widgets = widgets.clone();
        let key = key.clone();
        let current_fields = current_fields.clone();
        let type_choices = type_choices.clone();
        let type_drop = type_drop.clone();
        let title_entry = title_entry.clone();
        let authors_entry = authors_entry.clone();
        let year_entry = year_entry.clone();
        let publisher_entry = publisher_entry.clone();
        let doi_entry = doi_entry.clone();
        let isbn_entry = isbn_entry.clone();
        Rc::new(move || {
            let entry_type = type_choices
                .get(type_drop.selected() as usize)
                .map(|(_, t)| t.clone())
                .unwrap_or_else(|| current_fields.entry_type.clone());
            let authors_field = authors_entry
                .text()
                .split([';', '\n'])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let edited = fond_bib::entry::EntryFields {
                entry_type,
                title: title_entry.text().trim().to_string(),
                authors: authors_field,
                year: year_entry.text().trim().to_string(),
                publisher: publisher_entry.text().trim().to_string(),
                doi: doi_entry.text().trim().to_string(),
                isbn: isbn_entry.text().trim().to_string(),
            };
            if edited == current_fields {
                return;
            }
            let result = {
                let s = state.borrow();
                s.library.as_ref().map(|lib| lib.edit_fields(&key, &edited))
            };
            match result {
                Some(Ok(())) => {
                    rebuild_index_silent(&state);
                    reload_current(&state, &widgets);
                    select_key(&state, &widgets, &key);
                }
                Some(Err(e)) => toast(&widgets, &friendly::bib_error(&e)),
                None => {}
            }
        })
    };
    for entry in [
        &title_entry,
        &authors_entry,
        &year_entry,
        &publisher_entry,
        &doi_entry,
        &isbn_entry,
    ] {
        let save = save_citation.clone();
        entry.connect_activate(move |_| save());
        let save = save_citation.clone();
        let focus = gtk4::EventControllerFocus::new();
        focus.connect_leave(move |_| save());
        entry.add_controller(focus);
    }
    {
        let save = save_citation.clone();
        type_drop.connect_selected_notify(move |_| save());
    }

    // First present attachment of each format Kartoteka has a built-in reader for. Previously
    // this was one untyped `find_map` over *any* attachment (an EPUB attachment was picked up
    // identically to a PDF one, so "Read" opened `show_pdf_reader` against EPUB bytes, which
    // PDFium can't parse: a blank "Page 1 of 1" window with no error — M5-SPEC.md 5A) and
    // then, once typed, still only the *first* readable attachment of *any* kind (M5-SPEC.md
    // Tier 4) — an entry with both a PDF and an EPUB of the same work only ever exposed
    // whichever the attachments list happened to list first, silently. Looking up each kind
    // independently lets "Read" offer a chooser when both are present, and lets
    // "Annotations…" route each row's "Go to" to the format that specific annotation actually
    // anchors on (`page` vs `chapter`) instead of whichever kind the dialog happened to open
    // with. Two attachments of the *same* kind (two PDFs) isn't a supported case — the
    // sidecar's single `pdf_hash` field can't disambiguate between them — so this still takes
    // the first match per kind, not a full list.
    let readable_attachment_of = |wanted: ReaderAttachmentKind| {
        note.as_ref().and_then(|n| {
            n.frontmatter.attachments.iter().find_map(|att| {
                let hex = att
                    .hash
                    .split_once(':')
                    .map(|(_, h)| h)
                    .unwrap_or(&att.hash);
                let path = library.attachment_blob_path(hex);
                if !path.exists() || detect_attachment_kind(&att.filename, &path) != Some(wanted) {
                    return None;
                }
                Some((path, att.filename.clone(), att.hash.clone()))
            })
        })
    };
    let pdf_attachment = readable_attachment_of(ReaderAttachmentKind::Pdf);
    let epub_attachment = readable_attachment_of(ReaderAttachmentKind::Epub);
    // Still untyped: used only for "Open externally", which works for any file type via the
    // system file launcher and shouldn't be limited to PDF/EPUB.
    let present_any_attachment = note.as_ref().and_then(|n| {
        n.frontmatter.attachments.iter().find_map(|att| {
            let hex = att
                .hash
                .split_once(':')
                .map(|(_, h)| h)
                .unwrap_or(&att.hash);
            let path = library.attachment_blob_path(hex);
            path.exists()
                .then(|| (path, att.filename.clone(), att.hash.clone()))
        })
    });

    let doi = (!current_fields.doi.is_empty()).then(|| current_fields.doi.clone());

    // Action row: a bounded primary set — the PDF action (Read/Find PDF, contextual), Edit,
    // Cite — plus a "More" popover for everything else. Previously this was a single
    // non-wrapping Box that could hold up to eleven buttons (Edit note, Edit citation…,
    // Cite, Read, Annotations…, Open externally, Collections…, Relations…, AI keywords…,
    // Link author…, Locate, Delete…), which forced the whole detail pane to scroll
    // horizontally to reach the later ones at any normal window width. Capping the row at
    // four items — never more, regardless of how many actions an entry has — fixes that
    // structurally rather than just making the overflow prettier.
    let actions = gtk4::Box::new(Orientation::Horizontal, 8);
    actions.set_margin_top(6);

    // Primary: the read action, contextual to whether a PDF/EPUB is attached (and which, or
    // both — M5-SPEC.md Tier 4) or a DOI is known.
    match (pdf_attachment.clone(), epub_attachment.clone()) {
        (Some((path, _filename, hash)), None) => {
            let read_button = gtk4::Button::with_label("Read");
            // Resumes at the saved Progress page, if any — "Read" opening on page 1 every
            // time despite a recorded reading position was the whole gap 5A/M5's Tier 2
            // exists to close.
            let start_page = note
                .as_ref()
                .and_then(|n| n.frontmatter.progress)
                .map(|p| p.page)
                .unwrap_or(1);
            read_button.set_tooltip_text(Some(if start_page > 1 {
                "Open the built-in PDF reader, resuming where you left off"
            } else {
                "Open the built-in PDF reader"
            }));
            let host = KartotekaReaderHost::for_entry(state, widgets, &key);
            let window = widgets.window.clone();
            let title = title_text.to_string();
            read_button.connect_clicked(move |_| {
                show_pdf_reader(&host, &window, &hash, &path, &title, start_page)
            });
            actions.append(&read_button);
        }
        (None, Some((path, _filename, hash))) => {
            let read_button = gtk4::Button::with_label("Read");
            // Resumes at the saved reading position, if any — same Tier 2a resume the PDF
            // "Read" button above already gives.
            let start_progress = note.as_ref().and_then(|n| n.frontmatter.progress);
            read_button.set_tooltip_text(Some(if start_progress.is_some() {
                "Open the built-in EPUB reader, resuming where you left off"
            } else {
                "Open the built-in EPUB reader"
            }));
            let host = KartotekaReaderHost::for_entry(state, widgets, &key);
            let window = widgets.window.clone();
            let title = title_text.to_string();
            read_button.connect_clicked(move |_| {
                show_epub_reader(&host, &window, &hash, &path, &title, None, start_progress);
            });
            actions.append(&read_button);
        }
        (Some((pdf_path, pdf_filename, pdf_hash)), Some((epub_path, epub_filename, epub_hash))) => {
            // Both a PDF and an EPUB are attached (presumably the same work in two formats)
            // — a small chooser instead of silently opening whichever the attachments list
            // happened to list first.
            let read_button = gtk4::MenuButton::builder().label("Read").build();
            read_button.set_tooltip_text(Some(
                "Both a PDF and an EPUB are attached — choose which to open",
            ));
            let (popover, rows) = popover_menu(220);
            let start_progress = note.as_ref().and_then(|n| n.frontmatter.progress);
            let start_page = start_progress.map(|p| p.page).unwrap_or(1);

            let row = popover_button(&format!("PDF — {pdf_filename}"), false);
            {
                let popover = popover.clone();
                let host = KartotekaReaderHost::for_entry(state, widgets, &key);
                let window = widgets.window.clone();
                let title = title_text.to_string();
                row.connect_clicked(move |_| {
                    popover.popdown();
                    show_pdf_reader(&host, &window, &pdf_hash, &pdf_path, &title, start_page);
                });
            }
            rows.append(&row);

            let row = popover_button(&format!("EPUB — {epub_filename}"), false);
            {
                let popover = popover.clone();
                let host = KartotekaReaderHost::for_entry(state, widgets, &key);
                let window = widgets.window.clone();
                let title = title_text.to_string();
                row.connect_clicked(move |_| {
                    popover.popdown();
                    show_epub_reader(
                        &host,
                        &window,
                        &epub_hash,
                        &epub_path,
                        &title,
                        None,
                        start_progress,
                    );
                });
            }
            rows.append(&row);

            read_button.set_popover(Some(&popover));
            actions.append(&read_button);
        }
        (None, None) => {
            if let Some(doi) = doi.clone() {
                let find_button = gtk4::Button::with_label("Find PDF");
                find_button.set_tooltip_text(Some("Search Unpaywall for an open-access PDF"));
                let state = state.clone();
                let widgets = widgets.clone();
                let key = key.clone();
                find_button
                    .connect_clicked(move |_| find_pdf_unpaywall(&state, &widgets, &key, &doi));
                actions.append(&find_button);
            }
        }
    }

    // Edit: the bibliographic fields and tags/status/rating are now editable directly in
    // the fields below (click into a field, no dialog) — this button opens the small notes
    // list (`docs/NOTES-SPEC.md` Tier 1: primary note + any child notes, plus "+ New note")
    // rather than jumping straight to the primary note's own editor the way it used to.
    let edit_button = gtk4::Button::with_label("Edit note…");
    edit_button.set_tooltip_text(Some(
        "Edit this entry's notes: reading progress, citation preferences, tasks, and prose",
    ));
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let key = key.clone();
        edit_button.connect_clicked(move |_| show_notes_list_dialog(&state, &widgets, &key));
    }
    actions.append(&edit_button);

    // Cite: copies the Typst citation key, the thing this app exists to feed into a
    // document — not a technical detail, so it stays on the primary row.
    let cite_button = gtk4::Button::with_label("Cite");
    cite_button.set_tooltip_text(Some(
        "Copy this entry's citation key, to reference it in a Typst document (@key)",
    ));
    {
        let widgets = widgets.clone();
        let key = key.clone();
        cite_button.connect_clicked(move |_| copy_citation(&widgets, &key));
    }
    actions.append(&cite_button);

    // More: everything else, grouped — library organization, then external links, then
    // the destructive action last and set off by its own separator.
    let has_annotations = (pdf_attachment.is_some() || epub_attachment.is_some())
        && library
            .load_annotations(&key)
            .ok()
            .flatten()
            .is_some_and(|s| !s.annotations.is_empty());
    // Promote AI keyword → tag: only offered when there's an ai/<key>.yml sidecar with
    // keywords to offer. One-directional, user-triggered only — see docs/M2-SPEC.md §4's
    // boundary rule; nothing here ever writes back into ai/<key>.yml or runs automatically.
    let ai_keywords = library
        .load_ai(&key)
        .ok()
        .flatten()
        .map(|ai| ai.keywords)
        .unwrap_or_default();
    let more_button = gtk4::MenuButton::builder().label("More").build();
    {
        let (popover, rows) = popover_menu(210);

        let row = popover_button("Collections…", false);
        {
            let popover = popover.clone();
            let state = state.clone();
            let widgets = widgets.clone();
            let key = key.clone();
            row.connect_clicked(move |_| {
                popover.popdown();
                membership_dialog(&state, &widgets, &key);
            });
        }
        rows.append(&row);

        let row = popover_button("Relations…", false);
        {
            let popover = popover.clone();
            let state = state.clone();
            let widgets = widgets.clone();
            let key = key.clone();
            row.connect_clicked(move |_| {
                popover.popdown();
                relations_dialog(&state, &widgets, &key);
            });
        }
        rows.append(&row);

        let row = popover_button("Relations map… (prototype)", false);
        row.set_tooltip_text(Some(
            "Explore this entry's connections visually — click a node to expand its own \
             connections outward",
        ));
        {
            let popover = popover.clone();
            let state = state.clone();
            let widgets = widgets.clone();
            let key = key.clone();
            row.connect_clicked(move |_| {
                popover.popdown();
                show_relations_graph(&state, &widgets, &key);
            });
        }
        rows.append(&row);

        if has_annotations {
            {
                let row = popover_button("Annotations…", false);
                row.set_tooltip_text(Some("Review, jump to, or delete highlights"));
                let popover = popover.clone();
                let state = state.clone();
                let widgets = widgets.clone();
                let key = key.clone();
                let title = title_text.to_string();
                let pdf = pdf_attachment
                    .clone()
                    .map(|(path, _filename, hash)| (hash, path));
                let epub = epub_attachment
                    .clone()
                    .map(|(path, _filename, hash)| (hash, path));
                let host = KartotekaReaderHost::for_entry(&state, &widgets, &key);
                let window = widgets.window.clone();
                let document_id = key.clone();
                row.connect_clicked(move |_| {
                    popover.popdown();
                    show_annotations_dialog(
                        &host,
                        &window,
                        &document_id,
                        pdf.clone(),
                        epub.clone(),
                        &title,
                    );
                });
                rows.append(&row);
            }
        }

        if !ai_keywords.is_empty() {
            let row = popover_button("AI keywords…", false);
            row.set_tooltip_text(Some("Promote AI-suggested keywords into tags"));
            let popover = popover.clone();
            let state = state.clone();
            let widgets = widgets.clone();
            let key = key.clone();
            let ai_keywords = ai_keywords.clone();
            row.connect_clicked(move |_| {
                popover.popdown();
                show_promote_keywords_dialog(&state, &widgets, &key, ai_keywords.clone());
            });
            rows.append(&row);
        }

        // Author → node: create/link a person node for each author (feature §1 author IDs).
        if !summary.author.is_empty() {
            let row = popover_button("Link author…", false);
            row.set_tooltip_text(Some("Create or link a person node for each author"));
            let popover = popover.clone();
            let state = state.clone();
            let widgets = widgets.clone();
            let key = key.clone();
            row.connect_clicked(move |_| {
                popover.popdown();
                link_authors_dialog(&state, &widgets, &key);
            });
            rows.append(&row);
        }

        // Book part (chapter/section) authoring — §book parts. Only offered from a
        // book/anthology entry itself; the resulting part's own "Refresh from source
        // book…" lives on the part, not here.
        if matches!(current_fields.entry_type.as_str(), "book" | "anthology") {
            let row = popover_button("Create book part…", false);
            row.set_tooltip_text(Some(
                "Start a new chapter/section entry that cites this book, e.g. for one \
                 contributor's chapter in an edited collection",
            ));
            let popover = popover.clone();
            let state = state.clone();
            let widgets = widgets.clone();
            let key = key.clone();
            row.connect_clicked(move |_| {
                popover.popdown();
                show_create_book_part_dialog(&state, &widgets, key.clone());
            });
            rows.append(&row);
        }
        if let Some(source_key) = note
            .as_ref()
            .and_then(|n| n.frontmatter.derived_from_book.clone())
        {
            let row = popover_button("Refresh from source book…", false);
            row.set_tooltip_text(Some(
                "Re-pull this part's book-level fields (title, editor, publisher, …) from \
                 the source book — for when the book's own entry was edited since this part \
                 was created",
            ));
            let popover = popover.clone();
            let state = state.clone();
            let widgets = widgets.clone();
            let key = key.clone();
            row.connect_clicked(move |_| {
                popover.popdown();
                refresh_book_part(&state, &widgets, key.clone(), source_key.clone());
            });
            rows.append(&row);
        }

        rows.append(&popover_separator());

        if let Some((path, filename, _)) = present_any_attachment.clone() {
            let row = popover_button("Open externally", false);
            let popover = popover.clone();
            let window = widgets.window.clone();
            row.connect_clicked(move |_| {
                popover.popdown();
                open_pdf(&window, &path, &filename);
            });
            rows.append(&row);
        }
        if let Some(doi) = doi.clone() {
            let row = popover_button("Open DOI", false);
            let popover = popover.clone();
            let window = widgets.window.clone();
            row.connect_clicked(move |_| {
                popover.popdown();
                open_uri(&window, &format!("https://doi.org/{doi}"));
            });
            rows.append(&row);
        }
        {
            let row = popover_button("Google Scholar", false);
            let popover = popover.clone();
            let window = widgets.window.clone();
            let title_q = summary.title.clone();
            row.connect_clicked(move |_| {
                popover.popdown();
                let q = urlencode(&title_q);
                open_uri(
                    &window,
                    &format!("https://scholar.google.com/scholar?q={q}"),
                );
            });
            rows.append(&row);
        }

        rows.append(&popover_separator());

        // Delete: destructive, so it sits last, behind a menu rather than in the always-
        // visible row, and still asks for confirmation before doing anything.
        let row = popover_button("Delete…", true);
        row.set_tooltip_text(Some(
            "Delete this entry and its note, relations, and attachments",
        ));
        {
            let popover = popover.clone();
            let state = state.clone();
            let widgets = widgets.clone();
            let key = key.clone();
            let title = title_text.to_string();
            row.connect_clicked(move |_| {
                popover.popdown();
                confirm_delete_entry(&state, &widgets, &key, &title);
            });
        }
        rows.append(&row);

        more_button.set_popover(Some(&popover));
    }
    actions.append(&more_button);
    b.append(&actions);

    let fields = gtk4::Box::new(Orientation::Vertical, 4);
    fields.set_margin_top(8);
    // Rows that are internal/Typst-specific rather than something a reader of the entry
    // would recognize (the citation key exists to be typed into a document, not to be
    // read) — tucked behind a collapsed disclosure instead of the main field list, so a
    // non-technical user sees a clean card by default. Nothing is removed, just one click
    // further away; see the "Details" expander appended below.
    let details_fields = gtk4::Box::new(Orientation::Vertical, 4);

    // Structured fields from the entry — editable in place (see `save_citation` above);
    // Citation key stays read-only (it's derived, not a field to edit) and tucked in
    // "Details" since it's Typst-specific, not something a reader of the entry needs.
    fields.append(&labeled("Type", &type_drop));
    fields.append(&labeled("Author(s)", &authors_entry));
    fields.append(&labeled("Year", &year_entry));
    fields.append(&labeled("Publisher", &publisher_entry));
    fields.append(&labeled("DOI", &doi_entry));
    fields.append(&labeled("ISBN", &isbn_entry));
    let key_row = field_row("Citation key", &key);
    key_row.set_tooltip_text(Some(
        "Used to cite this work in a Typst document, e.g. @key",
    ));
    details_fields.append(&key_row);

    // Note-derived state: tags/status/rating (editable in place, below), attachments,
    // annotations, prose.
    let current_tags = note
        .as_ref()
        .map(|n| n.frontmatter.tags.clone())
        .unwrap_or_default();
    let current_status = note.as_ref().and_then(|n| n.frontmatter.read_status);
    let current_rating = note.as_ref().and_then(|n| n.frontmatter.rating);

    // A plain field, not the old facet-grouped chip display (`tags_row`/`chip_group`,
    // removed) — inline click-to-edit needs one widget that's both the display and the
    // editor, and chips aren't that. `facet:value` syntax still works when typed here, just
    // without the grouped/captioned rendering; worth revisiting if a flat list gets hard to
    // scan again once facets and plain tags mix, the original reason chips existed.
    let tags_entry = gtk4::Entry::builder()
        .text(current_tags.join(", "))
        .placeholder_text("comma, separated, tags")
        .build();
    let status_drop = gtk4::DropDown::from_strings(&["(none)", "unread", "reading", "read"]);
    status_drop.set_selected(match current_status {
        None => 0,
        Some(fond_bib::ReadStatus::Unread) => 1,
        Some(fond_bib::ReadStatus::Reading) => 2,
        Some(fond_bib::ReadStatus::Read) => 3,
    });
    let rating_drop = gtk4::DropDown::from_strings(&["(none)", "1", "2", "3", "4", "5"]);
    rating_drop.set_selected(current_rating.map(|r| r as u32).unwrap_or(0));

    // Library-wide custom fields (§ custom fields): one row per definition, seeded from
    // this entry's own note (empty if it's never had a value). All three types use a plain
    // `Entry` — Number isn't a stepper because most custom numeric fields aren't naturally
    // "step from what's already there" (a page count, an alternate rating scale, …), and
    // Tag is comma-separated exactly like the built-in Tags field above.
    let custom_defs = library
        .load_custom_field_defs()
        .map(|d| d.fields)
        .unwrap_or_default();
    let current_custom: HashMap<String, String> = note
        .as_ref()
        .map(|n| n.frontmatter.custom_fields.clone())
        .unwrap_or_default();
    let custom_entries: Vec<(String, gtk4::Entry, fond_bib::CustomFieldType)> = custom_defs
        .iter()
        .map(|def| {
            let value = current_custom.get(&def.name).cloned().unwrap_or_default();
            let entry = gtk4::Entry::builder().text(&value).build();
            match def.field_type {
                fond_bib::CustomFieldType::Tag => {
                    entry.set_placeholder_text(Some("comma, separated, values"));
                }
                fond_bib::CustomFieldType::Date => {
                    entry.set_placeholder_text(Some("YYYY-MM-DD"));
                }
                fond_bib::CustomFieldType::Text | fond_bib::CustomFieldType::Number => {}
            }
            (def.name.clone(), entry, def.field_type)
        })
        .collect();

    // Same shape as `save_citation`: rebuild the whole editable subset from live widget
    // state, skip the write if it matches what was on disk at render time, otherwise
    // load-mutate-write the note fresh (not the possibly-stale `note` this closure closes
    // over) so fields this form doesn't manage — prose, attachments, progress, cite
    // preferences, tasks — always round-trip untouched.
    let save_note_fields: Rc<dyn Fn()> = {
        let state = state.clone();
        let widgets = widgets.clone();
        let key = key.clone();
        let current_tags = current_tags.clone();
        let tags_entry = tags_entry.clone();
        let status_drop = status_drop.clone();
        let rating_drop = rating_drop.clone();
        let current_custom = current_custom.clone();
        let custom_entries = custom_entries.clone();
        Rc::new(move || {
            let new_tags: Vec<String> = tags_entry
                .text()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let new_status = match status_drop.selected() {
                1 => Some(fond_bib::ReadStatus::Unread),
                2 => Some(fond_bib::ReadStatus::Reading),
                3 => Some(fond_bib::ReadStatus::Read),
                _ => None,
            };
            let new_rating = match rating_drop.selected() {
                0 => None,
                n => Some(n as u8),
            };
            let new_custom: HashMap<String, String> = custom_entries
                .iter()
                .filter_map(|(name, entry, _)| {
                    let v = entry.text().trim().to_string();
                    (!v.is_empty()).then_some((name.clone(), v))
                })
                .collect();
            if new_tags == current_tags
                && new_status == current_status
                && new_rating == current_rating
                && new_custom == current_custom
            {
                return;
            }
            let result = {
                let s = state.borrow();
                s.library.as_ref().map(|lib| {
                    let mut note = lib.load_note(&key).ok().flatten().unwrap_or_default();
                    note.frontmatter.tags = new_tags;
                    note.frontmatter.read_status = new_status;
                    note.frontmatter.rating = new_rating;
                    note.frontmatter.custom_fields = new_custom.clone();
                    lib.write_note(&key, &note)
                })
            };
            match result {
                Some(Ok(_)) => {
                    rebuild_index_silent(&state);
                    reload_current(&state, &widgets);
                    select_key(&state, &widgets, &key);
                }
                Some(Err(e)) => toast(&widgets, &friendly::bib_error(&e)),
                None => {}
            }
        })
    };
    {
        let save = save_note_fields.clone();
        tags_entry.connect_activate(move |_| save());
        let save = save_note_fields.clone();
        let focus = gtk4::EventControllerFocus::new();
        focus.connect_leave(move |_| save());
        tags_entry.add_controller(focus);
    }
    {
        let save = save_note_fields.clone();
        status_drop.connect_selected_notify(move |_| save());
    }
    {
        let save = save_note_fields.clone();
        rating_drop.connect_selected_notify(move |_| save());
    }
    for (_, entry, _) in &custom_entries {
        let save = save_note_fields.clone();
        entry.connect_activate(move |_| save());
        let save = save_note_fields.clone();
        let focus = gtk4::EventControllerFocus::new();
        focus.connect_leave(move |_| save());
        entry.add_controller(focus);
    }
    fields.append(&labeled("Tags", &tags_entry));
    fields.append(&labeled("Status", &status_drop));
    fields.append(&labeled("Rating", &rating_drop));
    for (name, entry, field_type) in &custom_entries {
        if *field_type == fond_bib::CustomFieldType::Date {
            let row = gtk4::Box::new(Orientation::Horizontal, 4);
            row.append(entry);
            entry.set_hexpand(true);
            let pick = gtk4::MenuButton::builder()
                .icon_name("x-office-calendar-symbolic")
                .tooltip_text("Pick a date")
                .build();
            let calendar = gtk4::Calendar::new();
            let calendar_popover = gtk4::Popover::new();
            calendar_popover.set_child(Some(&calendar));
            pick.set_popover(Some(&calendar_popover));
            {
                let entry = entry.clone();
                let save = save_note_fields.clone();
                let popover = calendar_popover.clone();
                calendar.connect_day_selected(move |cal| {
                    if let Ok(text) = cal.date().format("%Y-%m-%d") {
                        entry.set_text(&text);
                    }
                    popover.popdown();
                    save();
                });
            }
            row.append(&pick);
            fields.append(&labeled(name, &row));
        } else {
            fields.append(&labeled(name, entry));
        }
    }

    let mut note_body = String::new();
    if let Some(note) = &note {
        for att in &note.frontmatter.attachments {
            let hex = att
                .hash
                .split_once(':')
                .map(|(_, h)| h)
                .unwrap_or(&att.hash);
            let present = library.attachment_blob_path(hex).exists();
            let pages = att
                .pages
                .map(|p| format!(", {p} pages"))
                .unwrap_or_default();
            let value = if present {
                format!("{} ({}{})", att.filename, human_size(att.bytes), pages)
            } else {
                format!("{} — missing", att.filename)
            };
            fields.append(&field_row("PDF", &value));
        }
        note_body = note.body.trim().to_string();
    }
    if let Ok(Some(sidecar)) = library.load_annotations(&key) {
        if !sidecar.annotations.is_empty() {
            fields.append(&field_row(
                "Annotations",
                &sidecar.annotations.len().to_string(),
            ));
        }
    }
    // "Used in": the reverse map from scan_usage() — which declared projects' Typst
    // documents cite this key. Derived, so scanned live rather than cached; projects are
    // typically a handful of files, so this is cheap. Nothing to show until a project is
    // declared (vim/git for now — there's no project-creation GUI yet).
    if let Ok(usage) = library.scan_usage() {
        if let Some(uses) = usage.by_key.get(&key) {
            if !uses.is_empty() {
                let text = uses
                    .iter()
                    .map(|(project, path)| {
                        let doc = std::path::Path::new(path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(path);
                        format!("{project} ({doc})")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                details_fields.append(&field_row("Used in", &text));
            }
        }
    }
    b.append(&fields);
    if details_fields.first_child().is_some() {
        let details = gtk4::Expander::new(Some("Details"));
        details.set_margin_top(4);
        details.set_child(Some(&details_fields));
        b.append(&details);
    }

    // Relations: typed edges grouped by predicate, each a wrapped row of link buttons that
    // navigate to the linked entry. Legacy untyped `related` is folded into the "Related"
    // group so both display together (per docs/M2-SPEC.md open item — one merged view).
    {
        use std::collections::BTreeMap;
        // Group target keys by predicate label. BTreeMap keeps a stable predicate order.
        let mut groups: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
        if let Some(n) = &note {
            for r in &n.frontmatter.relations {
                groups
                    .entry(r.predicate.label())
                    .or_default()
                    .push(r.target.clone());
            }
            for rk in &n.frontmatter.related {
                groups
                    .entry(fond_bib::Predicate::Related.label())
                    .or_default()
                    .push(rk.clone());
            }
        }
        for (predicate_label, mut targets) in groups {
            targets.sort();
            targets.dedup();
            let label = gtk4::Label::new(Some(predicate_label));
            label.add_css_class("caption-heading");
            label.add_css_class("dim-label");
            label.set_xalign(0.0);
            label.set_halign(gtk4::Align::Start);
            label.set_margin_top(6);
            b.append(&label);
            let flow = gtk4::FlowBox::new();
            flow.set_selection_mode(gtk4::SelectionMode::None);
            flow.set_max_children_per_line(20);
            flow.set_column_spacing(4);
            flow.set_row_spacing(4);
            for rk in &targets {
                let display = s
                    .key_to_index
                    .get(rk)
                    .map(|&i| {
                        let e = &s.entries[i];
                        if e.title.is_empty() {
                            e.key.clone()
                        } else {
                            e.title.clone()
                        }
                    })
                    // Not an entry — it may be a node slug; show the node label if so.
                    .unwrap_or_else(|| {
                        library
                            .load_node(rk)
                            .ok()
                            .map(|n| n.frontmatter.label)
                            .filter(|l| !l.is_empty())
                            .unwrap_or_else(|| rk.clone())
                    });
                let link = gtk4::Button::with_label(&display);
                link.add_css_class("flat");
                link.set_tooltip_text(Some(rk));
                let state = state.clone();
                let widgets = widgets.clone();
                let rk = rk.clone();
                link.connect_clicked(move |_| select_key(&state, &widgets, &rk));
                flow.insert(&link, -1);
            }
            b.append(&flow);
        }
    }

    // Note prose.
    if !note_body.is_empty() {
        let sep = gtk4::Separator::new(Orientation::Horizontal);
        sep.set_margin_top(6);
        sep.set_margin_bottom(6);
        b.append(&sep);
        let prose = gtk4::Label::new(Some(&note_body));
        prose.set_wrap(true);
        prose.set_xalign(0.0);
        prose.set_halign(gtk4::Align::Start);
        prose.set_selectable(true);
        b.append(&prose);
    }
}

/// Export a collection's bibliography: a formatted reference list, or an annotated Typst
/// document. Choose a collection, CSL style, and format, then a file to save to.
fn show_export_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    let (slugs, names) = {
        let s = state.borrow();
        let Some(library) = s.library.as_ref() else {
            toast(widgets, "Open a library first");
            return;
        };
        let slugs = library.collection_slugs().unwrap_or_default();
        if slugs.is_empty() {
            toast(
                widgets,
                "No collections to export (import from Zotero creates them)",
            );
            return;
        }
        let names: Vec<String> = slugs
            .iter()
            .map(|sl| {
                library
                    .load_collection(sl)
                    .map(|c| c.name)
                    .unwrap_or_else(|_| sl.clone())
            })
            .collect();
        (slugs, names)
    };

    let dialog = adw::Window::new();
    dialog.set_title(Some("Export bibliography"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(460, -1);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let export = gtk4::Button::with_label("Export");
    export.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&export);
    view.add_top_bar(&header);

    let content = gtk4::Box::new(Orientation::Vertical, 10);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let collection = gtk4::DropDown::from_strings(&name_refs);
    let style =
        gtk4::DropDown::from_strings(&["sbl", "chicago-notes", "chicago-author-date", "apa"]);
    let format = gtk4::DropDown::from_strings(&["Reference list (text)", "Annotated (.typ)"]);
    content.append(&labeled("Collection", &collection));
    content.append(&labeled("Style", &style));
    content.append(&labeled("Format", &format));
    view.set_content(Some(&content));
    dialog.set_content(Some(&view));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        export.connect_clicked(move |_| {
            let slug = slugs[collection.selected() as usize].clone();
            let style_name = match style.selected() {
                0 => "sbl",
                1 => "chicago-notes",
                2 => "chicago-author-date",
                _ => "apa",
            };
            let annotated = format.selected() == 1;

            // Render synchronously (fast for a collection).
            let rendered = {
                let s = state.borrow();
                let library = match s.library.as_ref() {
                    Some(l) => l,
                    None => return,
                };
                let csl = match fond_bib::resolve_style(style_name) {
                    Ok(c) => c,
                    Err(e) => {
                        toast(&widgets, &format!("Style error: {e}"));
                        return;
                    }
                };
                if annotated {
                    library.annotated_bibliography_typ(&slug, &csl)
                } else {
                    library
                        .bibliography_for_collection(&slug, &csl, fond_bib::BufWriteFormat::Plain)
                        .map(|entries| {
                            entries
                                .iter()
                                .map(|r| r.text.clone())
                                .collect::<Vec<_>>()
                                .join("\n\n")
                        })
                }
            };
            let content = match rendered {
                Ok(c) => c,
                Err(e) => {
                    toast(&widgets, &format!("Render failed: {e}"));
                    return;
                }
            };

            let default_name = format!("{slug}.{}", if annotated { "typ" } else { "txt" });
            let save = gtk4::FileDialog::builder()
                .title("Save bibliography")
                .initial_name(&default_name)
                .build();
            let widgets = widgets.clone();
            let dialog = dialog.clone();
            let parent = widgets.window.clone();
            save.save(Some(&parent), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        match std::fs::write(&path, &content) {
                            Ok(()) => {
                                toast(&widgets, &format!("Exported to {}", path.display()));
                                dialog.close();
                            }
                            Err(e) => toast(&widgets, &format!("Could not write file: {e}")),
                        }
                    }
                }
            });
        });
    }

    dialog.present();
}

/// Re-render the detail pane for the currently selected row (after an edit).
fn refresh_detail(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    if let Some(row) = widgets.selection.selected_item().and_downcast::<EntryRow>() {
        show_detail(state, widgets, row.idx());
    }
}

/// Which built-in reader (if any) an attachment's filename extension maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReaderAttachmentKind {
    Pdf,
    Epub,
}

impl ReaderAttachmentKind {
    fn from_filename(filename: &str) -> Option<ReaderAttachmentKind> {
        let ext = std::path::Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())?;
        if ext.eq_ignore_ascii_case("pdf") {
            Some(ReaderAttachmentKind::Pdf)
        } else if ext.eq_ignore_ascii_case("epub") {
            Some(ReaderAttachmentKind::Epub)
        } else {
            None
        }
    }
}

/// Detect an attachment's kind: by filename extension first (cheap, no I/O — covers the
/// common case, including `book.kepub.epub`, since `Path::extension()` reads the trailing
/// `.epub` regardless of the `.kepub` in front of it and Kartoteka's EPUB reader is generic
/// ZIP+XML that already tolerates KEPUB's extra Kobo markup with no changes). Falls back to
/// sniffing the blob's own bytes when the extension doesn't say — the one real remaining gap,
/// an extensionless KEPUB as sometimes downloaded raw from Kobo's store. `blob` must already
/// be known to exist (every caller checks `attachment_blob_path(hex).exists()` first).
fn detect_attachment_kind(filename: &str, blob: &std::path::Path) -> Option<ReaderAttachmentKind> {
    ReaderAttachmentKind::from_filename(filename).or_else(|| {
        if fond_doc::looks_like_pdf(blob) {
            Some(ReaderAttachmentKind::Pdf)
        } else if fond_doc::looks_like_epub(blob) {
            Some(ReaderAttachmentKind::Epub)
        } else {
            None
        }
    })
}

/// Whether a (possibly absent) note has a readable (present-on-disk) PDF and/or EPUB
/// attachment — same detection `show_detail` uses for its own Read button, factored out so
/// the entry list's row icon (`EntrySummary::has_pdf`/`has_epub`) can reuse it. Takes an
/// already-loaded note rather than a key, so the entries-loading loop in `open_library` (which
/// needs the note anyway, for tags/status/custom fields) doesn't read each note file twice.
fn attachment_presence(library: &Library, note: Option<&fond_bib::Note>) -> (bool, bool) {
    let has = |wanted: ReaderAttachmentKind| {
        note.is_some_and(|n| {
            n.frontmatter.attachments.iter().any(|att| {
                let hex = att
                    .hash
                    .split_once(':')
                    .map(|(_, h)| h)
                    .unwrap_or(&att.hash);
                let blob = library.attachment_blob_path(hex);
                blob.exists() && detect_attachment_kind(&att.filename, &blob) == Some(wanted)
            })
        })
    };
    (
        has(ReaderAttachmentKind::Pdf),
        has(ReaderAttachmentKind::Epub),
    )
}

/// Open an attachment blob in the system PDF viewer. The blob is content-addressed with no
/// extension, so copy it to a cache file named after the original filename first.
fn open_pdf(window: &adw::ApplicationWindow, blob: &std::path::Path, filename: &str) {
    let cache = glib::user_cache_dir().join("kartoteka").join("open");
    if std::fs::create_dir_all(&cache).is_err() {
        return;
    }
    let target = cache.join(filename);
    if std::fs::copy(blob, &target).is_err() {
        return;
    }
    let launcher = gtk4::FileLauncher::new(Some(&gio::File::for_path(&target)));
    launcher.launch(Some(window), gio::Cancellable::NONE, |_| {});
}

/// Kartoteka's implementation of the reader's [`ReaderHost`] boundary: it resolves a
/// citation key against whatever library is currently open, and routes notifications to the
/// main window's toast overlay.
///
/// The library is looked up per call rather than captured once, deliberately — that is what
/// the reader did inline before this boundary existed, and it means a reader left open
/// across a library switch keeps writing to the library that is open *now*. Preserved as-is
/// here so the extraction changes no behaviour; whether it is the right behaviour is a
/// separate question (see `docs/READER-EXTRACTION.md`).
struct KartotekaReaderHost {
    state: Rc<RefCell<AppState>>,
    widgets: Rc<Widgets>,
    key: String,
}

impl KartotekaReaderHost {
    fn for_entry(
        state: &Rc<RefCell<AppState>>,
        widgets: &Rc<Widgets>,
        key: &str,
    ) -> Rc<dyn ReaderHost> {
        Rc::new(KartotekaReaderHost {
            state: state.clone(),
            widgets: widgets.clone(),
            key: key.to_string(),
        })
    }

    /// Run `f` against the open library, if there is one. Every method here is a no-op
    /// without one, matching the `if let Some(library) = …` guards this replaced.
    fn with_library<T>(&self, f: impl FnOnce(&Library) -> T) -> Option<T> {
        self.state.borrow().library.as_ref().map(f)
    }

    /// Read-modify-write the entry's note frontmatter. Used for the only two fields the
    /// reader touches: reading progress and the page-numbering override.
    fn edit_note(&self, f: impl FnOnce(&mut fond_bib::Note)) {
        self.with_library(|library| {
            if let Ok(Some(mut note)) = library.load_note(&self.key) {
                f(&mut note);
                let _ = library.write_note(&self.key, &note);
            }
        });
    }
}

impl ReaderHost for KartotekaReaderHost {
    fn load_annotations(&self) -> fond_bib::AnnotationSidecar {
        // Absent, unreadable, and "no library open" all collapse to an empty sidecar —
        // exactly what the `.ok().flatten().unwrap_or_else(…)` at each call site did before
        // this boundary existed.
        self.with_library(|library| library.load_annotations(&self.key).ok().flatten())
            .flatten()
            .unwrap_or_else(|| fond_bib::AnnotationSidecar::new(&self.key))
    }

    fn save_annotations(&self, sidecar: &fond_bib::AnnotationSidecar) -> Result<(), String> {
        // "No library open" becomes an Err rather than a silent Ok. The call sites this
        // replaced had a distinct `None` arm reporting exactly that, and collapsing it into
        // success would mean telling the user an annotation was saved when it was not.
        match self.with_library(|library| library.write_annotations(sidecar)) {
            Some(Ok(_)) => Ok(()),
            Some(Err(e)) => Err(e.to_string()),
            None => Err("No open library".to_string()),
        }
    }

    fn save_progress(&self, progress: fond_bib::Progress) {
        self.edit_note(|note| note.frontmatter.progress = Some(progress));
    }

    fn page_label_override(&self) -> Option<fond_bib::PageLabelOverride> {
        self.with_library(|library| library.load_note(&self.key).ok().flatten())
            .flatten()
            .and_then(|note| note.frontmatter.page_label_override)
    }

    fn set_page_label_override(&self, value: Option<fond_bib::PageLabelOverride>) {
        self.edit_note(|note| note.frontmatter.page_label_override = value);
    }

    fn notify(&self, message: &str) {
        toast(&self.widgets, message);
    }
}

/// "Edit note…"'s entry point: a small list of this entry's notes — the primary note
/// (progress/cite/tasks/prose, `show_note_editor`, unchanged) always first, then any child
/// notes (`docs/NOTES-SPEC.md` Tier 1) — with a "+ New note" action. Standalone (item-less)
/// notes aren't reachable from here by design (Tier 1 decided they live only in a future
/// library-wide Notes view, not attached to any one entry).
fn show_notes_list_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, key: &str) {
    if state.borrow().library.is_none() {
        toast(widgets, "Open a library first");
        return;
    }

    let dialog = adw::Window::new();
    dialog.set_title(Some("Notes"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(420, 480);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    let new_btn = gtk4::Button::from_icon_name("list-add-symbolic");
    new_btn.set_tooltip_text(Some("New note"));
    header.pack_start(&new_btn);
    view.add_top_bar(&header);

    let listbox = gtk4::ListBox::new();
    listbox.add_css_class("fond-list");
    listbox.set_selection_mode(gtk4::SelectionMode::Single);
    let scroll = gtk4::ScrolledWindow::new();
    scroll.add_css_class("fond-ground");
    scroll.set_child(Some(&listbox));
    scroll.set_vexpand(true);
    view.set_content(Some(&scroll));
    dialog.set_content(Some(&view));

    // Row 0 is always "Primary note"; row `i+1` is `shown_ids[i]`'s child note.
    let shown_ids = Rc::new(RefCell::new(Vec::<String>::new()));

    let populate: Rc<dyn Fn()> = Rc::new({
        let state = state.clone();
        let listbox = listbox.clone();
        let key = key.to_string();
        let shown_ids = shown_ids.clone();
        move || {
            while let Some(child) = listbox.first_child() {
                listbox.remove(&child);
            }

            let primary_row = gtk4::ListBoxRow::new();
            primary_row.add_css_class("fond-row");
            let primary_title = gtk4::Label::new(Some("Primary note"));
            primary_title.add_css_class("fond-row-title");
            primary_title.set_xalign(0.0);
            primary_title.set_halign(gtk4::Align::Start);
            primary_title.set_margin_top(6);
            primary_title.set_margin_bottom(6);
            primary_title.set_margin_start(8);
            primary_title.set_margin_end(8);
            primary_row.set_child(Some(&primary_title));
            listbox.append(&primary_row);

            let ids: Vec<String> = {
                let s = state.borrow();
                s.library
                    .as_ref()
                    .and_then(|lib| lib.child_note_ids(&key).ok())
                    .unwrap_or_default()
            };
            let mut shown = Vec::new();
            for id in ids {
                let title = {
                    let s = state.borrow();
                    s.library
                        .as_ref()
                        .and_then(|lib| lib.load_child_note(&key, &id).ok())
                        .map(|n| n.title())
                        .unwrap_or_else(|| id.clone())
                };
                let row = gtk4::ListBoxRow::new();
                row.add_css_class("fond-row");
                let label = gtk4::Label::new(Some(&title));
                label.add_css_class("fond-row-title");
                label.set_xalign(0.0);
                label.set_halign(gtk4::Align::Start);
                label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                label.set_margin_top(6);
                label.set_margin_bottom(6);
                label.set_margin_start(8);
                label.set_margin_end(8);
                row.set_child(Some(&label));
                listbox.append(&row);
                shown.push(id);
            }
            *shown_ids.borrow_mut() = shown;
        }
    });

    {
        let state = state.clone();
        let widgets = widgets.clone();
        let key = key.to_string();
        let shown_ids = shown_ids.clone();
        let populate = populate.clone();
        listbox.connect_row_activated(move |_, row| {
            let idx = row.index();
            if idx == 0 {
                show_note_editor(&state, &widgets, &key);
                return;
            }
            let Some(id) = shown_ids.borrow().get((idx - 1) as usize).cloned() else {
                return;
            };
            show_child_note_editor(&state, &widgets, &key, Some(id), populate.clone());
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let key = key.to_string();
        let populate = populate.clone();
        new_btn.connect_clicked(move |_| {
            show_child_note_editor(&state, &widgets, &key, None, populate.clone());
        });
    }

    populate();
    dialog.present();
}

/// A single child note's editor (`docs/NOTES-SPEC.md` Tier 1): tags plus free-text
/// Markdown body — deliberately lighter than the primary note's `show_note_editor`, since
/// progress/cite-prefs/tasks only make sense once per entry. `note_id` is `None` to create a
/// new child note, `Some(id)` to edit an existing one; `on_saved` refreshes the caller's list
/// (the notes-list dialog above) after a save or delete.
fn show_child_note_editor(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    key: &str,
    note_id: Option<String>,
    on_saved: Rc<dyn Fn()>,
) {
    let existing = note_id.as_ref().and_then(|id| {
        let s = state.borrow();
        s.library
            .as_ref()
            .and_then(|lib| lib.load_child_note(key, id).ok())
    });

    let dialog = adw::Window::new();
    dialog.set_title(Some(if note_id.is_some() {
        "Edit note"
    } else {
        "New note"
    }));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(480, 480);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let save = gtk4::Button::with_label("Save");
    save.add_css_class("suggested-action");
    header.pack_start(&cancel);
    if note_id.is_some() {
        let delete_button = gtk4::Button::from_icon_name("user-trash-symbolic");
        delete_button.set_tooltip_text(Some("Delete this note"));
        {
            let state = state.clone();
            let widgets = widgets.clone();
            let dialog = dialog.clone();
            let key = key.to_string();
            let id = note_id.clone().unwrap();
            let on_saved = on_saved.clone();
            delete_button.connect_clicked(move |_| {
                let result = {
                    let s = state.borrow();
                    match s.library.as_ref() {
                        Some(library) => library.delete_child_note(&key, &id),
                        None => return,
                    }
                };
                match result {
                    Ok(()) => {
                        toast(&widgets, "Note deleted");
                        rebuild_index_silent(&state);
                        dialog.close();
                        on_saved();
                    }
                    Err(e) => toast(&widgets, &friendly::bib_error(&e)),
                }
            });
        }
        header.pack_start(&delete_button);
    }
    header.pack_end(&save);
    view.add_top_bar(&header);

    let content = gtk4::Box::new(Orientation::Vertical, 10);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let tags_entry = gtk4::Entry::builder()
        .placeholder_text("comma, separated, tags")
        .build();
    tags_entry.set_text(
        &existing
            .as_ref()
            .map(|n| n.frontmatter.tags.join(", "))
            .unwrap_or_default(),
    );
    content.append(&labeled("Tags", &tags_entry));

    let body = gtk4::TextView::builder()
        .wrap_mode(gtk4::WrapMode::WordChar)
        .left_margin(8)
        .right_margin(8)
        .top_margin(8)
        .bottom_margin(8)
        .build();
    body.buffer()
        .set_text(existing.as_ref().map(|n| n.body.as_str()).unwrap_or(""));
    let body_scroll = gtk4::ScrolledWindow::new();
    body_scroll.set_child(Some(&body));
    body_scroll.set_vexpand(true);
    body_scroll.add_css_class("card");
    content.append(&labeled("Note", &body_scroll));

    view.set_content(Some(&content));
    dialog.set_content(Some(&view));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        let key = key.to_string();
        let note_id = note_id.clone();
        save.connect_clicked(move |_| {
            let tags: Vec<String> = tags_entry
                .text()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let buffer = body.buffer();
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();

            let mut note = existing
                .clone()
                .unwrap_or_else(|| fond_bib::ExtraNote::new(""));
            note.frontmatter.tags = tags;
            note.body = text;
            note.frontmatter.modified = Some(fond_bib::util::today_iso());

            let result = {
                let s = state.borrow();
                match s.library.as_ref() {
                    Some(library) => {
                        let id = note_id.clone().unwrap_or_else(fond_bib::generate_note_id);
                        library.write_child_note(&key, &id, &note)
                    }
                    None => return,
                }
            };
            match result {
                Ok(_) => {
                    toast(&widgets, "Note saved");
                    rebuild_index_silent(&state);
                    dialog.close();
                    on_saved();
                }
                Err(e) => toast(&widgets, &friendly::bib_error(&e)),
            }
        });
    }

    dialog.present();
}

/// Edit an entry's note: tags, read status, rating, and prose. Writes `notes/<key>.md`.
/// (The bibliographic fields this used to share a dialog with — type, title, authors, year,
/// publisher, DOI, ISBN — are edited inline in the detail pane now; see `save_citation` in
/// `show_detail`. Tags/status/rating moved inline too, via `save_note_fields`. This dialog
/// remains for the fields still without an inline home: progress, cite preferences, tasks,
/// and the free-text note body.)
fn show_note_editor(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, key: &str) {
    let note = {
        let s = state.borrow();
        let Some(library) = s.library.as_ref() else {
            return;
        };
        library.load_note(key).ok().flatten().unwrap_or_default()
    };

    let dialog = adw::Window::new();
    dialog.set_title(Some("Edit note"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(540, 560);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let save = gtk4::Button::with_label("Save");
    save.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&save);
    view.add_top_bar(&header);

    let content = gtk4::Box::new(Orientation::Vertical, 10);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    // Progress (page X of Y) and per-entry citation preferences — both empty-by-default,
    // optional fields that round-tripped on disk only until now.
    let progress_row = gtk4::Box::new(Orientation::Horizontal, 12);
    let progress_page = gtk4::Entry::builder()
        .input_purpose(gtk4::InputPurpose::Digits)
        .width_chars(6)
        .build();
    let progress_of = gtk4::Entry::builder()
        .input_purpose(gtk4::InputPurpose::Digits)
        .width_chars(6)
        .build();
    if let Some(p) = &note.frontmatter.progress {
        progress_page.set_text(&p.page.to_string());
        progress_of.set_text(&p.of.to_string());
    }
    progress_row.append(&labeled("Page", &progress_page));
    progress_row.append(&labeled("Of", &progress_of));
    content.append(&labeled("Progress", &progress_row));

    let cite_row = gtk4::Box::new(Orientation::Horizontal, 12);
    let cite_short = gtk4::Entry::builder()
        .placeholder_text("e.g. Cone, Black Theology")
        .hexpand(true)
        .build();
    cite_short.set_text(note.frontmatter.cite.short.as_deref().unwrap_or(""));
    const CITE_STYLES: &[&str] = &[
        "(none)",
        "sbl",
        "chicago-notes",
        "chicago-author-date",
        "apa",
    ];
    let cite_style = gtk4::DropDown::from_strings(CITE_STYLES);
    let style_idx = note
        .frontmatter
        .cite
        .preferred_style
        .as_deref()
        .and_then(|s| CITE_STYLES.iter().position(|c| *c == s))
        .unwrap_or(0);
    cite_style.set_selected(style_idx as u32);
    cite_row.append(&labeled("Short cite", &cite_short));
    cite_row.append(&labeled("Preferred style", &cite_style));
    content.append(&labeled("Cite", &cite_row));

    // Tasks: a small editable list. Rows are built once from the existing tasks, plus an
    // "Add task" entry that appends a fresh row; Save reads back whatever rows are still
    // present (a deleted row is simply gone from the list), same as the Tags manager.
    let tasks_list = gtk4::ListBox::new();
    tasks_list.set_selection_mode(gtk4::SelectionMode::None);
    tasks_list.add_css_class("fond-list");

    struct TaskRow {
        row: gtk4::ListBoxRow,
        done: gtk4::CheckButton,
        text: gtk4::Entry,
        due: gtk4::Entry,
    }
    let task_rows: Rc<RefCell<Vec<TaskRow>>> = Rc::new(RefCell::new(Vec::new()));

    fn build_task_row(list: &gtk4::ListBox, task: Option<&fond_bib::Task>) -> TaskRow {
        let hbox = gtk4::Box::new(Orientation::Horizontal, 8);
        hbox.set_margin_top(4);
        hbox.set_margin_bottom(4);
        hbox.set_margin_start(8);
        hbox.set_margin_end(8);
        let done = gtk4::CheckButton::new();
        done.set_active(task.map(|t| t.done).unwrap_or(false));
        let text = gtk4::Entry::builder()
            .placeholder_text("Task")
            .hexpand(true)
            .build();
        text.set_text(task.map(|t| t.text.as_str()).unwrap_or(""));
        let due = gtk4::Entry::builder()
            .placeholder_text("due (YYYY-MM-DD)")
            .width_chars(14)
            .build();
        due.set_text(task.and_then(|t| t.due.as_deref()).unwrap_or(""));
        let delete = gtk4::Button::from_icon_name("user-trash-symbolic");
        delete.add_css_class("flat");
        delete.set_tooltip_text(Some("Delete this task"));
        hbox.append(&done);
        hbox.append(&text);
        hbox.append(&due);
        hbox.append(&delete);
        let row = gtk4::ListBoxRow::new();
        row.add_css_class("fond-row");
        row.set_activatable(false);
        row.set_child(Some(&hbox));
        list.append(&row);
        {
            let list = list.clone();
            let row = row.clone();
            delete.connect_clicked(move |_| list.remove(&row));
        }
        TaskRow {
            row,
            done,
            text,
            due,
        }
    }

    for task in &note.frontmatter.tasks {
        task_rows
            .borrow_mut()
            .push(build_task_row(&tasks_list, Some(task)));
    }

    let tasks_scroll = gtk4::ScrolledWindow::new();
    tasks_scroll.add_css_class("fond-ground");
    tasks_scroll.set_child(Some(&tasks_list));
    tasks_scroll.set_max_content_height(160);
    tasks_scroll.set_propagate_natural_height(true);

    let add_task = gtk4::Button::from_icon_name("list-add-symbolic");
    add_task.set_tooltip_text(Some("Add task"));
    add_task.set_halign(gtk4::Align::Start);
    {
        let tasks_list = tasks_list.clone();
        let task_rows = task_rows.clone();
        add_task.connect_clicked(move |_| {
            let new_row = build_task_row(&tasks_list, None);
            new_row.text.grab_focus();
            task_rows.borrow_mut().push(new_row);
        });
    }

    let tasks_section = gtk4::Box::new(Orientation::Vertical, 4);
    tasks_section.append(&tasks_scroll);
    tasks_section.append(&add_task);
    content.append(&labeled("Tasks", &tasks_section));

    let body = gtk4::TextView::builder()
        .wrap_mode(gtk4::WrapMode::WordChar)
        .left_margin(8)
        .right_margin(8)
        .top_margin(8)
        .bottom_margin(8)
        .build();
    body.buffer().set_text(&note.body);
    let body_scroll = gtk4::ScrolledWindow::new();
    body_scroll.set_child(Some(&body));
    body_scroll.set_vexpand(true);
    body_scroll.add_css_class("card");
    content.append(&labeled("Note", &body_scroll));

    view.set_content(Some(&content));
    dialog.set_content(Some(&view));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        let key = key.to_string();
        save.connect_clicked(move |_| {
            // Tags/status/rating aren't managed by this dialog anymore (inline in the detail
            // pane instead) — `note.clone()` already carries them forward unchanged.
            let mut updated = note.clone();

            let page: Option<u32> = progress_page.text().trim().parse().ok();
            let of: Option<u32> = progress_of.text().trim().parse().ok();
            updated.frontmatter.progress = match (page, of) {
                (Some(page), Some(of)) => Some(fond_bib::Progress {
                    page,
                    of,
                    chapter_percent: None,
                }),
                _ => None,
            };

            let short = cite_short.text().trim().to_string();
            updated.frontmatter.cite = fond_bib::CitePrefs {
                short: (!short.is_empty()).then_some(short),
                preferred_style: match cite_style.selected() {
                    0 => None,
                    n => CITE_STYLES.get(n as usize).map(|s| s.to_string()),
                },
            };

            // A row still has a parent iff it wasn't removed by its delete button.
            updated.frontmatter.tasks = task_rows
                .borrow()
                .iter()
                .filter(|t| t.row.parent().is_some())
                .filter_map(|t| {
                    let text = t.text.text().trim().to_string();
                    if text.is_empty() {
                        return None;
                    }
                    let due = t.due.text().trim().to_string();
                    Some(fond_bib::Task {
                        text,
                        done: t.done.is_active(),
                        due: (!due.is_empty()).then_some(due),
                    })
                })
                .collect();

            let buffer = body.buffer();
            updated.body = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();
            if updated.frontmatter.date_added.is_none() {
                updated.frontmatter.date_added = glib::DateTime::now_local()
                    .ok()
                    .and_then(|d| d.format("%Y-%m-%d").ok())
                    .map(|s| s.to_string());
            }

            let result = {
                let s = state.borrow();
                match s.library.as_ref() {
                    Some(library) => library.write_note(&key, &updated),
                    None => return,
                }
            };
            match result {
                Ok(_) => {
                    toast(&widgets, "Note saved");
                    dialog.close();
                    refresh_detail(&state, &widgets);
                }
                Err(e) => toast(&widgets, &friendly::bib_error(&e)),
            }
        });
    }

    dialog.present();
}

/// The node-type choices, in dropdown order, paired with their `NodeType`.
fn node_type_choices() -> [(&'static str, fond_bib::NodeType); 6] {
    use fond_bib::NodeType::*;
    [
        ("Person", Person),
        ("Concept", Concept),
        ("School", School),
        ("Event", Event),
        ("Place", Place),
        ("Uncatalogued work", WorkUncataloged),
    ]
}

/// The human label for a node type (for list rows).
fn node_type_label(t: fond_bib::NodeType) -> &'static str {
    node_type_choices()
        .iter()
        .find(|(_, nt)| *nt == t)
        .map(|(l, _)| *l)
        .unwrap_or("Concept")
}

/// A human display name for a relation `target`: an entry's title, else a node's label, else
/// the raw id (a dangling target). Lets relation lists read as names, not slugs.
fn target_display(lib: &Library, target: &str) -> String {
    if let Ok(parsed) = lib.load_entry(target) {
        if let Some(t) = bibentry::title_string(&parsed.entry) {
            if !t.is_empty() {
                return t;
            }
        }
    }
    if let Ok(node) = lib.load_node(target) {
        if !node.frontmatter.label.is_empty() {
            return node.frontmatter.label;
        }
    }
    target.to_string()
}

/// Group a relation list by predicate label, resolving each target to a display name and
/// sorting for a stable, de-duplicated view. Returns `(predicate label, [display names])`.
fn group_relations_display(
    lib: &Library,
    relations: &[fond_bib::Relation],
) -> Vec<(String, Vec<String>)> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for r in relations {
        groups
            .entry(r.predicate.label())
            .or_default()
            .push(target_display(lib, &r.target));
    }
    groups
        .into_iter()
        .map(|(label, mut names)| {
            names.sort();
            names.dedup();
            (label.to_string(), names)
        })
        .collect()
}

/// Walk up from `widget` (the leaf cell a right-click landed on — a `Label` for most
/// spreadsheet columns, a `Box` for the Files column) to find the citation key stashed as
/// qdata during bind (see `add_text_column_with`, and the Key/Files factories in
/// `build_entries_column_view`). `GtkColumnView`'s row container is a private type with no
/// public "what item is this" API, so every cell carries its own copy of the key instead of
/// relying on one shared row widget to ask.
fn row_key_at(widget: gtk4::Widget) -> Option<String> {
    let mut current = Some(widget);
    while let Some(w) = current {
        if let Some(key) = unsafe { w.data::<String>("row-key") } {
            return Some(unsafe { key.as_ref() }.clone());
        }
        current = w.parent();
    }
    None
}

/// Right-click menu for one entry — shared by the spreadsheet (resolved via `row_key_at` +
/// `ColumnView::pick`) and the Bookshelf grid (which already has the key in hand at bind
/// time). Selects the entry first, so the detail pane matches whatever the menu ends up
/// acting on. Deliberately a subset of the detail pane's "More" popover, not a duplicate of
/// every row action there: Collections… (the existing membership dialog), Create book
/// part… (book/anthology entries only, mirroring the same condition `show_detail`'s "More"
/// popover already uses for it), and Delete… — the handful of actions worth not opening the
/// detail pane first for.
fn show_entry_context_menu(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    parent: &impl IsA<gtk4::Widget>,
    key: &str,
    x: f64,
    y: f64,
) {
    select_key(state, widgets, key);

    let (title, entry_type) = {
        let s = state.borrow();
        match s.library.as_ref().and_then(|lib| lib.load_entry(key).ok()) {
            Some(parsed) => (
                bibentry::title_string(&parsed.entry).unwrap_or_else(|| key.to_string()),
                format!("{:?}", parsed.entry.entry_type()).to_lowercase(),
            ),
            None => (key.to_string(), String::new()),
        }
    };

    let (popover, rows) = popover_menu(200);
    popover.set_parent(parent);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(
        x.round() as i32,
        y.round() as i32,
        1,
        1,
    )));
    popover.set_has_arrow(true);

    let collections_row = popover_button("Collections…", false);
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let popover = popover.clone();
        let key = key.to_string();
        collections_row.connect_clicked(move |_| {
            popover.popdown();
            membership_dialog(&state, &widgets, &key);
        });
    }
    rows.append(&collections_row);

    if matches!(entry_type.as_str(), "book" | "anthology") {
        let part_row = popover_button("Create book part…", false);
        {
            let state = state.clone();
            let widgets = widgets.clone();
            let popover = popover.clone();
            let key = key.to_string();
            part_row.connect_clicked(move |_| {
                popover.popdown();
                show_create_book_part_dialog(&state, &widgets, key.clone());
            });
        }
        rows.append(&part_row);
    }

    let delete_row = popover_button("Delete…", true);
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let popover = popover.clone();
        let key = key.to_string();
        let title = title.clone();
        delete_row.connect_clicked(move |_| {
            popover.popdown();
            confirm_delete_entry(&state, &widgets, &key, &title);
        });
    }
    rows.append(&delete_row);

    popover.popup();
}

/// Rebuild the search index quietly (no toast) so newly created/edited nodes and entries are
/// findable. A no-op if no library is open or the rebuild fails (search just stays stale).
/// Confirm and then delete an entry. Uses an `adw::MessageDialog` with a destructive
/// "Delete" response; on confirmation, removes the entry via the library (note, relations,
/// collection membership, and unshared attachment blobs go with it), rebuilds the search
/// index, and reloads the list.
fn confirm_delete_entry(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    key: &str,
    title: &str,
) {
    let dialog = adw::MessageDialog::new(
        Some(&widgets.window),
        Some("Delete this entry?"),
        Some(&format!(
            "“{title}” and its note, relations, collection membership, and any attachments \
             unique to it will be permanently removed. The underlying files are deleted from \
             the library (use git to recover if needed)."
        )),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let state = state.clone();
    let widgets = widgets.clone();
    let key = key.to_string();
    dialog.connect_response(None, move |dlg, response| {
        dlg.close();
        if response != "delete" {
            return;
        }
        let result = {
            let s = state.borrow();
            s.library
                .as_ref()
                .map(|lib| lib.delete_entry(&key))
                .transpose()
        };
        match result {
            Ok(_) => {
                rebuild_index_silent(&state);
                reload_current(&state, &widgets);
                clear_box(&widgets.detail);
                toast(&widgets, &format!("Deleted {key}"));
            }
            Err(e) => toast(
                &widgets,
                &format!("Couldn't delete \"{key}\": {}", friendly::bib_error(&e)),
            ),
        }
    });
    dialog.present();
}

/// Every key currently checked in bulk-select mode, order unspecified — the bulk actions
/// below don't care about order, only membership.
fn bulk_selected_keys(state: &Rc<RefCell<AppState>>) -> Vec<String> {
    state.borrow().bulk_selected.iter().cloned().collect()
}

/// Small popover (anchored to the button that opened it) with a single tag entry, applied to
/// every checked entry on submit — added to each note's existing tags rather than replacing
/// them, same as typing a new tag into one entry's own Tags field would.
fn show_bulk_tag_popover(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    anchor: &gtk4::Widget,
    on_bulk_change: &Rc<dyn Fn()>,
) {
    let keys = bulk_selected_keys(state);
    if keys.is_empty() {
        toast(widgets, "No entries selected");
        return;
    }

    let popover = gtk4::Popover::new();
    popover.set_parent(anchor);
    let row = gtk4::Box::new(Orientation::Horizontal, 6);
    row.set_margin_top(8);
    row.set_margin_bottom(8);
    row.set_margin_start(8);
    row.set_margin_end(8);
    let entry = gtk4::Entry::builder()
        .placeholder_text("tag, another-tag")
        .build();
    let add = gtk4::Button::with_label("Add");
    add.add_css_class("suggested-action");
    row.append(&entry);
    row.append(&add);
    popover.set_child(Some(&row));

    let apply: Rc<dyn Fn()> = {
        let state = state.clone();
        let widgets = widgets.clone();
        let popover = popover.clone();
        let entry = entry.clone();
        let keys = keys.clone();
        let on_bulk_change = on_bulk_change.clone();
        Rc::new(move || {
            let new_tags: Vec<String> = entry
                .text()
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
            if new_tags.is_empty() {
                return;
            }
            let mut failed = 0usize;
            {
                let s = state.borrow();
                if let Some(lib) = s.library.as_ref() {
                    for key in &keys {
                        let mut note = lib.load_note(key).ok().flatten().unwrap_or_default();
                        for t in &new_tags {
                            if !note.frontmatter.tags.contains(t) {
                                note.frontmatter.tags.push(t.clone());
                            }
                        }
                        if lib.write_note(key, &note).is_err() {
                            failed += 1;
                        }
                    }
                }
            }
            popover.popdown();
            state.borrow_mut().bulk_selected.clear();
            on_bulk_change();
            rebuild_index_silent(&state);
            reload_current(&state, &widgets);
            if failed > 0 {
                toast(
                    &widgets,
                    &format!("Tagged {} entries, {failed} failed", keys.len() - failed),
                );
            } else {
                toast(&widgets, &format!("Tagged {} entries", keys.len()));
            }
        })
    };
    {
        let apply = apply.clone();
        add.connect_clicked(move |_| apply());
    }
    entry.connect_activate(move |_| apply());

    popover.popup();
}

/// Small popover listing every collection as a row to add all checked entries to — no "new
/// collection" option here; create one first via the sidebar's + button, same as adding a
/// single entry to a collection already requires.
fn show_bulk_collection_popover(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    anchor: &gtk4::Widget,
    on_bulk_change: &Rc<dyn Fn()>,
) {
    let keys = bulk_selected_keys(state);
    if keys.is_empty() {
        toast(widgets, "No entries selected");
        return;
    }

    let collections: Vec<(String, String)> = {
        let s = state.borrow();
        let Some(lib) = s.library.as_ref() else {
            return;
        };
        s.collections
            .iter()
            .map(|slug| {
                let name = lib
                    .load_collection(slug)
                    .map(|c| c.name)
                    .unwrap_or_else(|_| slug.clone());
                (slug.clone(), name)
            })
            .collect()
    };
    if collections.is_empty() {
        toast(widgets, "No collections yet — create one first");
        return;
    }

    let popover = gtk4::Popover::new();
    popover.set_parent(anchor);
    let list = gtk4::ListBox::new();
    list.set_selection_mode(gtk4::SelectionMode::None);
    list.set_size_request(200, -1);
    for (slug, name) in &collections {
        let row = gtk4::ListBoxRow::new();
        let label = gtk4::Label::new(Some(name));
        label.set_xalign(0.0);
        label.set_margin_top(6);
        label.set_margin_bottom(6);
        label.set_margin_start(10);
        label.set_margin_end(10);
        row.set_child(Some(&label));
        unsafe { row.set_data("collection-slug", slug.clone()) };
        list.append(&row);
    }
    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_max_content_height(240);
    scroll.set_propagate_natural_height(true);
    scroll.set_child(Some(&list));
    popover.set_child(Some(&scroll));

    {
        let state = state.clone();
        let widgets = widgets.clone();
        let popover = popover.clone();
        let keys = keys.clone();
        let on_bulk_change = on_bulk_change.clone();
        list.connect_row_activated(move |_, row| {
            let Some(slug) = (unsafe { row.data::<String>("collection-slug") }) else {
                return;
            };
            let slug = unsafe { slug.as_ref().clone() };
            let mut failed = 0usize;
            {
                let s = state.borrow();
                if let Some(lib) = s.library.as_ref() {
                    for key in &keys {
                        if lib.add_to_collection(&slug, key).is_err() {
                            failed += 1;
                        }
                    }
                }
            }
            popover.popdown();
            state.borrow_mut().bulk_selected.clear();
            on_bulk_change();
            refresh_collections(&state, &widgets);
            reload_current(&state, &widgets);
            if failed > 0 {
                toast(
                    &widgets,
                    &format!("Added {} entries, {failed} failed", keys.len() - failed),
                );
            } else {
                toast(
                    &widgets,
                    &format!("Added {} entries to collection", keys.len()),
                );
            }
        });
    }

    popover.popup();
}

/// Confirm, then permanently delete every checked entry — same per-entry consequences as
/// `confirm_delete_entry`, just for a batch.
fn confirm_bulk_delete(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    on_bulk_change: &Rc<dyn Fn()>,
) {
    let keys = bulk_selected_keys(state);
    if keys.is_empty() {
        toast(widgets, "No entries selected");
        return;
    }

    let dialog = adw::MessageDialog::new(
        Some(&widgets.window),
        Some(&format!("Delete {} entries?", keys.len())),
        Some(
            "Each entry's note, relations, collection membership, and any attachments unique \
             to it will be permanently removed. The underlying files are deleted from the \
             library (use git to recover if needed).",
        ),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let state = state.clone();
    let widgets = widgets.clone();
    let on_bulk_change = on_bulk_change.clone();
    dialog.connect_response(None, move |dlg, response| {
        dlg.close();
        if response != "delete" {
            return;
        }
        let mut failed = 0usize;
        {
            let s = state.borrow();
            if let Some(lib) = s.library.as_ref() {
                for key in &keys {
                    if lib.delete_entry(key).is_err() {
                        failed += 1;
                    }
                }
            }
        }
        state.borrow_mut().bulk_selected.clear();
        on_bulk_change();
        rebuild_index_silent(&state);
        reload_current(&state, &widgets);
        clear_box(&widgets.detail);
        if failed > 0 {
            toast(
                &widgets,
                &format!("Deleted {} entries, {failed} failed", keys.len() - failed),
            );
        } else {
            toast(&widgets, &format!("Deleted {} entries", keys.len()));
        }
    });
    dialog.present();
}

/// Confirm and delete a knowledge-graph node from its editor. `editor` is the node editor
/// window itself (closed on success, alongside the confirmation dialog); `on_saved` is the
/// Nodes manager's list-refresh callback, reused here since a delete changes that list too.
fn confirm_delete_node(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    editor: &adw::Window,
    slug: &str,
    label: &str,
    on_saved: Rc<dyn Fn()>,
) {
    let dialog = adw::MessageDialog::new(
        Some(editor),
        Some("Delete this node?"),
        Some(&format!(
            "“{label}” and every relation edge naming it will be permanently removed. \
             The underlying file is deleted from the library (use git to recover if needed)."
        )),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let state = state.clone();
    let widgets = widgets.clone();
    let editor = editor.clone();
    let slug = slug.to_string();
    dialog.connect_response(None, move |dlg, response| {
        dlg.close();
        if response != "delete" {
            return;
        }
        let result = {
            let s = state.borrow();
            s.library
                .as_ref()
                .map(|lib| lib.delete_node(&slug))
                .transpose()
        };
        match result {
            Ok(_) => {
                rebuild_index_silent(&state);
                on_saved();
                toast(&widgets, &format!("Deleted {slug}"));
                editor.close();
            }
            Err(e) => toast(
                &widgets,
                &format!("Couldn't delete \"{slug}\": {}", friendly::bib_error(&e)),
            ),
        }
    });
    dialog.present();
}

fn rebuild_index_silent(state: &Rc<RefCell<AppState>>) {
    let rebuilt = {
        let s = state.borrow();
        s.library.as_ref().map(|lib| {
            let dir = lib.root().join(".kartoteka").join("index");
            fond_index::SearchIndex::rebuild(lib, &dir, |_| None, |_| None)
        })
    };
    if let Some(Ok(index)) = rebuilt {
        state.borrow_mut().index = Some(index);
    }
}

/// The knowledge-graph node manager: a filterable list of `nodes/` with a `+` to create one;
/// activating a row opens the node editor. Deletion is intentionally left to vim/git for now
/// (removing a node needs relation cleanup — a later PR).
fn show_nodes_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    if state.borrow().library.is_none() {
        toast(widgets, "Open a library first");
        return;
    }

    let dialog = adw::Window::new();
    dialog.set_title(Some("Nodes"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(520, 600);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    let new_btn = gtk4::Button::from_icon_name("list-add-symbolic");
    new_btn.set_tooltip_text(Some("New node"));
    header.pack_start(&new_btn);
    view.add_top_bar(&header);

    let outer = gtk4::Box::new(Orientation::Vertical, 0);
    let subtitle = gtk4::Label::new(Some(
        "People, places, and other things you can connect your references to.",
    ));
    subtitle.set_wrap(true);
    subtitle.set_xalign(0.0);
    subtitle.add_css_class("dim-label");
    subtitle.add_css_class("caption");
    subtitle.set_margin_top(6);
    subtitle.set_margin_start(8);
    subtitle.set_margin_end(8);
    let search = gtk4::SearchEntry::new();
    search.set_placeholder_text(Some("Filter nodes"));
    search.set_margin_top(6);
    search.set_margin_bottom(6);
    search.set_margin_start(6);
    search.set_margin_end(6);
    let listbox = gtk4::ListBox::new();
    listbox.add_css_class("fond-list");
    listbox.set_selection_mode(gtk4::SelectionMode::Single);
    let scroll = gtk4::ScrolledWindow::new();
    scroll.add_css_class("fond-ground");
    scroll.set_child(Some(&listbox));
    scroll.set_vexpand(true);
    outer.append(&subtitle);
    outer.append(&search);
    outer.append(&scroll);
    view.set_content(Some(&outer));
    dialog.set_content(Some(&view));

    // Slugs currently displayed, in row order — maps a row index back to a node.
    let shown_slugs = Rc::new(RefCell::new(Vec::<String>::new()));

    let populate: Rc<dyn Fn()> = Rc::new({
        let state = state.clone();
        let listbox = listbox.clone();
        let search = search.clone();
        let shown_slugs = shown_slugs.clone();
        move || {
            while let Some(child) = listbox.first_child() {
                listbox.remove(&child);
            }
            let filter = search.text().to_lowercase();
            // Snapshot (slug, frontmatter) under a short borrow, then build rows.
            let nodes: Vec<(String, fond_bib::NodeFrontmatter)> = {
                let s = state.borrow();
                match s.library.as_ref() {
                    Some(lib) => lib
                        .node_slugs()
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|slug| lib.load_node(&slug).ok().map(|n| (slug, n.frontmatter)))
                        .collect(),
                    None => Vec::new(),
                }
            };

            let mut shown = Vec::new();
            for (slug, fm) in nodes {
                if !filter.is_empty() {
                    let hay =
                        format!("{} {} {}", fm.label, slug, fm.aliases.join(" ")).to_lowercase();
                    if !hay.contains(&filter) {
                        continue;
                    }
                }
                let row = gtk4::ListBoxRow::new();
                row.add_css_class("fond-row");
                let b = gtk4::Box::new(Orientation::Vertical, 2);
                b.set_margin_top(6);
                b.set_margin_bottom(6);
                b.set_margin_start(8);
                b.set_margin_end(8);
                let title = gtk4::Label::new(Some(&fm.label));
                title.add_css_class("fond-row-title");
                title.set_xalign(0.0);
                title.set_halign(gtk4::Align::Start);
                let sub = gtk4::Label::new(Some(&format!(
                    "{} · {}",
                    node_type_label(fm.node_type),
                    slug
                )));
                sub.add_css_class("fond-row-meta");
                sub.set_xalign(0.0);
                sub.set_halign(gtk4::Align::Start);
                b.append(&title);
                b.append(&sub);
                row.set_child(Some(&b));
                listbox.append(&row);
                shown.push(slug);
            }

            if shown.is_empty() {
                let row = gtk4::ListBoxRow::new();
                row.set_selectable(false);
                row.set_activatable(false);
                let l = gtk4::Label::new(Some(if filter.is_empty() {
                    "No nodes yet — create one with +"
                } else {
                    "No matching nodes"
                }));
                l.add_css_class("dim-label");
                l.set_margin_top(12);
                l.set_margin_bottom(12);
                row.set_child(Some(&l));
                listbox.append(&row);
            }
            *shown_slugs.borrow_mut() = shown;
        }
    });

    // Activate a row (Enter / double-click) to edit that node.
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let shown_slugs = shown_slugs.clone();
        let populate = populate.clone();
        listbox.connect_row_activated(move |_, row| {
            let idx = row.index();
            if idx < 0 {
                return;
            }
            let slug = shown_slugs.borrow().get(idx as usize).cloned();
            if let Some(slug) = slug {
                show_node_editor(&state, &widgets, Some(slug), populate.clone());
            }
        });
    }
    {
        let populate = populate.clone();
        search.connect_search_changed(move |_| populate());
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let populate = populate.clone();
        new_btn
            .connect_clicked(move |_| show_node_editor(&state, &widgets, None, populate.clone()));
    }

    populate();
    dialog.present();
}

/// Create (`slug == None`) or edit an existing node. On save, an existing node keeps its
/// (stable) slug; a new one gets a collision-free slug derived from the label. Relations are
/// preserved untouched — they're edited elsewhere. `on_saved` refreshes the caller's list.
fn show_node_editor(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    slug: Option<String>,
    on_saved: Rc<dyn Fn()>,
) {
    // Load the existing node (edit) or start from defaults (create).
    let existing = slug.as_ref().and_then(|s| {
        let st = state.borrow();
        st.library.as_ref().and_then(|lib| lib.load_node(s).ok())
    });
    let fm = existing
        .as_ref()
        .map(|n| n.frontmatter.clone())
        .unwrap_or_default();
    let body_text = existing.map(|n| n.body).unwrap_or_default();

    // This node's relations, grouped by predicate with targets resolved to display names —
    // a read-only "neighbours" view (relations are authored from the entry side).
    let relation_groups: Vec<(String, Vec<String>)> = {
        let st = state.borrow();
        match st.library.as_ref() {
            Some(lib) => group_relations_display(lib, &fm.relations),
            None => Vec::new(),
        }
    };

    let dialog = adw::Window::new();
    dialog.set_title(Some(if slug.is_some() {
        "Edit node"
    } else {
        "New node"
    }));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(540, 600);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let save = gtk4::Button::with_label("Save");
    save.add_css_class("suggested-action");
    header.pack_start(&cancel);
    if let Some(slug) = &slug {
        // Relations from this node's own side — the gap M3 left open ("relations are
        // authored from the entry side only"). `relations_dialog` is already host-agnostic
        // (it resolves its `key` through notes ∪ nodes uniformly), so this is the same
        // dialog the entry detail panel uses, just given a node slug instead of an entry
        // key. The one imperfection: this editor's own read-only neighbours section below
        // (built once, when the editor opened) won't reflect a save made through this
        // button until the node is reopened — same boundary every other dialog in the app
        // already has with its siblings.
        let relations_button = gtk4::Button::with_label("Relations…");
        relations_button.set_tooltip_text(Some("Relate this node to entries or other nodes"));
        {
            let state = state.clone();
            let widgets = widgets.clone();
            let slug = slug.clone();
            relations_button.connect_clicked(move |_| relations_dialog(&state, &widgets, &slug));
        }
        header.pack_start(&relations_button);

        let delete_button = gtk4::Button::with_label("Delete…");
        delete_button.add_css_class("destructive-action");
        delete_button.set_tooltip_text(Some("Delete this node and its relation edges"));
        {
            let state = state.clone();
            let widgets = widgets.clone();
            let dialog = dialog.clone();
            let slug = slug.clone();
            let label = fm.label.clone();
            let on_saved = on_saved.clone();
            delete_button.connect_clicked(move |_| {
                confirm_delete_node(&state, &widgets, &dialog, &slug, &label, on_saved.clone())
            });
        }
        header.pack_start(&delete_button);
    }
    header.pack_end(&save);
    view.add_top_bar(&header);

    let content = gtk4::Box::new(Orientation::Vertical, 10);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let choices = node_type_choices();
    let type_labels: Vec<&str> = choices.iter().map(|(l, _)| *l).collect();
    let type_drop = gtk4::DropDown::from_strings(&type_labels);
    let sel = choices
        .iter()
        .position(|(_, t)| *t == fm.node_type)
        .unwrap_or(1);
    type_drop.set_selected(sel as u32);
    content.append(&labeled("Type", &type_drop));

    let label_entry = gtk4::Entry::builder()
        .text(&fm.label)
        .placeholder_text("Display name")
        .build();
    content.append(&labeled("Label", &label_entry));

    let aliases_entry = gtk4::Entry::builder()
        .text(fm.aliases.join(", "))
        .placeholder_text("comma, separated, aliases")
        .build();
    content.append(&labeled("Aliases", &aliases_entry));

    let ident_view = gtk4::TextView::builder()
        .wrap_mode(gtk4::WrapMode::WordChar)
        .left_margin(8)
        .right_margin(8)
        .top_margin(8)
        .bottom_margin(8)
        .build();
    ident_view.set_height_request(90);
    let ident_text = fm
        .identifiers
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join("\n");
    ident_view.buffer().set_text(&ident_text);
    let ident_scroll = gtk4::ScrolledWindow::new();
    ident_scroll.set_child(Some(&ident_view));
    ident_scroll.add_css_class("card");
    content.append(&labeled(
        "Identifiers (one per line — scheme: value)",
        &ident_scroll,
    ));

    // Read-only neighbours: this node's relations grouped by predicate. Edited from the
    // entry's "Relations…" dialog, not here.
    if !relation_groups.is_empty() {
        let section = gtk4::Box::new(Orientation::Vertical, 2);
        let heading = gtk4::Label::new(Some("Relations"));
        heading.add_css_class("dim-label");
        heading.set_xalign(0.0);
        heading.set_halign(gtk4::Align::Start);
        section.append(&heading);
        for (pred, names) in &relation_groups {
            let line = gtk4::Label::new(Some(&format!("{pred}: {}", names.join(", "))));
            line.set_wrap(true);
            line.set_xalign(0.0);
            line.set_halign(gtk4::Align::Start);
            line.add_css_class("caption");
            section.append(&line);
        }
        content.append(&section);
    }

    let body = gtk4::TextView::builder()
        .wrap_mode(gtk4::WrapMode::WordChar)
        .left_margin(8)
        .right_margin(8)
        .top_margin(8)
        .bottom_margin(8)
        .build();
    body.buffer().set_text(&body_text);
    let body_scroll = gtk4::ScrolledWindow::new();
    body_scroll.set_child(Some(&body));
    body_scroll.set_vexpand(true);
    body_scroll.add_css_class("card");
    content.append(&labeled("Notes", &body_scroll));

    view.set_content(Some(&content));
    dialog.set_content(Some(&view));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        let slug = slug.clone();
        save.connect_clicked(move |_| {
            let label = label_entry.text().trim().to_string();
            if label.is_empty() {
                toast(&widgets, "A node needs a label");
                return;
            }
            let node_type = choices
                .get(type_drop.selected() as usize)
                .map(|(_, t)| *t)
                .unwrap_or(fond_bib::NodeType::Concept);
            let aliases: Vec<String> = aliases_entry
                .text()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let mut identifiers = std::collections::BTreeMap::new();
            let ibuf = ident_view.buffer();
            let itext = ibuf
                .text(&ibuf.start_iter(), &ibuf.end_iter(), false)
                .to_string();
            for line in itext.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    let (k, v) = (k.trim(), v.trim());
                    if !k.is_empty() && !v.is_empty() {
                        identifiers.insert(k.to_string(), v.to_string());
                    }
                }
            }
            let bbuf = body.buffer();
            let body_str = bbuf
                .text(&bbuf.start_iter(), &bbuf.end_iter(), false)
                .to_string();

            // Preserve any existing relations (edited elsewhere); update the curated fields.
            let mut new_fm = fm.clone();
            new_fm.node_type = node_type;
            new_fm.label = label.clone();
            new_fm.aliases = aliases;
            new_fm.identifiers = identifiers;
            let node = fond_bib::Node {
                frontmatter: new_fm,
                body: body_str,
            };

            // An existing node keeps its slug; a new one gets a fresh collision-free slug.
            let target_slug = match &slug {
                Some(s) => s.clone(),
                None => {
                    let s = state.borrow();
                    let Some(lib) = s.library.as_ref() else {
                        return;
                    };
                    let existing: std::collections::HashSet<String> =
                        lib.node_slugs().unwrap_or_default().into_iter().collect();
                    fond_bib::key::assign_key(&fond_bib::key::node_slug(&label), &existing)
                }
            };

            let result = {
                let s = state.borrow();
                s.library
                    .as_ref()
                    .map(|lib| lib.write_node(&target_slug, &node))
            };
            match result {
                Some(Ok(_)) => {
                    rebuild_index_silent(&state);
                    toast(&widgets, "Node saved");
                    dialog.close();
                    on_saved();
                }
                Some(Err(e)) => toast(&widgets, &friendly::bib_error(&e)),
                None => {}
            }
        });
    }

    dialog.present();
}

/// Offer to create or link a **person node** for each of an entry's authors, then relate it
/// to the entry with `authored` (which maintains the entry's `authored-by` inverse). This is
/// how §1 author identifiers (ORCID/VIAF/…) get captured in practice: link the author, then
/// add identifiers in the node editor. A new node's slug is the family name (`docs/M3-SPEC.md`
/// §1); an author whose family-name slug already exists is offered as a link to that node.
fn link_authors_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, key: &str) {
    use fond_bib::{NodeFrontmatter, NodeType, Predicate};

    struct AuthorPlan {
        label: String,
        slug: String,
        exists: bool,
    }
    let plans: Vec<AuthorPlan> = {
        let s = state.borrow();
        let Some(lib) = s.library.as_ref() else {
            toast(widgets, "Open a library first");
            return;
        };
        let Ok(parsed) = lib.load_entry(key) else {
            toast(widgets, "Could not load this entry");
            return;
        };
        let authors: Vec<_> = parsed.entry.authors().unwrap_or_default().to_vec();
        if authors.is_empty() {
            toast(widgets, "This entry has no authors");
            return;
        }
        let existing: std::collections::HashSet<String> =
            lib.node_slugs().unwrap_or_default().into_iter().collect();
        let mut taken = existing.clone();
        authors
            .iter()
            .map(|p| {
                let family = p.name.clone();
                let label = match &p.given_name {
                    Some(g) if !g.is_empty() => format!("{g} {family}"),
                    _ => family.clone(),
                };
                let base = fond_bib::key::node_slug(&family);
                if existing.contains(&base) {
                    AuthorPlan {
                        label,
                        slug: base,
                        exists: true,
                    }
                } else {
                    // Fresh, collision-free against disk slugs and others in this batch.
                    let slug = fond_bib::key::assign_key(&base, &taken);
                    taken.insert(slug.clone());
                    AuthorPlan {
                        label,
                        slug,
                        exists: false,
                    }
                }
            })
            .collect()
    };

    let dialog = adw::Window::new();
    dialog.set_title(Some("Link authors to nodes"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(460, -1);
    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let link = gtk4::Button::with_label("Link");
    link.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&link);
    view.add_top_bar(&header);

    let list = gtk4::Box::new(Orientation::Vertical, 8);
    list.set_margin_top(14);
    list.set_margin_bottom(14);
    list.set_margin_start(16);
    list.set_margin_end(16);

    // (checkbox, slug, label, exists) per author.
    let rows: Rc<Vec<(gtk4::CheckButton, String, String, bool)>> = Rc::new(
        plans
            .into_iter()
            .map(|p| {
                let row = gtk4::Box::new(Orientation::Horizontal, 8);
                let check = gtk4::CheckButton::new();
                check.set_active(true);
                check.set_valign(gtk4::Align::Center);
                let text = gtk4::Box::new(Orientation::Vertical, 0);
                text.set_hexpand(true);
                let name = gtk4::Label::new(Some(&p.label));
                name.set_xalign(0.0);
                name.set_halign(gtk4::Align::Start);
                let sub = gtk4::Label::new(Some(&if p.exists {
                    format!("→ link existing node «{}»", p.slug)
                } else {
                    format!("→ create person node «{}»", p.slug)
                }));
                sub.add_css_class("dim-label");
                sub.add_css_class("caption");
                sub.set_xalign(0.0);
                sub.set_halign(gtk4::Align::Start);
                text.append(&name);
                text.append(&sub);
                row.append(&check);
                row.append(&text);
                list.append(&row);
                (check, p.slug, p.label, p.exists)
            })
            .collect(),
    );

    view.set_content(Some(&list));
    dialog.set_content(Some(&view));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        let key = key.to_string();
        let rows = rows.clone();
        link.connect_clicked(move |_| {
            let mut linked = 0usize;
            let mut failed: Option<String> = None;
            {
                let s = state.borrow();
                let Some(lib) = s.library.as_ref() else {
                    return;
                };
                for (check, slug, label, exists) in rows.iter() {
                    if !check.is_active() {
                        continue;
                    }
                    // Create the person node first (so the slug resolves to a node host),
                    // unless we're linking one that already exists.
                    if !exists {
                        let node = fond_bib::Node {
                            frontmatter: NodeFrontmatter {
                                node_type: NodeType::Person,
                                label: label.clone(),
                                ..Default::default()
                            },
                            body: String::new(),
                        };
                        if let Err(e) = lib.write_node(slug, &node) {
                            failed = Some(e.to_string());
                            break;
                        }
                    }
                    match lib.add_relation(slug, Predicate::Authored, &key) {
                        Ok(()) => linked += 1,
                        Err(e) => {
                            failed = Some(e.to_string());
                            break;
                        }
                    }
                }
            }
            match failed {
                Some(e) => toast(&widgets, &format!("Could not link: {e}")),
                None => {
                    rebuild_index_silent(&state);
                    toast(&widgets, &format!("Linked {linked} author(s)"));
                    dialog.close();
                    reload_current(&state, &widgets);
                }
            }
        });
    }

    dialog.present();
}

/// A flat, left-aligned row for a hand-built popover menu — house style (see Zerkalo's
/// hamburger): a `Popover` holding a vertical `Box` of flat buttons, rather than a
/// `gio::Menu` model. Used for the detail panel's "Edit"/"More" popovers and the main
/// hamburger, all of which have more items than fit as a flat always-visible row or read
/// well as one undifferentiated `gio::Menu` section.
fn popover_button(label: &str, destructive: bool) -> gtk4::Button {
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

/// The shared frame a hand-built popover's rows are appended into: a `Popover` wrapping a
/// margined vertical `Box`, sized to a minimum width so short labels don't look cramped.
fn popover_menu(min_width: i32) -> (gtk4::Popover, gtk4::Box) {
    let rows = gtk4::Box::new(Orientation::Vertical, 2);
    rows.set_margin_top(6);
    rows.set_margin_bottom(6);
    rows.set_margin_start(6);
    rows.set_margin_end(6);
    rows.set_width_request(min_width);

    // Cap and scroll rather than let the popover's natural height grow unbounded — with
    // ~20 rows, the hamburger's popover found this the hard way: on a screen without enough
    // room below the button for its full height, it didn't reposition or shrink, it just
    // failed to show at all (reproduced locally by shrinking the test display). A `gio::Menu`
    // (the old hamburger) scrolls automatically once it doesn't fit; a plain `Box` doesn't,
    // so this gives it back explicitly. Short popovers (Edit, More) stay exactly as tall as
    // their content — `propagate_natural_height` only engages the scrollbar past the cap.
    let scroller = gtk4::ScrolledWindow::new();
    scroller.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroller.set_propagate_natural_height(true);
    scroller.set_max_content_height(420);
    scroller.set_child(Some(&rows));

    let popover = gtk4::Popover::new();
    popover.set_child(Some(&scroller));
    (popover, rows)
}

fn popover_separator() -> gtk4::Separator {
    let sep = gtk4::Separator::new(Orientation::Horizontal);
    sep.set_margin_top(4);
    sep.set_margin_bottom(4);
    sep
}

fn clear_box(b: &gtk4::Box) {
    while let Some(child) = b.first_child() {
        b.remove(&child);
    }
}

fn field_row(name: &str, value: &str) -> gtk4::Box {
    let row = gtk4::Box::new(Orientation::Horizontal, 10);
    let name_label = gtk4::Label::new(Some(name));
    name_label.add_css_class("dim-label");
    name_label.set_xalign(1.0);
    name_label.set_width_chars(13);
    name_label.set_valign(gtk4::Align::Start);
    let value_label = gtk4::Label::new(Some(value));
    value_label.set_xalign(0.0);
    value_label.set_halign(gtk4::Align::Start);
    value_label.set_wrap(true);
    value_label.set_selectable(true);
    value_label.set_hexpand(true);
    row.append(&name_label);
    row.append(&value_label);
    row
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn toast(widgets: &Rc<Widgets>, message: &str) {
    widgets.toasts.add_toast(adw::Toast::new(message));
}
