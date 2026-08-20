//! The main application window: a sidebar list of entries with a live filter, and a detail
//! pane showing the selected entry's YAML and note. All data comes from `fond-bib`.

use std::cell::{Cell, RefCell};
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
use crate::ui::friendly;
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
}

#[derive(Default)]
struct AppState {
    library: Option<Library>,
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
mod entry_row {
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
    menu_button.set_popover(Some(&build_hamburger_popover(&config)));
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

    // Entries spreadsheet: a sortable, in-place-editable `ColumnView` (Zotero-style) in place
    // of the old card list — see `build_entries_column_view`. The factories it wires up need
    // `Widgets` (for toasts/reload on a committed edit), which doesn't exist until after this
    // block — `widgets_slot` is filled in once it does; edits can't happen before the window
    // is shown, so it's always populated by the time a factory closure runs.
    let widgets_slot: Rc<RefCell<Option<Rc<Widgets>>>> = Rc::new(RefCell::new(None));
    let (column_view, store, selection) = build_entries_column_view();
    apply_column_visibility(&column_view, &config.borrow());
    reorder_columns(&column_view, &config.borrow().column_order);
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
    sidebar.append(&search);
    sidebar.append(&bulk_bar);
    sidebar.append(&list_scroll);

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

    // Restore the last-opened library.
    if let Some(path) = config.borrow().library_path.clone() {
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
fn build_hamburger_popover(config: &Rc<RefCell<Config>>) -> gtk4::Popover {
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
    let (sender, receiver) = glib::MainContext::channel::<
        Result<(bool, String, Option<u32>), String>,
    >(glib::Priority::DEFAULT);
    let worker_path = path.clone();
    std::thread::spawn(move || {
        let _ = sender.send(identify_pdf(&worker_path));
    });

    let state = state.clone();
    let widgets = widgets.clone();
    receiver.attach(None, move |result| {
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

    let (sender, receiver) =
        glib::MainContext::channel::<Result<String, String>>(glib::Priority::DEFAULT);
    let worker_path = path.clone();
    std::thread::spawn(move || {
        let _ = sender.send(epub_entry_yaml(&worker_path));
    });

    let state = state.clone();
    let widgets = widgets.clone();
    receiver.attach(None, move |result| {
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

    let (sender, receiver) = glib::MainContext::channel::<FolderProgress>(glib::Priority::DEFAULT);
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
    receiver.attach(None, move |msg| match msg {
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
    let (sender, receiver) =
        glib::MainContext::channel::<Result<(Vec<u8>, String), String>>(glib::Priority::DEFAULT);
    std::thread::spawn(move || {
        let _ = sender.send(unpaywall_download(&doi, &email));
    });

    let state = state.clone();
    let widgets = widgets.clone();
    let key = key.to_string();
    receiver.attach(None, move |result| {
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

    let (sender, receiver) = glib::MainContext::channel::<ScrapeResult>(glib::Priority::DEFAULT);
    std::thread::spawn(move || {
        let _ = sender.send(scrape_url(&url));
    });

    let state = state.clone();
    let widgets = widgets.clone();
    receiver.attach(None, move |result| {
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
/// duplicate-then-retype (Zotero's "Create Book Section From Item") would.
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

            let (sender, receiver) = glib::MainContext::channel::<
                Result<fond_bib::ImportReport, String>,
            >(glib::Priority::DEFAULT);
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
            receiver.attach(None, move |result| {
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
    let (sender, receiver) =
        glib::MainContext::channel::<Result<PathBuf, String>>(glib::Priority::DEFAULT);
    let worker_dest = dest.clone();
    std::thread::spawn(move || {
        let result = copy_library_dir(&root, &worker_dest)
            .map(|_| worker_dest)
            .map_err(|e| e.to_string());
        let _ = sender.send(result);
    });

    let widgets = widgets.clone();
    receiver.attach(None, move |result| {
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
    let (sender, receiver) =
        glib::MainContext::channel::<Result<(), String>>(glib::Priority::DEFAULT);
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
    receiver.attach(None, move |result| {
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

    let (sender, receiver) = glib::MainContext::channel::<Result<github::DeviceCodeResponse, String>>(
        glib::Priority::DEFAULT,
    );
    std::thread::spawn(move || {
        let _ =
            sender.send(github::request_device_code(github::CLIENT_ID).map_err(|e| e.to_string()));
    });

    let widgets = widgets.clone();
    receiver.attach(None, move |result| {
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

    let (sender, receiver) =
        glib::MainContext::channel::<Result<(String, String), String>>(glib::Priority::DEFAULT);
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
    receiver.attach(None, move |result| {
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

    let (sender, receiver) =
        glib::MainContext::channel::<Result<(), String>>(glib::Priority::DEFAULT);
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
    receiver.attach(None, move |result| {
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
            let (sender, receiver) =
                glib::MainContext::channel::<Result<usize, String>>(glib::Priority::DEFAULT);
            std::thread::spawn(move || {
                let _ = sender.send(webdav::upload_library(&base, &user, &pass, &root));
            });

            let widgets = widgets.clone();
            let dialog = dialog.clone();
            let back_up = back_up.clone();
            let spinner = spinner.clone();
            receiver.attach(None, move |result| {
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

    let (sender, receiver) =
        glib::MainContext::channel::<Result<(), String>>(glib::Priority::DEFAULT);
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
    receiver.attach(None, move |result| {
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
            let (sender, receiver) = glib::MainContext::channel::<Result<(bool, String), String>>(
                glib::Priority::DEFAULT,
            );
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
            receiver.attach(None, move |result| {
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

    let defs = library.load_custom_field_defs().unwrap_or_default();
    sync_custom_field_columns(
        &widgets.column_view,
        &widgets.custom_columns,
        &defs,
        &widgets.config.borrow(),
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
            let drop = gtk4::DropTarget::new(glib::types::Type::STRING, gdk::DragAction::COPY);
            drop.connect_drop(move |_, value, _, _| {
                let Ok(key) = value.get::<String>() else {
                    return false;
                };
                let result = {
                    let s = state.borrow();
                    s.library
                        .as_ref()
                        .map(|lib| lib.add_to_collection(&slug, &key))
                };
                match result {
                    Some(Ok(())) => {
                        refresh_list(&state, &widgets);
                        toast(&widgets, "Added to collection");
                        true
                    }
                    Some(Err(e)) => {
                        toast(&widgets, &friendly::bib_error(&e));
                        false
                    }
                    None => false,
                }
            });
            row.add_controller(drop);
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
            let slug = fond_bib::zotero::slugify(&name);
            let result = {
                let s = state.borrow();
                let lib = s.library.as_ref().expect("library open");
                lib.save_collection(
                    &slug,
                    &fond_bib::Collection {
                        name: name.clone(),
                        description: None,
                        parent,
                        keys: Vec::new(),
                    },
                )
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
            }
        });
    }
    let column = gtk4::ColumnViewColumn::new(Some(&spec.title), Some(factory));
    column.set_expand(spec.expand);
    if spec.width > 0 {
        column.set_fixed_width(spec.width);
    }
    column.set_resizable(true);
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
fn build_entries_column_view() -> (gtk4::ColumnView, gio::ListStore, gtk4::SingleSelection) {
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
    let selection = gtk4::SingleSelection::new(Some(sort_model));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    column_view.set_model(Some(&selection));

    (column_view, store, selection)
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
                let kind = ReaderAttachmentKind::from_filename(&att.filename)?;
                if kind != wanted {
                    return None;
                }
                let hex = att
                    .hash
                    .split_once(':')
                    .map(|(_, h)| h)
                    .unwrap_or(&att.hash);
                let path = library.attachment_blob_path(hex);
                path.exists()
                    .then(|| (path, att.filename.clone(), att.hash.clone()))
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
            let state = state.clone();
            let widgets = widgets.clone();
            let key = key.clone();
            let title = title_text.to_string();
            read_button.connect_clicked(move |_| {
                show_pdf_reader(&state, &widgets, &key, &hash, &path, &title, start_page)
            });
            actions.append(&read_button);
        }
        (None, Some((path, _filename, hash))) => {
            let read_button = gtk4::Button::with_label("Read");
            read_button.set_tooltip_text(Some("Open the built-in EPUB reader"));
            let state = state.clone();
            let widgets = widgets.clone();
            let key = key.clone();
            let title = title_text.to_string();
            read_button.connect_clicked(move |_| {
                show_epub_reader(&state, &widgets, &key, &hash, &path, &title, None);
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
            let start_page = note
                .as_ref()
                .and_then(|n| n.frontmatter.progress)
                .map(|p| p.page)
                .unwrap_or(1);

            let row = popover_button(&format!("PDF — {pdf_filename}"), false);
            {
                let popover = popover.clone();
                let state = state.clone();
                let widgets = widgets.clone();
                let key = key.clone();
                let title = title_text.to_string();
                row.connect_clicked(move |_| {
                    popover.popdown();
                    show_pdf_reader(
                        &state, &widgets, &key, &pdf_hash, &pdf_path, &title, start_page,
                    );
                });
            }
            rows.append(&row);

            let row = popover_button(&format!("EPUB — {epub_filename}"), false);
            {
                let popover = popover.clone();
                let state = state.clone();
                let widgets = widgets.clone();
                let key = key.clone();
                let title = title_text.to_string();
                row.connect_clicked(move |_| {
                    popover.popdown();
                    show_epub_reader(&state, &widgets, &key, &epub_hash, &epub_path, &title, None);
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
    // the fields below (click into a field, no dialog) — this button is left for the note
    // editor's remaining fields (progress, cite preferences, tasks, prose) that aren't
    // inline yet.
    let edit_button = gtk4::Button::with_label("Edit note…");
    edit_button.set_tooltip_text(Some(
        "Edit reading progress, citation preferences, tasks, and your own notes",
    ));
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let key = key.clone();
        edit_button.connect_clicked(move |_| show_note_editor(&state, &widgets, &key));
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
                row.connect_clicked(move |_| {
                    popover.popdown();
                    show_annotations_dialog(
                        &state,
                        &widgets,
                        &key,
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

/// Whether a (possibly absent) note has a readable (present-on-disk) PDF and/or EPUB
/// attachment — same detection `show_detail` uses for its own Read button, factored out so
/// the entry list's row icon (`EntrySummary::has_pdf`/`has_epub`) can reuse it. Takes an
/// already-loaded note rather than a key, so the entries-loading loop in `open_library` (which
/// needs the note anyway, for tags/status/custom fields) doesn't read each note file twice.
fn attachment_presence(library: &Library, note: Option<&fond_bib::Note>) -> (bool, bool) {
    let has = |wanted: ReaderAttachmentKind| {
        note.is_some_and(|n| {
            n.frontmatter.attachments.iter().any(|att| {
                ReaderAttachmentKind::from_filename(&att.filename) == Some(wanted) && {
                    let hex = att
                        .hash
                        .split_once(':')
                        .map(|(_, h)| h)
                        .unwrap_or(&att.hash);
                    library.attachment_blob_path(hex).exists()
                }
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

/// List an entry's annotations — page, kind, note — so each can be jumped to in the reader,
/// have its note edited, or be deleted. Reads and writes the same `annots/<key>.json`
/// sidecar `show_pdf_reader`'s drag-to-highlight writes to.
fn show_annotations_dialog(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    key: &str,
    // Independent per-format attachment info (hash, blob path) — an entry can have both a
    // PDF and an EPUB attached, so each annotation row's "Go to" routes to whichever of these
    // matches that specific annotation's anchor (`page` → PDF, `chapter` → EPUB), not a
    // single kind fixed for the whole dialog (M5-SPEC.md Tier 4).
    pdf_attachment: Option<(String, std::path::PathBuf)>,
    epub_attachment: Option<(String, std::path::PathBuf)>,
    reader_title: &str,
) {
    let sidecar = {
        let s = state.borrow();
        s.library
            .as_ref()
            .and_then(|lib| lib.load_annotations(key).ok().flatten())
    };
    let Some(sidecar) = sidecar else {
        toast(widgets, "No annotations for this entry");
        return;
    };

    let dialog = adw::Window::new();
    dialog.set_title(Some("Annotations"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(480, 560);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    let close_button = gtk4::Button::with_label("Close");
    {
        let dialog = dialog.clone();
        close_button.connect_clicked(move |_| dialog.close());
    }
    header.pack_start(&close_button);
    let export_button = gtk4::Button::with_label("Export…");
    export_button.set_tooltip_text(Some("Save these annotations as a portable Markdown file"));
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let key = key.to_string();
        let reader_title = reader_title.to_string();
        export_button.connect_clicked(move |_| {
            // Reload fresh rather than reuse the dialog's own `sidecar` capture — the list
            // above can go stale if a note was edited or an annotation deleted earlier in
            // this same dialog session (each of those reloads independently, not through
            // this closure's binding), so an export should reflect what's actually on disk.
            let sidecar = {
                let s = state.borrow();
                s.library
                    .as_ref()
                    .and_then(|lib| lib.load_annotations(&key).ok().flatten())
            };
            let Some(sidecar) = sidecar else {
                toast(&widgets, "No annotations for this entry");
                return;
            };
            let markdown = sidecar.to_markdown(&reader_title);

            let default_name = format!("{key}-annotations.md");
            let save = gtk4::FileDialog::builder()
                .title("Export annotations")
                .initial_name(&default_name)
                .build();
            let widgets = widgets.clone();
            let parent = widgets.window.clone();
            save.save(Some(&parent), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        match std::fs::write(&path, &markdown) {
                            Ok(()) => toast(&widgets, &format!("Exported to {}", path.display())),
                            Err(e) => toast(&widgets, &format!("Could not write file: {e}")),
                        }
                    }
                }
            });
        });
    }
    header.pack_end(&export_button);
    view.add_top_bar(&header);

    let list = gtk4::ListBox::new();
    list.set_selection_mode(gtk4::SelectionMode::None);
    list.add_css_class("fond-list");
    list.set_margin_top(12);
    list.set_margin_bottom(12);
    list.set_margin_start(12);
    list.set_margin_end(12);

    let mut annotations: Vec<fond_bib::Annotation> = sidecar.annotations.clone();
    annotations.sort_by_key(|a| a.page);
    let last = annotations.len().saturating_sub(1);

    for (i, annotation) in annotations.into_iter().enumerate() {
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

        let outer = gtk4::Box::new(Orientation::Vertical, 6);
        outer.set_margin_top(8);
        outer.set_margin_bottom(8);
        outer.set_margin_start(10);
        outer.set_margin_end(10);

        let header_row = gtk4::Box::new(Orientation::Horizontal, 8);
        // Location text is format-aware: a PDF annotation always has `page`, an EPUB one
        // always has `chapter` (shown as just the chapter's filename, not the full
        // zip-internal path — plenty to recognize which chapter, without the clutter).
        let location = match (annotation.page, annotation.chapter.as_deref()) {
            (Some(p), _) => format!("Page {p}"),
            (None, Some(chapter)) => std::path::Path::new(chapter)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| chapter.to_string()),
            (None, None) => String::from("Unknown location"),
        };
        let kind_label = gtk4::Label::new(Some(&format!("{location} · {:?}", annotation.kind)));
        kind_label.add_css_class("fond-row-title");
        kind_label.set_xalign(0.0);
        kind_label.set_hexpand(true);
        header_row.append(&kind_label);

        // Which format this specific annotation anchors on — not a single kind fixed for the
        // whole dialog, so a mixed PDF+EPUB entry routes each row to the right reader.
        let is_pdf = annotation.page.is_some();
        let goto_button = gtk4::Button::with_label(if is_pdf {
            "Go to page"
        } else {
            "Go to chapter"
        });
        let attachment_for_row = if is_pdf {
            pdf_attachment.clone()
        } else {
            epub_attachment.clone()
        };
        match attachment_for_row {
            Some((hash, blob)) => {
                let state = state.clone();
                let widgets = widgets.clone();
                let key = key.to_string();
                let title = reader_title.to_string();
                let page = annotation.page;
                let annotation_id = annotation.id.clone();
                goto_button.connect_clicked(move |_| {
                    if is_pdf {
                        // `page` is always `Some` for a PDF-anchored annotation;
                        // `unwrap_or(1)` is just a defensive fallback, not an expected path.
                        show_pdf_reader(
                            &state,
                            &widgets,
                            &key,
                            &hash,
                            &blob,
                            &title,
                            page.unwrap_or(1),
                        )
                    } else {
                        show_epub_reader(
                            &state,
                            &widgets,
                            &key,
                            &hash,
                            &blob,
                            &title,
                            Some(&annotation_id),
                        )
                    }
                });
            }
            None => {
                // That format's attachment isn't currently present (e.g. it was removed
                // after this annotation was created) — show the button, disabled, rather
                // than hide it, so the row still reads as "this was a PDF/EPUB highlight".
                goto_button.set_sensitive(false);
                goto_button.set_tooltip_text(Some("That attachment is no longer present"));
            }
        }
        header_row.append(&goto_button);

        let delete_button = gtk4::Button::from_icon_name("user-trash-symbolic");
        delete_button.add_css_class("flat");
        delete_button.set_tooltip_text(Some("Delete this annotation"));
        header_row.append(&delete_button);
        outer.append(&header_row);

        let note_entry = gtk4::Entry::new();
        note_entry.set_placeholder_text(Some("No note"));
        if let Some(note) = &annotation.note {
            note_entry.set_text(note);
        }
        outer.append(&note_entry);

        row.set_child(Some(&outer));
        list.append(&row);

        // Note edits save on Enter or when the field loses focus, matching the rest of the
        // app's "save as you go" dialogs rather than needing an explicit Save button.
        let save_note = {
            let state = state.clone();
            let widgets = widgets.clone();
            let key = key.to_string();
            let id = annotation.id.clone();
            move |text: &str| {
                let text = text.trim();
                let s = state.borrow();
                let Some(library) = s.library.as_ref() else {
                    return;
                };
                let Ok(Some(mut sidecar)) = library.load_annotations(&key) else {
                    return;
                };
                let Some(a) = sidecar.annotations.iter_mut().find(|a| a.id == id) else {
                    return;
                };
                a.note = (!text.is_empty()).then(|| text.to_string());
                if let Err(e) = library.write_annotations(&sidecar) {
                    drop(s);
                    toast(&widgets, &friendly::bib_error(&e));
                }
            }
        };
        {
            let save_note = save_note.clone();
            note_entry.connect_activate(move |e| save_note(&e.text()));
        }
        {
            let focus = gtk4::EventControllerFocus::new();
            let save_note = save_note.clone();
            let note_entry_weak = note_entry.downgrade();
            focus.connect_leave(move |_| {
                if let Some(e) = note_entry_weak.upgrade() {
                    save_note(&e.text());
                }
            });
            note_entry.add_controller(focus);
        }

        {
            let state = state.clone();
            let widgets = widgets.clone();
            let key = key.to_string();
            let id = annotation.id.clone();
            let list = list.clone();
            let row = row.clone();
            delete_button.connect_clicked(move |_| {
                let result = {
                    let s = state.borrow();
                    s.library.as_ref().and_then(|library| {
                        let mut sidecar = library.load_annotations(&key).ok().flatten()?;
                        sidecar.annotations.retain(|a| a.id != id);
                        Some(library.write_annotations(&sidecar))
                    })
                };
                match result {
                    Some(Ok(_)) => {
                        list.remove(&row);
                        toast(&widgets, "Annotation deleted");
                    }
                    Some(Err(e)) => toast(&widgets, &friendly::bib_error(&e)),
                    None => toast(&widgets, "No open library"),
                }
            });
        }
    }

    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    scrolled.set_child(Some(&list));
    view.set_content(Some(&scrolled));
    dialog.set_content(Some(&view));
    dialog.present();
}

/// Live state of an open PDF reader window.
struct ReaderState {
    pdfium: fond_doc::Pdfium,
    bytes: Vec<u8>,
    page: u16,
    count: u16,
    /// Render width in px = `BASE_WIDTH * zoom`.
    zoom: f64,
    /// This entry's annotation sidecar, loaded once at open and rewritten to disk on every
    /// highlight added. Held here (not re-read from the library each time) so the in-memory
    /// list and the on-screen render never disagree mid-session.
    annotations: fond_bib::AnnotationSidecar,
    /// The current page's rendered pixel size and PDF-point size, refreshed by `render()` —
    /// the scale a drag-selected rectangle is converted through when saving a new highlight.
    render_px: (u32, u32),
    page_pts: (f32, f32),
    /// Which kind the next drag creates — set by the mode `DropDown`, defaulting to
    /// `Highlight`. `Note` is never drawn this way (it has no on-page region); it's added
    /// via the separate "Note…" button instead. `None` is the drop-down's "Select text"
    /// entry — a drag copies the covered text to the clipboard instead of saving an
    /// annotation.
    draw_kind: Option<fond_bib::AnnotationKind>,
    /// The most recent "Select text" copy — page (0-based) and text — so the next note added
    /// on that same page can quote it instead of starting blank. Cleared once consumed.
    last_selection: Option<(u16, String)>,
    /// Every match from the last search (empty if none run yet, or the last search found
    /// nothing), and which one is "current" — `render()` blends that one's quads in a
    /// distinct colour when the current page matches, and prev/next-match cycle this index.
    search_matches: Vec<fond_doc::PdfSearchMatch>,
    search_current: usize,
    /// Hex colour (`#rrggbb`) the next drag saves onto its annotation — set by the colour
    /// `DropDown`, defaulting to the original hardcoded amber.
    draw_color: String,
    /// Continuous-scroll mode's state — empty until the mode is toggled on for the first
    /// time (built lazily, not at reader-open, so plain "Read" stays as fast as it always
    /// was). `continuous_pictures[i]` is page `i`'s permanent `Picture` widget (unlike the
    /// paged view's single recycled one); `continuous_offsets[i]` is that page's cumulative
    /// top position in the continuous view, in pixels, for scroll-to-page and for tracking
    /// which page is "current" from scroll position; `continuous_rendered[i]` avoids
    /// re-rendering a page that hasn't changed since it was last drawn.
    continuous_pictures: Vec<gtk4::Picture>,
    continuous_offsets: Vec<f64>,
    continuous_rendered: Vec<bool>,
    /// Each page's document-defined `/PageLabels` printed number (`None` where the PDF
    /// doesn't define one, which is most PDFs) — read once at open, since it's an immutable
    /// property of the file. Index `i` (0-based) matches every other page index in this
    /// struct.
    page_labels: Vec<Option<String>>,
    /// Snapshot-based undo/redo: each entry is a full clone of `annotations` taken
    /// immediately before a mutation (drag-created highlight, note added, annotation
    /// deleted or edited). `push_undo_snapshot` is the single place that pushes here and
    /// clears `redo_stack` — every mutation site calls it first. Capped at
    /// `UNDO_HISTORY_LIMIT` so a long session doesn't grow this unbounded.
    undo_stack: Vec<fond_bib::AnnotationSidecar>,
    redo_stack: Vec<fond_bib::AnnotationSidecar>,
}

/// How many undo steps a PDF reader session keeps before dropping the oldest.
const UNDO_HISTORY_LIMIT: usize = 50;

/// Snapshot `reader`'s current annotations onto the undo stack and clear the redo stack —
/// call this immediately before any mutation to `reader.annotations`, so the mutation can be
/// undone. Standard editor convention: a fresh mutation invalidates any pending redo.
fn push_undo_snapshot(reader: &Rc<RefCell<ReaderState>>) {
    let mut r = reader.borrow_mut();
    let snapshot = r.annotations.clone();
    r.undo_stack.push(snapshot);
    if r.undo_stack.len() > UNDO_HISTORY_LIMIT {
        r.undo_stack.remove(0);
    }
    r.redo_stack.clear();
}

/// Refresh the Undo/Redo header buttons' sensitivity from `reader`'s current stacks — called
/// after every mutation site (both the paged view's and, since `build_continuous_view` builds
/// its own drag handlers, continuous mode's) so the buttons never sit enabled/disabled stale.
fn sync_undo_redo_buttons(
    reader: &Rc<RefCell<ReaderState>>,
    undo_button: &gtk4::Button,
    redo_button: &gtk4::Button,
) {
    let r = reader.borrow();
    undo_button.set_sensitive(!r.undo_stack.is_empty());
    redo_button.set_sensitive(!r.redo_stack.is_empty());
}

/// The colour `DropDown`'s fixed preset order — a small curated set (like a real
/// highlighter's usual colours) rather than a full colour-wheel picker, index into this by
/// selection.
const COLOR_PRESETS: [(&str, &str); 5] = [
    ("Amber", "#f6c344"),
    ("Green", "#8bc34a"),
    ("Blue", "#4a90d9"),
    ("Pink", "#e91e8c"),
    ("Red", "#e74c3c"),
];

/// Parse an annotation's stored `#rrggbb` hex colour into RGBA at the standard highlight
/// alpha (matching the original hardcoded `HIGHLIGHT_RGBA`'s opacity), falling back to that
/// same amber for anything that doesn't parse — a hand-edited sidecar entry, or one written
/// before the colour picker existed and so has no `color` at all.
fn annotation_rgba(hex: Option<&str>) -> [u8; 4] {
    let parsed = hex.and_then(|h| {
        let h = h.trim_start_matches('#');
        if h.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&h[0..2], 16).ok()?;
        let g = u8::from_str_radix(&h[2..4], 16).ok()?;
        let b = u8::from_str_radix(&h[4..6], 16).ok()?;
        Some([r, g, b, HIGHLIGHT_RGBA[3]])
    });
    parsed.unwrap_or(HIGHLIGHT_RGBA)
}

/// Wraps `picture` in an `Overlay` with a semi-transparent `DrawingArea` on top that tracks a
/// live drag rectangle, so a highlight/underline/strikeout is visible *while* it's being
/// dragged instead of only appearing once the drag ends. Returns the overlay (to use in the
/// widget tree in place of `picture`), the preview `DrawingArea` itself (drag handlers call
/// `.queue_draw()` on it after updating the rect), and the shared cell those handlers write
/// into — `Some((x0, y0, x1, y1))` in the picture's own pixel space while dragging, `None`
/// otherwise. The preview doesn't try to match the final narrowed underline/strikeout band —
/// it's just the raw drag rectangle in the current draw colour, which is enough to show what's
/// about to be created.
/// A drag rectangle in a picture's own pixel space: `(x0, y0, x1, y1)`.
type DragRectCell = Rc<Cell<Option<(f64, f64, f64, f64)>>>;
/// Self-referential slot for the notes-sidebar rebuild closure — a row's own delete button
/// needs to trigger a fresh rebuild of the list it lives in.
type RebuildNotesCell = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// `page_of` resolves which page this preview belongs to at draw time — a fixed value in
/// continuous-scroll mode (one overlay per page), but the reader's *current* page in paged
/// mode (one recycled overlay, page changes as the user navigates).
fn build_drag_preview_overlay(
    picture: &gtk4::Picture,
    reader: &Rc<RefCell<ReaderState>>,
    page_of: impl Fn() -> u16 + 'static,
) -> (gtk4::Overlay, gtk4::DrawingArea, DragRectCell) {
    let live_rect: DragRectCell = Rc::new(Cell::new(None));

    let preview = gtk4::DrawingArea::new();
    preview.set_can_target(false);
    preview.set_hexpand(true);
    preview.set_vexpand(true);
    {
        let live_rect = live_rect.clone();
        let reader = reader.clone();
        preview.set_draw_func(move |_area, cr, w, h| {
            let Some((x0, y0, x1, y1)) = live_rect.get() else {
                return;
            };
            let select_mode = reader.borrow().draw_kind.is_none();
            let rgba = if select_mode {
                SELECTION_RGBA
            } else {
                annotation_rgba(Some(&reader.borrow().draw_color))
            };
            cr.set_source_rgba(
                rgba[0] as f64 / 255.0,
                rgba[1] as f64 / 255.0,
                rgba[2] as f64 / 255.0,
                (rgba[3] as f64 / 255.0 * 1.6).min(1.0),
            );

            // Line-aware preview: what a drag from (x0,y0) to (x1,y1) would actually select,
            // not just its own bounding rectangle — matches what `save_drag_annotation`/
            // `copy_drag_selection` compute at drag-end, so the preview doesn't lie about
            // what's about to happen. Falls back to the plain rectangle over blank space
            // (a figure, a margin) where there's no text to hug.
            let page = page_of();
            let page_pts = {
                let r = reader.borrow();
                fond_doc::page_size(&r.pdfium, &r.bytes, page).unwrap_or((0.0, 0.0))
            };
            let line_rects = (page_pts.0 > 0.0 && page_pts.1 > 0.0 && w > 0 && h > 0)
                .then(|| {
                    let scale_x = w as f64 / page_pts.0 as f64;
                    let scale_y = h as f64 / page_pts.1 as f64;
                    let to_pdf = |px: f64, py: f64| {
                        let x = px.clamp(0.0, w as f64) / scale_x;
                        let y = page_pts.1 as f64 - py.clamp(0.0, h as f64) / scale_y;
                        (x, y)
                    };
                    let (sx, sy) = to_pdf(x0, y0);
                    let (ex, ey) = to_pdf(x1, y1);
                    let r = reader.borrow();
                    fond_doc::select_text_range(
                        &r.pdfium, &r.bytes, page, sx as f32, sy as f32, ex as f32, ey as f32,
                    )
                    .ok()
                    .flatten()
                })
                .flatten()
                .map(|sel| {
                    let scale_x = w as f64 / page_pts.0 as f64;
                    let scale_y = h as f64 / page_pts.1 as f64;
                    sel.quads
                        .iter()
                        .map(|q| {
                            let min_x = q[0].min(q[2]).min(q[4]).min(q[6]);
                            let max_x = q[0].max(q[2]).max(q[4]).max(q[6]);
                            let min_y = q[1].min(q[3]).min(q[5]).min(q[7]);
                            let max_y = q[1].max(q[3]).max(q[5]).max(q[7]);
                            (
                                min_x * scale_x,
                                h as f64 - max_y * scale_y,
                                max_x * scale_x,
                                h as f64 - min_y * scale_y,
                            )
                        })
                        .collect::<Vec<_>>()
                });

            match line_rects {
                Some(rects) if !rects.is_empty() => {
                    for (rx0, ry0, rx1, ry1) in rects {
                        cr.rectangle(
                            rx0.min(rx1),
                            ry0.min(ry1),
                            (rx1 - rx0).abs(),
                            (ry1 - ry0).abs(),
                        );
                    }
                }
                _ => {
                    let x = x0.min(x1);
                    let y = y0.min(y1);
                    cr.rectangle(x, y, (x1 - x0).abs(), (y1 - y0).abs());
                }
            }
            let _ = cr.fill();
        });
    }

    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(picture));
    overlay.add_overlay(&preview);

    (overlay, preview, live_rect)
}

/// The `DropDown`'s fixed option order — index into this, not into `AnnotationKind`
/// directly, since the drop-down deliberately excludes `Note` (drawn via its own button, not
/// a drag gesture) and adds a `None` "Select text" entry with no `AnnotationKind` of its own
/// (a drag in that mode copies to the clipboard instead of saving an annotation).
const DRAW_KIND_OPTIONS: [(&str, Option<fond_bib::AnnotationKind>); 4] = [
    ("Select text", None),
    ("Highlight", Some(fond_bib::AnnotationKind::Highlight)),
    ("Underline", Some(fond_bib::AnnotationKind::Underline)),
    ("Strikeout", Some(fond_bib::AnnotationKind::Strikeout)),
];

/// The EPUB reader's mode `DropDown` options — unlike `DRAW_KIND_OPTIONS`, no "Select
/// text" entry (the browser's native selection is always available regardless of this
/// mode) and no bare `Option` wrapper (every entry applies a real, always-selected kind).
const EPUB_MARK_KIND_OPTIONS: [(&str, fond_bib::AnnotationKind); 3] = [
    ("Highlight", fond_bib::AnnotationKind::Highlight),
    ("Underline", fond_bib::AnnotationKind::Underline),
    ("Strikeout", fond_bib::AnnotationKind::Strikeout),
];

const READER_BASE_WIDTH: f64 = 820.0;
/// Amber at ~55% opacity — a highlight tint, not a solid block.
const HIGHLIGHT_RGBA: [u8; 4] = [246, 195, 68, 140];
/// Blue at ~65% opacity — deliberately distinct from `HIGHLIGHT_RGBA` so the current search
/// match reads as "found this" and not as a saved highlight.
const SEARCH_MATCH_RGBA: [u8; 4] = [66, 133, 244, 165];
/// Blue at ~35% opacity, the live drag preview's tint in "Select text" mode — same hue
/// family as `SEARCH_MATCH_RGBA` (a "this is transient, not a saved mark" blue) but its own
/// constant since the two never appear together and may want to diverge later.
const SELECTION_RGBA: [u8; 4] = [66, 133, 244, 90];
/// Below this, a drag reads as a stray click, not an intentional highlight.
const MIN_DRAG_PX: f64 = 6.0;
/// Vertical gap between pages in continuous-scroll mode.
const CONTINUOUS_PAGE_GAP: f64 = 8.0;

/// The pointer shown over the page: an I-beam in "Select text" mode (this is a text
/// selection, not a highlight-drawing gesture), the default arrow otherwise. `None` means
/// "reset to default" — `Widget::set_cursor(None)` is how GTK4 clears a per-widget override.
fn cursor_for_select_mode(select_mode: bool) -> Option<gdk::Cursor> {
    select_mode
        .then(|| gdk::Cursor::from_name("text", None))
        .flatten()
}

/// Render `page` (0-based) to a ready-to-display texture, with this entry's saved
/// annotations — and, if `page` has the current search match, that match's highlight too —
/// blended in. Shared by both the page-by-page view and continuous-scroll mode so the two
/// can never visually disagree about what a page looks like. Returns the texture, its pixel
/// size, and the page's PDF-point size (the scale a drag-selected rectangle on that page
/// converts through).
fn render_pdf_page_texture(
    r: &ReaderState,
    page: u16,
) -> Option<(gdk::Texture, u32, u32, (f32, f32))> {
    let width = (READER_BASE_WIDTH * r.zoom) as u32;
    let page_pts = fond_doc::page_size(&r.pdfium, &r.bytes, page).unwrap_or((0.0, 0.0));
    let mut rp = fond_doc::render_page(&r.pdfium, &r.bytes, page, width).ok()?;
    let current_page = page as u32 + 1;

    // Note has no quad to draw (it's marginal, not on-page) — only the three drawable kinds
    // get blended. Each annotation keeps its own colour (from the colour picker at draw
    // time), so this blends per-annotation rather than batching every quad on the page into
    // one shared-colour call.
    for a in r
        .annotations
        .annotations
        .iter()
        .filter(|a| a.page == Some(current_page) && !a.quadpoints.is_empty())
    {
        let kind = match a.kind {
            fond_bib::AnnotationKind::Highlight => fond_doc::MarkupKind::Highlight,
            fond_bib::AnnotationKind::Underline => fond_doc::MarkupKind::Underline,
            fond_bib::AnnotationKind::Strikeout => fond_doc::MarkupKind::Strikeout,
            fond_bib::AnnotationKind::Note => continue,
        };
        let items: Vec<(fond_doc::MarkupKind, [f64; 8])> =
            a.quadpoints.iter().map(|q| (kind, *q)).collect();
        fond_doc::blend_annotations(
            &mut rp,
            page_pts.0,
            page_pts.1,
            &items,
            annotation_rgba(a.color.as_deref()),
        );
    }

    // The current search match, if it's on this page — blended in its own colour, on top of
    // any saved highlights, so it reads as "found this" and not as another saved annotation.
    if let Some(current) = r.search_matches.get(r.search_current) {
        if current.page == page {
            fond_doc::blend_highlights(
                &mut rp,
                page_pts.0,
                page_pts.1,
                &current.quads,
                SEARCH_MATCH_RGBA,
            );
        }
    }

    let data = glib::Bytes::from(&rp.rgba);
    let texture = gdk::MemoryTexture::new(
        rp.width as i32,
        rp.height as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &data,
        (rp.width * 4) as usize,
    );
    Some((texture.upcast(), rp.width, rp.height, page_pts))
}

/// Convert a drag gesture's start/end (widget-local pixel coordinates on `page`'s own
/// render, at that page's own `render_w`×`render_h` / `page_w_pts`×`page_h_pts` scale) into
/// PDF-space quadpoints — hugging real text if the drag covers any, same as
/// `select_text_in_rect` documents — and save a new annotation for it. The shared save path
/// for both the page-by-page view's drag handler and continuous-scroll mode's per-page ones,
/// so the two don't duplicate the coordinate math and sidecar write. Returns whether an
/// annotation was actually saved, so the caller knows whether a redraw is needed.
/// The geometry a drag gesture needs converted to PDF-space quadpoints: the page's own
/// rendered pixel size and PDF-point size (its scale), and the drag's start/end in that
/// pixel space. Grouped into one struct rather than eight loose parameters on
/// `save_drag_annotation`.
struct DragGeometry {
    render_w: u32,
    render_h: u32,
    page_w_pts: f32,
    page_h_pts: f32,
    start_x: f64,
    start_y: f64,
    end_x: f64,
    end_y: f64,
}

/// The PDF-space points a drag's start/end cover, given its pixel geometry — the inverse of
/// the page's render scale. Order-preserving (unlike a sorted rectangle), since
/// `select_text_range` needs to know which endpoint is the reading-order start. `None` if
/// the geometry is degenerate (zero-sized render, or a page with no reported point size).
fn drag_pdf_points(geom: &DragGeometry) -> Option<((f64, f64), (f64, f64))> {
    let &DragGeometry {
        render_w,
        render_h,
        page_w_pts,
        page_h_pts,
        start_x,
        start_y,
        end_x,
        end_y,
    } = geom;
    if render_w == 0 || render_h == 0 || page_w_pts <= 0.0 || page_h_pts <= 0.0 {
        return None;
    }
    let scale_x = render_w as f64 / page_w_pts as f64;
    let scale_y = render_h as f64 / page_h_pts as f64;
    let to_pdf = |px: f64, py: f64| {
        let x = px.clamp(0.0, render_w as f64) / scale_x;
        // PDF y is bottom-up; the drag's y is top-down pixel space.
        let y = page_h_pts as f64 - py.clamp(0.0, render_h as f64) / scale_y;
        (x, y)
    };
    Some((to_pdf(start_x, start_y), to_pdf(end_x, end_y)))
}

/// Copies the text under a "Select text" mode drag to the clipboard instead of saving an
/// annotation — the drag-to-annotate gesture's other mode. Remembers the selection (page +
/// text) on `reader` so a note added right after can quote it — see `last_selection`.
fn copy_drag_selection(
    widgets: &Rc<Widgets>,
    reader: &Rc<RefCell<ReaderState>>,
    page: u16,
    geom: &DragGeometry,
) {
    let Some((start, end)) = drag_pdf_points(geom) else {
        return;
    };
    let selection = {
        let r = reader.borrow();
        fond_doc::select_text_range(
            &r.pdfium,
            &r.bytes,
            page,
            start.0 as f32,
            start.1 as f32,
            end.0 as f32,
            end.1 as f32,
        )
        .ok()
        .flatten()
    };
    match selection {
        Some(sel) if !sel.text.trim().is_empty() => {
            if let Some(display) = gdk::Display::default() {
                display.clipboard().set_text(&sel.text);
            }
            reader.borrow_mut().last_selection = Some((page, sel.text));
            toast(widgets, "Copied to clipboard");
        }
        _ => toast(widgets, "No text found in selection"),
    }
}

fn save_drag_annotation(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    reader: &Rc<RefCell<ReaderState>>,
    pdf_hash: &str,
    page: u16,
    geom: DragGeometry,
) -> bool {
    let Some((start, end)) = drag_pdf_points(&geom) else {
        return false;
    };

    let (draw_kind, draw_color) = {
        let r = reader.borrow();
        (r.draw_kind, r.draw_color.clone())
    };
    // "Select text" mode has no `AnnotationKind` to save under — the caller routes that mode
    // to `copy_drag_selection` instead and never reaches here, but guard anyway.
    let Some(draw_kind) = draw_kind else {
        return false;
    };

    // Prefer the actual text under the drag, line-aware (a straight vertical drag through a
    // paragraph selects each in-between line in full, not just the narrow column under the
    // pointer) — over the drag rectangle's own bounding box. Falls back to the plain
    // rectangle when the drag covers no text (a figure, a blank margin).
    let (quads, snippet) = {
        let r = reader.borrow();
        fond_doc::select_text_range(
            &r.pdfium,
            &r.bytes,
            page,
            start.0 as f32,
            start.1 as f32,
            end.0 as f32,
            end.1 as f32,
        )
        .ok()
        .flatten()
    }
    .map(|sel| (sel.quads, Some(sel.text)))
    .unwrap_or_else(|| {
        let x0 = start.0.min(end.0);
        let x1 = start.0.max(end.0);
        let y_top = start.1.max(end.1);
        let y_bottom = start.1.min(end.1);
        (
            vec![[x0, y_top, x1, y_top, x0, y_bottom, x1, y_bottom]],
            None,
        )
    });

    let annotation = fond_bib::Annotation::drawn(
        draw_kind,
        page as u32 + 1,
        quads,
        snippet,
        None,
        Some(draw_color),
    );

    push_undo_snapshot(reader);
    {
        let mut r = reader.borrow_mut();
        r.annotations.pdf_hash = Some(pdf_hash.to_string());
        r.annotations.upsert(annotation);
    }

    let write_result = {
        let s = state.borrow();
        s.library
            .as_ref()
            .map(|lib| lib.write_annotations(&reader.borrow().annotations))
    };
    match write_result {
        Some(Ok(_)) => {
            let label = match draw_kind {
                fond_bib::AnnotationKind::Highlight => "Highlight added",
                fond_bib::AnnotationKind::Underline => "Underline added",
                fond_bib::AnnotationKind::Strikeout => "Strikeout added",
                fond_bib::AnnotationKind::Note => "Annotation added",
            };
            toast(widgets, label);
            true
        }
        Some(Err(e)) => {
            toast(widgets, &friendly::bib_error(&e));
            false
        }
        None => {
            toast(widgets, "No open library — not saved");
            false
        }
    }
}

/// Which annotation (if any) on `page` contains the PDF-space point `(x_pt, y_pt)` — the
/// topmost (most recently added) one whose quad bounding box covers the point, so a
/// right-click over overlapping highlights lands on the one the user most likely means.
/// Returns the annotation's id.
fn annotation_at_pdf_point(
    annotations: &fond_bib::AnnotationSidecar,
    page: u16,
    x_pt: f32,
    y_pt: f32,
) -> Option<String> {
    let page_num = page as u32 + 1;
    annotations
        .annotations
        .iter()
        .rev()
        .find(|a| {
            a.page == Some(page_num)
                && a.quadpoints.iter().any(|q| {
                    let min_x = q[0].min(q[2]).min(q[4]).min(q[6]) as f32;
                    let max_x = q[0].max(q[2]).max(q[4]).max(q[6]) as f32;
                    let min_y = q[1].min(q[3]).min(q[5]).min(q[7]) as f32;
                    let max_y = q[1].max(q[3]).max(q[5]).max(q[7]) as f32;
                    x_pt >= min_x && x_pt <= max_x && y_pt >= min_y && y_pt <= max_y
                })
        })
        .map(|a| a.id.clone())
}

/// Convert a click's picture-local pixel position to PDF-space point coordinates, given the
/// page's rendered pixel size and PDF-point size — the inverse of the drag-to-annotate math
/// in `save_drag_annotation`.
fn px_to_pdf_point(
    click_x: f64,
    click_y: f64,
    render_w: u32,
    render_h: u32,
    page_w_pts: f32,
    page_h_pts: f32,
) -> Option<(f32, f32)> {
    if render_w == 0 || render_h == 0 || page_w_pts <= 0.0 || page_h_pts <= 0.0 {
        return None;
    }
    let scale_x = render_w as f64 / page_w_pts as f64;
    let scale_y = render_h as f64 / page_h_pts as f64;
    let x_pt = (click_x / scale_x) as f32;
    // PDF y is bottom-up; the click's y is top-down pixel space.
    let y_pt = (page_h_pts as f64 - click_y / scale_y) as f32;
    Some((x_pt, y_pt))
}

/// The geometry a right-click needs to hit-test against a page's annotations and, if it
/// lands on one, resolve back to PDF space for the popover's own bookkeeping. Grouped like
/// `DragGeometry` for the same reason: fewer loose parameters on `show_pdf_context_menu`.
struct ClickGeometry {
    render_w: u32,
    render_h: u32,
    page_w_pts: f32,
    page_h_pts: f32,
    click_x: f64,
    click_y: f64,
}

/// Right-click context menu for the PDF page: if the click landed on an existing
/// highlight/underline/strikeout/note, offers to edit its note text or delete it; otherwise
/// offers to add a new marginal note. Replaces the old "This page" dropdown — editing and
/// deleting now happens at the annotation itself instead of a separate list.
#[allow(clippy::too_many_arguments)]
fn show_pdf_context_menu(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    reader: &Rc<RefCell<ReaderState>>,
    pdf_hash: &str,
    parent: &gtk4::Picture,
    refresh: Rc<dyn Fn()>,
    rebuild_notes: Rc<dyn Fn()>,
    undo_button: &gtk4::Button,
    redo_button: &gtk4::Button,
    page: u16,
    geom: ClickGeometry,
) {
    let ClickGeometry {
        render_w,
        render_h,
        page_w_pts,
        page_h_pts,
        click_x,
        click_y,
    } = geom;

    let hit_id = px_to_pdf_point(click_x, click_y, render_w, render_h, page_w_pts, page_h_pts)
        .and_then(|(x_pt, y_pt)| {
            annotation_at_pdf_point(&reader.borrow().annotations, page, x_pt, y_pt)
        });

    let popover = gtk4::Popover::new();
    popover.set_parent(parent);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(
        click_x.round() as i32,
        click_y.round() as i32,
        1,
        1,
    )));
    popover.set_has_arrow(true);

    let rows = gtk4::Box::new(Orientation::Vertical, 2);
    rows.set_margin_top(6);
    rows.set_margin_bottom(6);
    rows.set_margin_start(6);
    rows.set_margin_end(6);
    rows.set_width_request(240);

    match hit_id {
        Some(id) => {
            let annotation = reader
                .borrow()
                .annotations
                .annotations
                .iter()
                .find(|a| a.id == id)
                .cloned();
            let Some(annotation) = annotation else {
                return;
            };

            let kind_label = gtk4::Label::new(Some(&format!("{:?}", annotation.kind)));
            kind_label.set_xalign(0.0);
            kind_label.add_css_class("dim-label");
            rows.append(&kind_label);

            let note_entry = gtk4::Entry::new();
            note_entry.set_placeholder_text(Some("No note"));
            if let Some(note) = &annotation.note {
                note_entry.set_text(note);
            }
            rows.append(&note_entry);

            let save_note = {
                let state = state.clone();
                let widgets = widgets.clone();
                let reader = reader.clone();
                let id = id.clone();
                let undo_button = undo_button.clone();
                let redo_button = redo_button.clone();
                let rebuild_notes = rebuild_notes.clone();
                move |text: &str| {
                    let text = text.trim();
                    let current_note = reader
                        .borrow()
                        .annotations
                        .annotations
                        .iter()
                        .find(|a| a.id == id)
                        .and_then(|a| a.note.clone());
                    if current_note.as_deref().unwrap_or("") == text {
                        return;
                    }
                    push_undo_snapshot(&reader);
                    {
                        let mut r = reader.borrow_mut();
                        if let Some(a) = r.annotations.annotations.iter_mut().find(|a| a.id == id) {
                            a.note = (!text.is_empty()).then(|| text.to_string());
                        }
                    }
                    let write_result = {
                        let s = state.borrow();
                        s.library
                            .as_ref()
                            .map(|lib| lib.write_annotations(&reader.borrow().annotations))
                    };
                    match write_result {
                        Some(Ok(_)) => {
                            sync_undo_redo_buttons(&reader, &undo_button, &redo_button);
                            rebuild_notes();
                        }
                        Some(Err(e)) => toast(&widgets, &friendly::bib_error(&e)),
                        None => toast(&widgets, "No open library"),
                    }
                }
            };
            {
                let save_note = save_note.clone();
                note_entry.connect_activate(move |e| save_note(&e.text()));
            }
            {
                let focus = gtk4::EventControllerFocus::new();
                let save_note = save_note.clone();
                let note_entry_weak = note_entry.downgrade();
                focus.connect_leave(move |_| {
                    if let Some(e) = note_entry_weak.upgrade() {
                        save_note(&e.text());
                    }
                });
                note_entry.add_controller(focus);
            }

            rows.append(&popover_separator());
            let delete_button = popover_button("Delete annotation", true);
            {
                let state = state.clone();
                let widgets = widgets.clone();
                let reader = reader.clone();
                let refresh = refresh.clone();
                let rebuild_notes = rebuild_notes.clone();
                let popover = popover.clone();
                let id = id.clone();
                let undo_button = undo_button.clone();
                let redo_button = redo_button.clone();
                delete_button.connect_clicked(move |_| {
                    push_undo_snapshot(&reader);
                    reader
                        .borrow_mut()
                        .annotations
                        .annotations
                        .retain(|a| a.id != id);
                    let write_result = {
                        let s = state.borrow();
                        s.library
                            .as_ref()
                            .map(|lib| lib.write_annotations(&reader.borrow().annotations))
                    };
                    match write_result {
                        Some(Ok(_)) => {
                            refresh();
                            sync_undo_redo_buttons(&reader, &undo_button, &redo_button);
                            rebuild_notes();
                            toast(&widgets, "Annotation deleted");
                        }
                        Some(Err(e)) => toast(&widgets, &friendly::bib_error(&e)),
                        None => toast(&widgets, "No open library"),
                    }
                    popover.popdown();
                });
            }
            rows.append(&delete_button);
        }
        None => {
            let add_note = popover_button("Add note here", false);
            {
                let state = state.clone();
                let widgets = widgets.clone();
                let reader = reader.clone();
                let pdf_hash = pdf_hash.to_string();
                let undo_button = undo_button.clone();
                let redo_button = redo_button.clone();
                let popover = popover.clone();
                let rebuild_notes = rebuild_notes.clone();
                add_note.connect_clicked(move |_| {
                    show_pdf_note_dialog(
                        &state,
                        &widgets,
                        &reader,
                        &pdf_hash,
                        &undo_button,
                        &redo_button,
                        rebuild_notes.clone(),
                    );
                    popover.popdown();
                });
            }
            rows.append(&add_note);
        }
    }

    popover.set_child(Some(&rows));
    popover.connect_closed(|p| p.unparent());
    popover.popup();
}

/// Build continuous-scroll mode's per-page `Picture` widgets, if not already built. A no-op
/// if `reader.continuous_pictures` is already populated (from an earlier toggle-on this
/// session).
///
/// Widget layout (sizes and offsets) is computed eagerly from each page's cheap PDF-point
/// metadata alone — every page gets one *permanent* widget up front, so its drag-to-annotate
/// gesture can capture that page's index directly with no risk of a recycled widget later
/// belonging to a different page (the failure mode a `ListView`-based virtualized version
/// would have to guard against), and so scrolling to any page works immediately. Actually
/// rasterizing each page's texture is the expensive part (PDFium render), so that's deferred
/// to `schedule_continuous_render`, spread one page per idle tick starting from the reader's
/// current page — this used to run inline here, which blocked the whole UI thread for the
/// entire document on every open once continuous mode became the default (previously it only
/// cost anything on an explicit toggle-on, rare enough not to notice).
#[allow(clippy::too_many_arguments)]
fn build_continuous_view(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    reader: &Rc<RefCell<ReaderState>>,
    pdf_hash: &str,
    continuous_box: &gtk4::Box,
    undo_button: &gtk4::Button,
    redo_button: &gtk4::Button,
    rebuild_notes: &Rc<dyn Fn()>,
) {
    if !reader.borrow().continuous_pictures.is_empty() {
        return;
    }
    let (count, zoom, current_page) = {
        let r = reader.borrow();
        (r.count, r.zoom, r.page)
    };

    let mut pictures = Vec::with_capacity(count as usize);
    let mut offsets = Vec::with_capacity(count as usize + 1);
    let mut y = 0.0f64;

    let select_mode = reader.borrow().draw_kind.is_none();
    for page in 0..count {
        let picture = gtk4::Picture::new();
        picture.set_halign(gtk4::Align::Center);
        picture.set_can_target(true);
        picture.set_cursor(cursor_for_select_mode(select_mode).as_ref());

        // Plausible size from the page's own point dimensions — cheap metadata, not a
        // rasterization — so the layout is correct before this page's texture has rendered.
        let pts = {
            let r = reader.borrow();
            fond_doc::page_size(&r.pdfium, &r.bytes, page).unwrap_or((612.0, 792.0))
        };
        let w = (READER_BASE_WIDTH * zoom) as u32;
        let h = if pts.0 > 0.0 {
            (w as f32 * pts.1 / pts.0) as u32
        } else {
            (w as f32 * 792.0 / 612.0) as u32
        };
        picture.set_size_request(w as i32, h as i32);
        offsets.push(y);
        y += h as f64 + CONTINUOUS_PAGE_GAP;

        let (page_overlay, drag_preview, drag_live_rect) =
            build_drag_preview_overlay(&picture, reader, move || page);
        page_overlay.set_halign(gtk4::Align::Center);

        // Drag-to-annotate on this page's own permanent Picture — `page` is captured by
        // value, so (unlike a recycled `ListView` row) it can never go stale.
        {
            let drag = gtk4::GestureDrag::new();
            let state = state.clone();
            let widgets = widgets.clone();
            let reader = reader.clone();
            let pdf_hash = pdf_hash.to_string();
            let this_picture = picture.clone();
            let undo_button = undo_button.clone();
            let redo_button = redo_button.clone();
            let rebuild_notes = rebuild_notes.clone();
            {
                let live_rect = drag_live_rect.clone();
                let drag_preview = drag_preview.clone();
                drag.connect_drag_begin(move |_gesture, start_x, start_y| {
                    live_rect.set(Some((start_x, start_y, start_x, start_y)));
                    drag_preview.queue_draw();
                });
            }
            {
                let live_rect = drag_live_rect.clone();
                let drag_preview = drag_preview.clone();
                drag.connect_drag_update(move |gesture, offset_x, offset_y| {
                    let Some((start_x, start_y)) = gesture.start_point() else {
                        return;
                    };
                    live_rect.set(Some((
                        start_x,
                        start_y,
                        start_x + offset_x,
                        start_y + offset_y,
                    )));
                    drag_preview.queue_draw();
                });
            }
            {
                let live_rect = drag_live_rect.clone();
                let drag_preview = drag_preview.clone();
                drag.connect_drag_end(move |gesture, offset_x, offset_y| {
                    live_rect.set(None);
                    drag_preview.queue_draw();
                    if offset_x.abs() < MIN_DRAG_PX && offset_y.abs() < MIN_DRAG_PX {
                        return;
                    }
                    let Some((start_x, start_y)) = gesture.start_point() else {
                        return;
                    };
                    let end_x = start_x + offset_x;
                    let end_y = start_y + offset_y;
                    let render_w = this_picture.width().max(0) as u32;
                    let render_h = this_picture.height().max(0) as u32;
                    let page_pts = {
                        let r = reader.borrow();
                        fond_doc::page_size(&r.pdfium, &r.bytes, page).unwrap_or((0.0, 0.0))
                    };
                    let geom = DragGeometry {
                        render_w,
                        render_h,
                        page_w_pts: page_pts.0,
                        page_h_pts: page_pts.1,
                        start_x,
                        start_y,
                        end_x,
                        end_y,
                    };
                    if reader.borrow().draw_kind.is_none() {
                        copy_drag_selection(&widgets, &reader, page, &geom);
                        return;
                    }
                    let saved =
                        save_drag_annotation(&state, &widgets, &reader, &pdf_hash, page, geom);
                    if saved {
                        render_continuous_page(&reader, page);
                        sync_undo_redo_buttons(&reader, &undo_button, &redo_button);
                        rebuild_notes();
                    }
                });
            }
            picture.add_controller(drag);
        }

        // Right-click: edit/delete the annotation under the cursor, or add a note — same
        // context menu as the paged view, hit-testing against this page's own picture size
        // (each page can differ slightly after per-page render fallbacks).
        {
            let click = gtk4::GestureClick::new();
            click.set_button(gdk::BUTTON_SECONDARY);
            let state = state.clone();
            let widgets = widgets.clone();
            let reader = reader.clone();
            let pdf_hash = pdf_hash.to_string();
            let this_picture = picture.clone();
            let undo_button = undo_button.clone();
            let redo_button = redo_button.clone();
            let rebuild_notes = rebuild_notes.clone();
            click.connect_pressed(move |_gesture, _n, x, y| {
                let render_w = this_picture.width().max(0) as u32;
                let render_h = this_picture.height().max(0) as u32;
                let page_pts = {
                    let r = reader.borrow();
                    fond_doc::page_size(&r.pdfium, &r.bytes, page).unwrap_or((0.0, 0.0))
                };
                let refresh: Rc<dyn Fn()> = {
                    let reader = reader.clone();
                    Rc::new(move || render_continuous_page(&reader, page))
                };
                show_pdf_context_menu(
                    &state,
                    &widgets,
                    &reader,
                    &pdf_hash,
                    &this_picture,
                    refresh,
                    rebuild_notes.clone(),
                    &undo_button,
                    &redo_button,
                    page,
                    ClickGeometry {
                        render_w,
                        render_h,
                        page_w_pts: page_pts.0,
                        page_h_pts: page_pts.1,
                        click_x: x,
                        click_y: y,
                    },
                );
            });
            picture.add_controller(click);
        }

        continuous_box.append(&page_overlay);
        pictures.push(picture);
    }
    offsets.push(y); // sentinel: total content height

    {
        let mut r = reader.borrow_mut();
        r.continuous_pictures = pictures;
        r.continuous_offsets = offsets;
        r.continuous_rendered = vec![false; count as usize];
    }

    // Render nearest-to-current-page first, so the page the reader actually opened on (or
    // was showing before the toggle) fills in first — the rest follow outward from it.
    let mut order: Vec<u16> = (0..count).collect();
    order.sort_by_key(|&p| (p as i32 - current_page as i32).unsigned_abs());
    schedule_continuous_render(reader.clone(), order, 0);
}

/// Rasterize one page of continuous-scroll mode's `order` list, then yield back to the main
/// loop before rasterizing the next — spreads the expensive part of `build_continuous_view`
/// across idle ticks instead of blocking the UI for the whole document at once. Stops early
/// if the reader closed (`continuous_pictures` cleared, e.g. by a zoom-triggered rebuild)
/// out from under it, or if a fresher build already restarted from scratch (guarded by
/// comparing against the picture that's actually installed at this index, so a stale
/// in-flight schedule from before a rebuild can't render into pictures that no longer exist).
fn schedule_continuous_render(reader: Rc<RefCell<ReaderState>>, order: Vec<u16>, idx: usize) {
    let Some(&page) = order.get(idx) else {
        return;
    };
    let still_valid = reader
        .borrow()
        .continuous_pictures
        .get(page as usize)
        .is_some();
    if still_valid {
        render_continuous_page(&reader, page);
    }
    glib::idle_add_local_once(move || {
        schedule_continuous_render(reader, order, idx + 1);
    });
}

/// Tear down and rebuild continuous-scroll mode's widgets after a zoom change — page pixel
/// sizes all changed, so every offset is stale too. A no-op if continuous mode was never
/// built (the next toggle-on will build fresh at the new zoom already). Simpler than
/// resizing everything in place: zoom changes are infrequent, so paying a full rebuild is a
/// reasonable trade for not having two code paths (initial build vs. resize-in-place) to
/// keep in sync.
#[allow(clippy::too_many_arguments)]
fn rebuild_continuous_view_for_zoom(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    reader: &Rc<RefCell<ReaderState>>,
    pdf_hash: &str,
    continuous_box: &gtk4::Box,
    undo_button: &gtk4::Button,
    redo_button: &gtk4::Button,
    rebuild_notes: &Rc<dyn Fn()>,
) {
    if reader.borrow().continuous_pictures.is_empty() {
        return;
    }
    while let Some(child) = continuous_box.first_child() {
        continuous_box.remove(&child);
    }
    {
        let mut r = reader.borrow_mut();
        r.continuous_pictures.clear();
        r.continuous_offsets.clear();
        r.continuous_rendered.clear();
    }
    build_continuous_view(
        state,
        widgets,
        reader,
        pdf_hash,
        continuous_box,
        undo_button,
        redo_button,
        rebuild_notes,
    );
}

/// Re-render one page's `Picture` in continuous-scroll mode in place (after an annotation on
/// it changed) — its position doesn't move, only its content, so this doesn't touch
/// `continuous_offsets`.
fn render_continuous_page(reader: &Rc<RefCell<ReaderState>>, page: u16) {
    let picture = {
        let r = reader.borrow();
        r.continuous_pictures.get(page as usize).cloned()
    };
    let Some(picture) = picture else {
        return;
    };
    let mut r = reader.borrow_mut();
    if let Some((texture, w, h, _)) = render_pdf_page_texture(&r, page) {
        picture.set_paintable(Some(&texture));
        picture.set_size_request(w as i32, h as i32);
        if let Some(flag) = r.continuous_rendered.get_mut(page as usize) {
            *flag = true;
        }
    }
}

/// Scroll continuous-scroll mode's `ScrolledWindow` so `page` is at the top of the viewport.
fn scroll_continuous_to_page(
    reader: &Rc<RefCell<ReaderState>>,
    scroll: &gtk4::ScrolledWindow,
    page: u16,
) {
    let offset = {
        let r = reader.borrow();
        r.continuous_offsets
            .get(page as usize)
            .copied()
            .unwrap_or(0.0)
    };
    scroll.vadjustment().set_value(offset);
}

/// Which page's span contains vertical position `y` (both in continuous-scroll pixel space)
/// — the last page whose own top offset is at or above `y`. `offsets` is
/// `ReaderState.continuous_offsets`: `count` real page-top offsets plus one trailing
/// sentinel (the total content height), ascending.
fn continuous_page_at(offsets: &[f64], y: f64) -> u16 {
    if offsets.len() < 2 {
        return 0;
    }
    let count = offsets.len() - 1;
    let i = offsets[..count].partition_point(|&o| o <= y);
    i.saturating_sub(1).min(count.saturating_sub(1)) as u16
}

/// Refresh the page-number entry/label/prev-next sensitivity for `page` (0-based) — shared
/// by the paged view's `render()` and continuous mode's scroll-position tracker, so the two
/// can't disagree about how the current page is displayed. Shows the document's own printed
/// label when the PDF defines one (`page_labels[page]`), falling back to the raw 1-based
/// file position otherwise — identical to the pre-`/PageLabels`-aware behaviour for the
/// common case of a PDF with no custom numbering.
fn update_page_display(
    page_entry: &gtk4::Entry,
    page_of_label: &gtk4::Label,
    prev: &gtk4::Button,
    next: &gtk4::Button,
    page: u16,
    count: u16,
    page_labels: &[Option<String>],
) {
    let raw = (page + 1).to_string();
    let label = page_labels
        .get(page as usize)
        .and_then(|l| l.clone())
        .unwrap_or_else(|| raw.clone());
    page_entry.set_text(&label);
    page_of_label.set_text(&format!("of {count}"));
    // The tooltip always gives the raw file position too — a PDF's `/PageLabels` isn't
    // required to be unique or even present on every page, but the raw number is the one
    // every internal API here (`Annotation.page`, `PdfSearchMatch.page`, `Contents` targets)
    // always means, so it's worth surfacing even when the printed label differs.
    page_entry.set_tooltip_text(Some(&format!("Page {raw} of {count} in the file")));
    prev.set_sensitive(page > 0);
    next.set_sensitive(page + 1 < count);
}

/// Resolve typed text in the page-number entry to a 0-based page index: an exact match
/// against the document's own printed labels first (case-insensitive, since a roman numeral
/// typed in the "wrong" case should still work), falling back to parsing it as a raw 1-based
/// file page number — so typing still works exactly as before on a PDF with no
/// `/PageLabels`, and a user who prefers raw numbers can always use them even on one that
/// has them.
fn find_page_by_label(page_labels: &[Option<String>], text: &str) -> Option<u16> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(idx) = page_labels.iter().position(|l| l.as_deref() == Some(text)) {
        return Some(idx as u16);
    }
    let lower = text.to_lowercase();
    if let Some(idx) = page_labels
        .iter()
        .position(|l| l.as_deref().map(|s| s.to_lowercase()) == Some(lower.clone()))
    {
        return Some(idx as u16);
    }
    text.parse::<u16>().ok().and_then(|n| n.checked_sub(1))
}

/// A built-in PDF reader: renders pages with PDFium to RGBA textures, with page navigation,
/// zoom, and click-drag highlighting. No Poppler (GPL) — pure PDFium (BSD), the same binding
/// used for text extraction. Highlights are the on-disk `Annotation` sidecar format `fond-bib`
/// already defines (`annots/<key>.json`) — this is its first writer; until now only PDF
/// import/export touched it.
/// `start_page` is 1-based (matching `Annotation.page`), clamped into range; pass `1` to
/// just open at the first page.
fn show_pdf_reader(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    key: &str,
    pdf_hash: &str,
    blob: &std::path::Path,
    title: &str,
    start_page: u32,
) {
    let window = &widgets.window;
    let bytes = match std::fs::read(blob) {
        Ok(b) => b,
        Err(e) => {
            gtk4::AlertDialog::builder()
                .message("Could not open PDF")
                .detail(e.to_string())
                .build()
                .show(Some(window));
            return;
        }
    };
    let pdfium = match fond_doc::bind_pdfium() {
        Ok(p) => p,
        Err(e) => {
            gtk4::AlertDialog::builder()
                .message("PDF reader unavailable")
                .detail(format!("PDFium could not be loaded: {e}"))
                .build()
                .show(Some(window));
            return;
        }
    };
    let count = fond_doc::page_count(&pdfium, &bytes).unwrap_or(1).max(1);
    // Empty for most PDFs — outlines are the exception, not the rule — so the Contents
    // button below only appears when there's actually something to jump to.
    let outline_entries = fond_doc::outline(&pdfium, &bytes).unwrap_or_default();
    // Likewise empty for most PDFs (no custom /PageLabels) — falls back to the raw page
    // number wherever it's displayed.
    let page_labels = fond_doc::page_labels(&pdfium, &bytes).unwrap_or_default();

    let annotations = state
        .borrow()
        .library
        .as_ref()
        .and_then(|lib| lib.load_annotations(key).ok().flatten())
        .unwrap_or_else(|| fond_bib::AnnotationSidecar::new(key));

    let start_page = start_page
        .saturating_sub(1)
        .min(count.saturating_sub(1) as u32) as u16;
    let reader = Rc::new(RefCell::new(ReaderState {
        pdfium,
        bytes,
        page: start_page,
        count,
        zoom: 1.0,
        annotations,
        render_px: (0, 0),
        page_pts: (0.0, 0.0),
        draw_kind: Some(fond_bib::AnnotationKind::Highlight),
        last_selection: None,
        search_matches: Vec::new(),
        search_current: 0,
        draw_color: COLOR_PRESETS[0].1.to_string(),
        continuous_pictures: Vec::new(),
        continuous_offsets: Vec::new(),
        continuous_rendered: Vec::new(),
        page_labels,
        undo_stack: Vec::new(),
        redo_stack: Vec::new(),
    }));

    let dialog = adw::Window::new();
    dialog.set_title(Some(title));
    dialog.set_transient_for(Some(window));
    dialog.set_default_size(900, 820);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");

    let prev = gtk4::Button::from_icon_name("go-previous-symbolic");
    prev.add_css_class("flat");
    prev.set_tooltip_text(Some("Previous page"));
    let next = gtk4::Button::from_icon_name("go-next-symbolic");
    next.add_css_class("flat");
    next.set_tooltip_text(Some("Next page"));
    // Shows (and, on Enter, navigates by) the *document's own* printed page number — its
    // `/PageLabels` numbering, e.g. roman-numeral front matter restarting at arabic "1" for
    // the body — the way Zotero's reader does, rather than always the raw file position.
    // Most PDFs have no `/PageLabels` at all, in which case this just shows the raw number,
    // identical to before.
    let page_entry = gtk4::Entry::new();
    page_entry.set_width_chars(5);
    page_entry.set_max_width_chars(5);
    gtk4::prelude::EntryExt::set_alignment(&page_entry, 0.5);
    let page_of_label = gtk4::Label::new(None);
    page_of_label.add_css_class("dim-label");
    // Page nav lives in the bottom status bar (below), not the headerbar's title-widget slot
    // — that slot is left to the default window title (the document's own name) instead.
    let nav = gtk4::Box::new(Orientation::Horizontal, 6);
    nav.append(&prev);
    nav.append(&page_entry);
    nav.append(&page_of_label);
    nav.append(&next);

    let zoom_out = gtk4::Button::from_icon_name("zoom-out-symbolic");
    zoom_out.add_css_class("flat");
    zoom_out.set_tooltip_text(Some("Zoom out"));
    let zoom_in = gtk4::Button::from_icon_name("zoom-in-symbolic");
    zoom_in.add_css_class("flat");
    zoom_in.set_tooltip_text(Some("Zoom in"));

    let note_button = gtk4::Button::with_label("Note…");
    note_button.set_tooltip_text(Some("Add a marginal note on the current page"));

    let mode_labels: Vec<&str> = DRAW_KIND_OPTIONS.iter().map(|(l, _)| *l).collect();
    let mode_drop = gtk4::DropDown::from_strings(&mode_labels);
    mode_drop.set_tooltip_text(Some("What a drag on the page does"));
    // Index 0 is "Select text" — start on "Highlight" (index 1) to match `ReaderState`'s own
    // default and the reader's original drag behaviour; `connect_selected_notify` below only
    // fires on a change, not on construction, so leaving this at the DropDown's own default
    // of 0 would show "Select text" while every drag still highlighted until first touched.
    mode_drop.set_selected(1);

    let color_labels: Vec<&str> = COLOR_PRESETS.iter().map(|(l, _)| *l).collect();
    let color_drop = gtk4::DropDown::from_strings(&color_labels);
    color_drop.set_tooltip_text(Some("Highlight colour"));

    // Only present when the PDF actually has an outline — most don't. Toggles a persistent
    // sidebar (built below, after `render`/`reader` exist) rather than a popover, per
    // CLAUDE.md's house sidebar style: toggle at the *start* of the headerbar, content as a
    // collapsible Paned start-child.
    let sidebar_toggle = (!outline_entries.is_empty()).then(|| {
        let button = gtk4::ToggleButton::new();
        button.set_icon_name("sidebar-show-symbolic");
        button.set_tooltip_text(Some("Show the table of contents"));
        button
    });

    // Whole-document notes/highlights list, in a persistent sidebar (built below, alongside
    // Contents) rather than the old per-page "This page" dropdown — readable prose, not just
    // on-page markers, and reachable regardless of which page is current. Editing/deleting
    // an individual annotation now happens by right-clicking it on the page itself.
    let notes_toggle = gtk4::ToggleButton::new();
    notes_toggle.set_icon_name("view-list-symbolic");
    notes_toggle.set_tooltip_text(Some("Show notes and highlights"));

    let continuous_toggle = gtk4::ToggleButton::with_label("Continuous");
    continuous_toggle.set_tooltip_text(Some(
        "Scroll continuously through every page, instead of one page at a time",
    ));

    let undo_button = gtk4::Button::from_icon_name("edit-undo-symbolic");
    undo_button.set_tooltip_text(Some("Undo (Ctrl+Z)"));
    undo_button.set_sensitive(false);
    let redo_button = gtk4::Button::from_icon_name("edit-redo-symbolic");
    redo_button.set_tooltip_text(Some("Redo (Ctrl+Shift+Z)"));
    redo_button.set_sensitive(false);

    // pack_end order is the reverse of visual order (last-packed ends up leftmost) — same
    // gotcha CLAUDE.md notes for the hamburger menu. Visual order here, left to right:
    // Continuous, mode picker, colour picker, Note. The Contents/Notes sidebar toggles and
    // Undo/Redo live at the *start* of the headerbar instead (house style for the sidebar
    // toggle; Undo/Redo follow it for the same "persistent chrome, not a per-mode control"
    // reasoning). Page nav and zoom move to the bottom status bar (below) so the headerbar's
    // title-widget slot stays free for the document's own name — a wide title plus this many
    // controls didn't fit together.
    header.pack_end(&note_button);
    header.pack_end(&color_drop);
    header.pack_end(&mode_drop);
    header.pack_end(&continuous_toggle);
    if let Some(sidebar_toggle) = &sidebar_toggle {
        header.pack_start(sidebar_toggle);
    }
    header.pack_start(&notes_toggle);
    header.pack_start(&undo_button);
    header.pack_start(&redo_button);
    view.add_top_bar(&header);

    // Status bar (house style, same classes as the main window's): page nav on the left,
    // zoom on the right.
    let statusbar = gtk4::Box::new(Orientation::Horizontal, 6);
    statusbar.add_css_class("toolbar");
    statusbar.add_css_class("fond-chrome");
    statusbar.add_css_class("fond-statusbar");
    let statusbar_spacer = gtk4::Box::new(Orientation::Horizontal, 0);
    statusbar_spacer.set_hexpand(true);
    statusbar.append(&nav);
    statusbar.append(&statusbar_spacer);
    statusbar.append(&zoom_out);
    statusbar.append(&zoom_in);
    view.add_bottom_bar(&statusbar);

    let hint = gtk4::Label::new(Some("Drag over the page to add a highlight"));
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    hint.set_margin_top(4);
    hint.set_margin_bottom(4);

    let search_entry = gtk4::SearchEntry::new();
    search_entry.set_placeholder_text(Some("Search this PDF…"));
    search_entry.set_hexpand(true);
    let search_prev = gtk4::Button::from_icon_name("go-up-symbolic");
    search_prev.set_tooltip_text(Some("Previous match"));
    search_prev.set_sensitive(false);
    let search_next = gtk4::Button::from_icon_name("go-down-symbolic");
    search_next.set_tooltip_text(Some("Next match"));
    search_next.set_sensitive(false);
    let search_count = gtk4::Label::new(None);
    search_count.add_css_class("dim-label");
    let search_row = gtk4::Box::new(Orientation::Horizontal, 6);
    search_row.set_margin_start(8);
    search_row.set_margin_end(8);
    search_row.set_margin_top(4);
    search_row.append(&search_entry);
    search_row.append(&search_count);
    search_row.append(&search_prev);
    search_row.append(&search_next);

    let picture = gtk4::Picture::new();
    picture.set_halign(gtk4::Align::Center);
    picture.set_valign(gtk4::Align::Start);
    picture.set_can_target(true);
    let (picture_overlay, drag_preview, drag_live_rect) = {
        let reader_for_page = reader.clone();
        build_drag_preview_overlay(&picture, &reader, move || reader_for_page.borrow().page)
    };
    picture_overlay.set_halign(gtk4::Align::Center);
    picture_overlay.set_valign(gtk4::Align::Start);
    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_child(Some(&picture_overlay));
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);

    // Continuous-scroll mode's surface: an empty Box for now — populated with one Picture
    // per page the first time the mode is toggled on (`build_continuous_view` below), not
    // eagerly here, so a plain "Read" stays exactly as fast as it always was.
    let continuous_box = gtk4::Box::new(Orientation::Vertical, CONTINUOUS_PAGE_GAP as i32);
    continuous_box.set_margin_top(CONTINUOUS_PAGE_GAP as i32);
    continuous_box.set_margin_bottom(CONTINUOUS_PAGE_GAP as i32);
    let continuous_scroll = gtk4::ScrolledWindow::new();
    continuous_scroll.set_child(Some(&continuous_box));
    continuous_scroll.set_vexpand(true);
    continuous_scroll.set_hexpand(true);

    let view_stack = gtk4::Stack::new();
    view_stack.add_named(&scroll, Some("paged"));
    view_stack.add_named(&continuous_scroll, Some("continuous"));
    view_stack.set_visible_child_name("paged");

    let content = gtk4::Box::new(Orientation::Vertical, 0);
    content.append(&search_row);
    content.append(&hint);
    content.append(&view_stack);
    // `content` is reparented into the sidebar Paned below instead of set directly here —
    // the Notes sidebar (and, when present, Contents) always builds that Paned now, and
    // `Paned::set_end_child` asserts its child has no existing parent.
    dialog.set_content(Some(&view));

    // Render the current page into the Picture (via the shared helper both this view and
    // continuous-scroll mode use), and refresh the page label.
    let render = {
        let reader = reader.clone();
        let picture = picture.clone();
        let page_entry = page_entry.clone();
        let page_of_label = page_of_label.clone();
        let prev = prev.clone();
        let next = next.clone();
        Rc::new(move || {
            let mut r = reader.borrow_mut();
            match render_pdf_page_texture(&r, r.page) {
                Some((texture, w, h, page_pts)) => {
                    r.render_px = (w, h);
                    r.page_pts = page_pts;
                    picture.set_paintable(Some(&texture));
                    picture.set_size_request(w as i32, h as i32);
                }
                None => picture.set_paintable(gdk::Paintable::NONE),
            }
            update_page_display(
                &page_entry,
                &page_of_label,
                &prev,
                &next,
                r.page,
                r.count,
                &r.page_labels,
            );
        })
    };
    render();

    // Undo/redo: a snapshot doesn't record which page(s) it touched, so pop it and
    // re-render everything — cheap even in continuous mode, since each page's blend is just
    // a texture re-render, not a re-parse of the PDF.
    let rerender_all_pages = {
        let reader = reader.clone();
        let render = render.clone();
        Rc::new(move || {
            render();
            let count = reader.borrow().count;
            for page in 0..count {
                render_continuous_page(&reader, page);
            }
        })
    };
    let undo = {
        let reader = reader.clone();
        let state = state.clone();
        let widgets = widgets.clone();
        let rerender_all_pages = rerender_all_pages.clone();
        let undo_button = undo_button.clone();
        let redo_button = redo_button.clone();
        Rc::new(move || {
            let popped = {
                let mut r = reader.borrow_mut();
                match r.undo_stack.pop() {
                    Some(prev) => {
                        let current = r.annotations.clone();
                        r.redo_stack.push(current);
                        r.annotations = prev;
                        true
                    }
                    None => false,
                }
            };
            if !popped {
                toast(&widgets, "Nothing to undo");
                return;
            }
            let write_result = {
                let s = state.borrow();
                s.library
                    .as_ref()
                    .map(|lib| lib.write_annotations(&reader.borrow().annotations))
            };
            match write_result {
                Some(Ok(_)) => {
                    rerender_all_pages();
                    toast(&widgets, "Undid last annotation change");
                }
                Some(Err(e)) => toast(&widgets, &format!("Could not undo: {e}")),
                None => toast(&widgets, "No open library"),
            }
            sync_undo_redo_buttons(&reader, &undo_button, &redo_button);
        })
    };
    let redo = {
        let reader = reader.clone();
        let state = state.clone();
        let widgets = widgets.clone();
        let rerender_all_pages = rerender_all_pages.clone();
        let undo_button = undo_button.clone();
        let redo_button = redo_button.clone();
        Rc::new(move || {
            let popped = {
                let mut r = reader.borrow_mut();
                match r.redo_stack.pop() {
                    Some(next) => {
                        let current = r.annotations.clone();
                        r.undo_stack.push(current);
                        r.annotations = next;
                        true
                    }
                    None => false,
                }
            };
            if !popped {
                toast(&widgets, "Nothing to redo");
                return;
            }
            let write_result = {
                let s = state.borrow();
                s.library
                    .as_ref()
                    .map(|lib| lib.write_annotations(&reader.borrow().annotations))
            };
            match write_result {
                Some(Ok(_)) => {
                    rerender_all_pages();
                    toast(&widgets, "Redid annotation change");
                }
                Some(Err(e)) => toast(&widgets, &format!("Could not redo: {e}")),
                None => toast(&widgets, "No open library"),
            }
            sync_undo_redo_buttons(&reader, &undo_button, &redo_button);
        })
    };
    {
        let undo = undo.clone();
        undo_button.connect_clicked(move |_| undo());
    }
    {
        let redo = redo.clone();
        redo_button.connect_clicked(move |_| redo());
    }
    {
        let key_controller = gtk4::EventControllerKey::new();
        let undo = undo.clone();
        let redo = redo.clone();
        key_controller.connect_key_pressed(move |_, keyval, _keycode, modifiers| {
            if keyval == gdk::Key::z && modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
                if modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
                    redo();
                } else {
                    undo();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        dialog.add_controller(key_controller);
    }

    // Contents/Notes sidebar: persistent (not a popover) so it stays visible while
    // navigating, per Cal's request. Both panels share one Paned start-child slot via a
    // Stack, since only one is useful to see at a time; the two toggles are mutually
    // exclusive (activating one deactivates the other) but each can still be clicked again
    // to close the sidebar entirely, unlike a strict radio-group.
    let contents_scroll = sidebar_toggle.as_ref().map(|_| {
        let rows = gtk4::Box::new(Orientation::Vertical, 2);
        rows.set_margin_top(6);
        rows.set_margin_bottom(6);
        rows.set_margin_start(6);
        rows.set_margin_end(6);
        for entry in &outline_entries {
            let label = format!("{}{}", "    ".repeat(entry.depth as usize), entry.title);
            let row = popover_button(&label, false);
            if let Some(lbl) = row.child().and_then(|w| w.downcast::<gtk4::Label>().ok()) {
                lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            }
            if let Some(page) = entry.page {
                let reader = reader.clone();
                let render = render.clone();
                let continuous_toggle = continuous_toggle.clone();
                let continuous_scroll = continuous_scroll.clone();
                row.connect_clicked(move |_| {
                    let target = {
                        let r = reader.borrow();
                        (page.saturating_sub(1)).min(r.count.saturating_sub(1))
                    };
                    if continuous_toggle.is_active() {
                        scroll_continuous_to_page(&reader, &continuous_scroll, target);
                    } else {
                        reader.borrow_mut().page = target;
                        render();
                    }
                });
            } else {
                row.set_sensitive(false);
            }
            rows.append(&row);
        }
        let scroll = gtk4::ScrolledWindow::new();
        scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        scroll.set_child(Some(&rows));
        scroll
    });

    // Notes/highlights list: every annotation in the document, readable prose rather than
    // just on-page markers, sorted by page. Rebuilt fresh (`rebuild_notes`, below) whenever
    // shown or whenever an annotation is added/removed elsewhere in the reader, via the
    // `Rc<RefCell<Option<...>>>` indirection so a row's own delete button can trigger a
    // rebuild of the list it lives in.
    let notes_rows = gtk4::Box::new(Orientation::Vertical, 2);
    notes_rows.set_margin_top(6);
    notes_rows.set_margin_bottom(6);
    notes_rows.set_margin_start(6);
    notes_rows.set_margin_end(6);
    let notes_scroll = gtk4::ScrolledWindow::new();
    notes_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    notes_scroll.set_child(Some(&notes_rows));

    let rebuild_notes_cell: RebuildNotesCell = Rc::new(RefCell::new(None));
    {
        let notes_rows = notes_rows.clone();
        let state = state.clone();
        let widgets = widgets.clone();
        let reader = reader.clone();
        let render = render.clone();
        let continuous_toggle = continuous_toggle.clone();
        let continuous_scroll = continuous_scroll.clone();
        let undo_button = undo_button.clone();
        let redo_button = redo_button.clone();
        let rebuild_notes_cell_inner = rebuild_notes_cell.clone();
        let builder = move || {
            while let Some(child) = notes_rows.first_child() {
                notes_rows.remove(&child);
            }
            let mut all: Vec<fond_bib::Annotation> = reader
                .borrow()
                .annotations
                .annotations
                .iter()
                .filter(|a| a.page.is_some())
                .cloned()
                .collect();
            all.sort_by(|a, b| (a.page, &a.created).cmp(&(b.page, &b.created)));
            if all.is_empty() {
                let label = gtk4::Label::new(Some("No notes or highlights yet"));
                label.add_css_class("dim-label");
                label.set_margin_top(6);
                label.set_margin_bottom(6);
                notes_rows.append(&label);
                return;
            }
            let last = all.len().saturating_sub(1);
            for (i, annotation) in all.into_iter().enumerate() {
                let page_num = annotation.page.unwrap_or(1);
                let outer = gtk4::Box::new(Orientation::Vertical, 2);

                let header_box = gtk4::Box::new(Orientation::Horizontal, 6);
                let header_label =
                    gtk4::Label::new(Some(&format!("p.{page_num} — {:?}", annotation.kind)));
                header_label.set_xalign(0.0);
                header_label.set_hexpand(true);
                header_label.add_css_class("dim-label");
                header_label.add_css_class("caption-heading");
                header_box.append(&header_label);
                let delete_button = gtk4::Button::from_icon_name("user-trash-symbolic");
                delete_button.add_css_class("flat");
                delete_button.set_tooltip_text(Some("Delete this annotation"));
                header_box.append(&delete_button);
                outer.append(&header_box);

                {
                    let jump = gtk4::GestureClick::new();
                    let reader = reader.clone();
                    let render = render.clone();
                    let continuous_toggle = continuous_toggle.clone();
                    let continuous_scroll = continuous_scroll.clone();
                    jump.connect_released(move |_gesture, _n, _x, _y| {
                        let target = (page_num.saturating_sub(1))
                            .min(reader.borrow().count.saturating_sub(1) as u32)
                            as u16;
                        if continuous_toggle.is_active() {
                            scroll_continuous_to_page(&reader, &continuous_scroll, target);
                        } else {
                            reader.borrow_mut().page = target;
                            render();
                        }
                    });
                    header_label.add_controller(jump);
                }

                if let Some(snippet) = &annotation.snippet {
                    let snippet_label = gtk4::Label::new(Some(snippet));
                    snippet_label.set_xalign(0.0);
                    snippet_label.set_wrap(true);
                    snippet_label.add_css_class("dim-label");
                    snippet_label.add_css_class("caption");
                    outer.append(&snippet_label);
                }

                let note_entry = gtk4::Entry::new();
                note_entry.set_placeholder_text(Some("No note"));
                if let Some(note) = &annotation.note {
                    note_entry.set_text(note);
                }
                outer.append(&note_entry);

                let save_note = {
                    let state = state.clone();
                    let widgets = widgets.clone();
                    let reader = reader.clone();
                    let id = annotation.id.clone();
                    let undo_button = undo_button.clone();
                    let redo_button = redo_button.clone();
                    move |text: &str| {
                        let text = text.trim();
                        let current_note = reader
                            .borrow()
                            .annotations
                            .annotations
                            .iter()
                            .find(|a| a.id == id)
                            .and_then(|a| a.note.clone());
                        if current_note.as_deref().unwrap_or("") == text {
                            return;
                        }
                        push_undo_snapshot(&reader);
                        {
                            let mut r = reader.borrow_mut();
                            if let Some(a) =
                                r.annotations.annotations.iter_mut().find(|a| a.id == id)
                            {
                                a.note = (!text.is_empty()).then(|| text.to_string());
                            }
                        }
                        let write_result = {
                            let s = state.borrow();
                            s.library
                                .as_ref()
                                .map(|lib| lib.write_annotations(&reader.borrow().annotations))
                        };
                        match write_result {
                            Some(Ok(_)) => {
                                sync_undo_redo_buttons(&reader, &undo_button, &redo_button);
                            }
                            Some(Err(e)) => toast(&widgets, &friendly::bib_error(&e)),
                            None => toast(&widgets, "No open library"),
                        }
                    }
                };
                {
                    let save_note = save_note.clone();
                    note_entry.connect_activate(move |e| save_note(&e.text()));
                }
                {
                    let focus = gtk4::EventControllerFocus::new();
                    let save_note = save_note.clone();
                    let note_entry_weak = note_entry.downgrade();
                    focus.connect_leave(move |_| {
                        if let Some(e) = note_entry_weak.upgrade() {
                            save_note(&e.text());
                        }
                    });
                    note_entry.add_controller(focus);
                }

                {
                    let state = state.clone();
                    let widgets = widgets.clone();
                    let reader = reader.clone();
                    let render = render.clone();
                    let id = annotation.id.clone();
                    let undo_button = undo_button.clone();
                    let redo_button = redo_button.clone();
                    let rebuild_notes_cell = rebuild_notes_cell_inner.clone();
                    delete_button.connect_clicked(move |_| {
                        push_undo_snapshot(&reader);
                        reader
                            .borrow_mut()
                            .annotations
                            .annotations
                            .retain(|a| a.id != id);
                        let write_result = {
                            let s = state.borrow();
                            s.library
                                .as_ref()
                                .map(|lib| lib.write_annotations(&reader.borrow().annotations))
                        };
                        match write_result {
                            Some(Ok(_)) => {
                                render();
                                render_continuous_page(
                                    &reader,
                                    (page_num.saturating_sub(1)) as u16,
                                );
                                sync_undo_redo_buttons(&reader, &undo_button, &redo_button);
                                toast(&widgets, "Annotation deleted");
                                if let Some(f) = rebuild_notes_cell.borrow().as_ref() {
                                    f();
                                }
                            }
                            Some(Err(e)) => toast(&widgets, &friendly::bib_error(&e)),
                            None => toast(&widgets, "No open library"),
                        }
                    });
                }

                notes_rows.append(&outer);
                if i != last {
                    notes_rows.append(&popover_separator());
                }
            }
        };
        *rebuild_notes_cell.borrow_mut() = Some(Rc::new(builder));
    }
    let rebuild_notes: Rc<dyn Fn()> = {
        let cell = rebuild_notes_cell.clone();
        Rc::new(move || {
            let f = cell.borrow().clone();
            if let Some(f) = f {
                f();
            }
        })
    };

    let sidebar_stack = gtk4::Stack::new();
    if let Some(contents_scroll) = &contents_scroll {
        sidebar_stack.add_named(contents_scroll, Some("contents"));
    }
    sidebar_stack.add_named(&notes_scroll, Some("notes"));
    // Width is user-adjustable via the Paned handle, not fixed to the longest label — start
    // at a reasonable default but let it be dragged down to a slim strip.
    sidebar_stack.set_size_request(60, -1);
    sidebar_stack.set_vexpand(true);

    let paned = gtk4::Paned::new(Orientation::Horizontal);
    paned.set_start_child(gtk4::Widget::NONE);
    paned.set_resize_start_child(false);
    paned.set_shrink_start_child(true);
    paned.set_end_child(Some(&content));
    paned.set_vexpand(true);
    paned.set_hexpand(true);
    paned.set_position(220);
    view.set_content(Some(&paned));

    if let Some(sidebar_toggle) = &sidebar_toggle {
        let paned = paned.clone();
        let sidebar_stack = sidebar_stack.clone();
        let notes_toggle = notes_toggle.clone();
        sidebar_toggle.connect_toggled(move |btn| {
            if btn.is_active() {
                notes_toggle.set_active(false);
                sidebar_stack.set_visible_child_name("contents");
                paned.set_start_child(Some(&sidebar_stack));
            } else if !notes_toggle.is_active() {
                paned.set_start_child(gtk4::Widget::NONE);
            }
        });
    }
    {
        let paned = paned.clone();
        let sidebar_stack = sidebar_stack.clone();
        let sidebar_toggle = sidebar_toggle.clone();
        let rebuild_notes = rebuild_notes.clone();
        notes_toggle.connect_toggled(move |btn| {
            if btn.is_active() {
                if let Some(st) = &sidebar_toggle {
                    st.set_active(false);
                }
                rebuild_notes();
                sidebar_stack.set_visible_child_name("notes");
                paned.set_start_child(Some(&sidebar_stack));
            } else if sidebar_toggle
                .as_ref()
                .map(|b| !b.is_active())
                .unwrap_or(true)
            {
                paned.set_start_child(gtk4::Widget::NONE);
            }
        });
    }

    // Click-drag on the page creates a highlight: the dragged rectangle (in the render's own
    // pixel grid — the Picture is size-requested to exactly that, so widget-local coordinates
    // from the gesture already are that grid) converts to PDF-space quadpoints via the current
    // page's point size, and is appended to the sidecar and written straight to disk.
    {
        let drag = gtk4::GestureDrag::new();
        let reader = reader.clone();
        let render = render.clone();
        let state = state.clone();
        let widgets = widgets.clone();
        let pdf_hash = pdf_hash.to_string();
        let undo_button = undo_button.clone();
        let redo_button = redo_button.clone();
        let rebuild_notes = rebuild_notes.clone();
        {
            let live_rect = drag_live_rect.clone();
            let drag_preview = drag_preview.clone();
            drag.connect_drag_begin(move |_gesture, start_x, start_y| {
                live_rect.set(Some((start_x, start_y, start_x, start_y)));
                drag_preview.queue_draw();
            });
        }
        {
            let live_rect = drag_live_rect.clone();
            let drag_preview = drag_preview.clone();
            drag.connect_drag_update(move |gesture, offset_x, offset_y| {
                let Some((start_x, start_y)) = gesture.start_point() else {
                    return;
                };
                live_rect.set(Some((
                    start_x,
                    start_y,
                    start_x + offset_x,
                    start_y + offset_y,
                )));
                drag_preview.queue_draw();
            });
        }
        {
            let live_rect = drag_live_rect.clone();
            let drag_preview = drag_preview.clone();
            drag.connect_drag_end(move |gesture, offset_x, offset_y| {
                live_rect.set(None);
                drag_preview.queue_draw();
                if offset_x.abs() < MIN_DRAG_PX && offset_y.abs() < MIN_DRAG_PX {
                    return;
                }
                let Some((start_x, start_y)) = gesture.start_point() else {
                    return;
                };
                let end_x = start_x + offset_x;
                let end_y = start_y + offset_y;

                let (page, render_w, render_h, page_w_pts, page_h_pts) = {
                    let r = reader.borrow();
                    (
                        r.page,
                        r.render_px.0,
                        r.render_px.1,
                        r.page_pts.0,
                        r.page_pts.1,
                    )
                };
                let geom = DragGeometry {
                    render_w,
                    render_h,
                    page_w_pts,
                    page_h_pts,
                    start_x,
                    start_y,
                    end_x,
                    end_y,
                };
                if reader.borrow().draw_kind.is_none() {
                    copy_drag_selection(&widgets, &reader, page, &geom);
                    return;
                }
                let saved = save_drag_annotation(&state, &widgets, &reader, &pdf_hash, page, geom);
                if saved {
                    render();
                    // Keep continuous mode's copy of this page in sync too, in case it was
                    // already built from an earlier toggle-on and the user drew this
                    // highlight after switching back to the paged view.
                    render_continuous_page(&reader, page);
                    sync_undo_redo_buttons(&reader, &undo_button, &redo_button);
                    rebuild_notes();
                }
            });
        }
        picture.add_controller(drag);
    }

    // Right-click: edit or delete the annotation under the cursor, or add a marginal note
    // if the click landed on blank page — replaces the old "This page" dropdown, which
    // listed the same actions in a fixed menu instead of at the annotation itself.
    {
        let click = gtk4::GestureClick::new();
        click.set_button(gdk::BUTTON_SECONDARY);
        let state = state.clone();
        let widgets = widgets.clone();
        let reader = reader.clone();
        let render = render.clone();
        let pdf_hash = pdf_hash.to_string();
        let picture_for_menu = picture.clone();
        let undo_button = undo_button.clone();
        let redo_button = redo_button.clone();
        let rebuild_notes = rebuild_notes.clone();
        click.connect_pressed(move |_gesture, _n, x, y| {
            let (page, render_w, render_h, page_w_pts, page_h_pts) = {
                let r = reader.borrow();
                (
                    r.page,
                    r.render_px.0,
                    r.render_px.1,
                    r.page_pts.0,
                    r.page_pts.1,
                )
            };
            let refresh: Rc<dyn Fn()> = {
                let reader = reader.clone();
                let render = render.clone();
                Rc::new(move || {
                    render();
                    let page = reader.borrow().page;
                    render_continuous_page(&reader, page);
                })
            };
            show_pdf_context_menu(
                &state,
                &widgets,
                &reader,
                &pdf_hash,
                &picture_for_menu,
                refresh,
                rebuild_notes.clone(),
                &undo_button,
                &redo_button,
                page,
                ClickGeometry {
                    render_w,
                    render_h,
                    page_w_pts,
                    page_h_pts,
                    click_x: x,
                    click_y: y,
                },
            );
        });
        picture.add_controller(click);
    }

    {
        let reader = reader.clone();
        let render = render.clone();
        let continuous_toggle = continuous_toggle.clone();
        let continuous_scroll = continuous_scroll.clone();
        prev.connect_clicked(move |_| {
            if continuous_toggle.is_active() {
                let target = reader.borrow().page.saturating_sub(1);
                scroll_continuous_to_page(&reader, &continuous_scroll, target);
                return;
            }
            {
                let mut r = reader.borrow_mut();
                if r.page > 0 {
                    r.page -= 1;
                }
            }
            render();
        });
    }
    {
        let reader = reader.clone();
        let render = render.clone();
        let continuous_toggle = continuous_toggle.clone();
        let continuous_scroll = continuous_scroll.clone();
        next.connect_clicked(move |_| {
            if continuous_toggle.is_active() {
                let target = {
                    let r = reader.borrow();
                    (r.page + 1).min(r.count.saturating_sub(1))
                };
                scroll_continuous_to_page(&reader, &continuous_scroll, target);
                return;
            }
            {
                let mut r = reader.borrow_mut();
                if r.page + 1 < r.count {
                    r.page += 1;
                }
            }
            render();
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let reader = reader.clone();
        let render = render.clone();
        let pdf_hash = pdf_hash.to_string();
        let continuous_box = continuous_box.clone();
        let continuous_toggle = continuous_toggle.clone();
        let continuous_scroll = continuous_scroll.clone();
        let undo_button = undo_button.clone();
        let redo_button = redo_button.clone();
        let rebuild_notes = rebuild_notes.clone();
        zoom_in.connect_clicked(move |_| {
            {
                let mut r = reader.borrow_mut();
                r.zoom = (r.zoom * 1.25).min(4.0);
            }
            render();
            rebuild_continuous_view_for_zoom(
                &state,
                &widgets,
                &reader,
                &pdf_hash,
                &continuous_box,
                &undo_button,
                &redo_button,
                &rebuild_notes,
            );
            if continuous_toggle.is_active() {
                let page = reader.borrow().page;
                scroll_continuous_to_page(&reader, &continuous_scroll, page);
            }
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let reader = reader.clone();
        let render = render.clone();
        let pdf_hash = pdf_hash.to_string();
        let continuous_box = continuous_box.clone();
        let continuous_toggle = continuous_toggle.clone();
        let continuous_scroll = continuous_scroll.clone();
        let undo_button = undo_button.clone();
        let redo_button = redo_button.clone();
        let rebuild_notes = rebuild_notes.clone();
        zoom_out.connect_clicked(move |_| {
            {
                let mut r = reader.borrow_mut();
                r.zoom = (r.zoom / 1.25).max(0.35);
            }
            render();
            rebuild_continuous_view_for_zoom(
                &state,
                &widgets,
                &reader,
                &pdf_hash,
                &continuous_box,
                &undo_button,
                &redo_button,
                &rebuild_notes,
            );
            if continuous_toggle.is_active() {
                let page = reader.borrow().page;
                scroll_continuous_to_page(&reader, &continuous_scroll, page);
            }
        });
    }
    // Tracks which page is "current" from scroll position alone — connected once, works
    // regardless of whether continuous mode has been built yet (an empty `continuous_offsets`
    // just makes `continuous_page_at` a no-op returning 0). Keeps `r.page`/the page label/
    // prev-next sensitivity live while scrolling, the same things the paged view's `render()`
    // updates on navigation, so "This page"/Progress/Contents-jump-target stay correct no
    // matter which view is currently visible.
    {
        let reader = reader.clone();
        let page_entry = page_entry.clone();
        let page_of_label = page_of_label.clone();
        let prev = prev.clone();
        let next = next.clone();
        continuous_scroll
            .vadjustment()
            .connect_value_changed(move |adj| {
                let mut r = reader.borrow_mut();
                if r.continuous_offsets.len() < 2 {
                    return;
                }
                let page = continuous_page_at(&r.continuous_offsets, adj.value());
                r.page = page;
                update_page_display(
                    &page_entry,
                    &page_of_label,
                    &prev,
                    &next,
                    page,
                    r.count,
                    &r.page_labels,
                );
            });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let reader = reader.clone();
        let render = render.clone();
        let pdf_hash = pdf_hash.to_string();
        let continuous_box = continuous_box.clone();
        let continuous_scroll = continuous_scroll.clone();
        let view_stack = view_stack.clone();
        let undo_button = undo_button.clone();
        let redo_button = redo_button.clone();
        let rebuild_notes = rebuild_notes.clone();
        continuous_toggle.connect_toggled(move |btn| {
            if btn.is_active() {
                build_continuous_view(
                    &state,
                    &widgets,
                    &reader,
                    &pdf_hash,
                    &continuous_box,
                    &undo_button,
                    &redo_button,
                    &rebuild_notes,
                );
                view_stack.set_visible_child_name("continuous");
                let page = reader.borrow().page;
                scroll_continuous_to_page(&reader, &continuous_scroll, page);
            } else {
                view_stack.set_visible_child_name("paged");
                render();
            }
        });
        // Continuous scrolling is the default reading mode; `set_active` fires the handler
        // above, which builds the continuous view and switches the stack to it.
        continuous_toggle.set_active(true);
    }
    // Typing a page number (the document's own printed label, or a raw file page number —
    // see `find_page_by_label`) and pressing Enter jumps there, the way Zotero's reader lets
    // you navigate by the printed page number rather than always the raw file position.
    {
        let reader = reader.clone();
        let render = render.clone();
        let widgets = widgets.clone();
        let page_entry = page_entry.clone();
        let page_of_label = page_of_label.clone();
        let prev = prev.clone();
        let next = next.clone();
        let continuous_toggle = continuous_toggle.clone();
        let continuous_scroll = continuous_scroll.clone();
        page_entry.clone().connect_activate(move |entry| {
            let text = entry.text();
            let target = {
                let r = reader.borrow();
                find_page_by_label(&r.page_labels, &text)
            };
            match target {
                Some(page) if page < reader.borrow().count => {
                    if continuous_toggle.is_active() {
                        scroll_continuous_to_page(&reader, &continuous_scroll, page);
                    } else {
                        reader.borrow_mut().page = page;
                        render();
                    }
                }
                _ => {
                    toast(&widgets, "No such page");
                    // Revert to the current page's actual label/number rather than leaving
                    // the entry showing whatever unresolvable text was typed.
                    let (page, count) = {
                        let r = reader.borrow();
                        (r.page, r.count)
                    };
                    let labels = reader.borrow().page_labels.clone();
                    update_page_display(
                        &page_entry,
                        &page_of_label,
                        &prev,
                        &next,
                        page,
                        count,
                        &labels,
                    );
                }
            }
        });
    }
    {
        let reader = reader.clone();
        let hint = hint.clone();
        let picture = picture.clone();
        mode_drop.connect_selected_notify(move |drop| {
            let idx = drop.selected() as usize;
            let Some((_, kind)) = DRAW_KIND_OPTIONS.get(idx) else {
                return;
            };
            reader.borrow_mut().draw_kind = *kind;
            let cursor = cursor_for_select_mode(kind.is_none());
            picture.set_cursor(cursor.as_ref());
            for p in &reader.borrow().continuous_pictures {
                p.set_cursor(cursor.as_ref());
            }
            let text = match kind {
                None => "Drag over text to copy it",
                Some(fond_bib::AnnotationKind::Highlight) => "Drag over text to highlight it",
                Some(fond_bib::AnnotationKind::Underline) => "Drag over text to underline it",
                Some(fond_bib::AnnotationKind::Strikeout) => "Drag over text to strike it out",
                Some(fond_bib::AnnotationKind::Note) => "Drag over the page",
            };
            hint.set_text(text);
        });
    }
    {
        let reader = reader.clone();
        color_drop.connect_selected_notify(move |drop| {
            let idx = drop.selected() as usize;
            if let Some((_, hex)) = COLOR_PRESETS.get(idx) {
                reader.borrow_mut().draw_color = hex.to_string();
            }
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let reader = reader.clone();
        let pdf_hash = pdf_hash.to_string();
        let undo_button = undo_button.clone();
        let redo_button = redo_button.clone();
        let rebuild_notes = rebuild_notes.clone();
        note_button.connect_clicked(move |_| {
            show_pdf_note_dialog(
                &state,
                &widgets,
                &reader,
                &pdf_hash,
                &undo_button,
                &redo_button,
                rebuild_notes.clone(),
            );
        });
    }
    // Search: run on Enter (not per-keystroke — PDFium re-searches every page each time, not
    // worth doing on every character typed), jumping straight to the first match's page.
    // Prev/Next cycle `search_current` with wraparound; the count label and match highlight
    // (blended in `render()`, a distinct colour from saved highlights) follow along.
    // Jump to `page` after a search match changes: in continuous mode, scroll there and
    // re-render both it and the previous match's page (to clear that page's now-stale match
    // tint — cheap no-op if continuous mode was never built); in paged mode, the shared
    // `render()` already re-blends the match into whichever page it lands on.
    let goto_search_match = {
        let reader = reader.clone();
        let render = render.clone();
        let continuous_toggle = continuous_toggle.clone();
        let continuous_scroll = continuous_scroll.clone();
        Rc::new(move |page: u16, previous_page: Option<u16>| {
            if continuous_toggle.is_active() {
                scroll_continuous_to_page(&reader, &continuous_scroll, page);
                render_continuous_page(&reader, page);
                if let Some(prev_page) = previous_page {
                    if prev_page != page {
                        render_continuous_page(&reader, prev_page);
                    }
                }
            } else {
                render();
            }
        })
    };
    let run_search = {
        let reader = reader.clone();
        let goto_search_match = goto_search_match.clone();
        let search_prev = search_prev.clone();
        let search_next = search_next.clone();
        let search_count = search_count.clone();
        Rc::new(move |query: &str| {
            let matches = {
                let r = reader.borrow();
                fond_doc::search_document(&r.pdfium, &r.bytes, query).unwrap_or_default()
            };
            let count = matches.len();
            let first_page = matches.first().map(|m| m.page);
            let previous_page = {
                let r = reader.borrow();
                r.search_matches.get(r.search_current).map(|m| m.page)
            };
            {
                let mut r = reader.borrow_mut();
                r.search_matches = matches;
                r.search_current = 0;
                if let Some(page) = first_page {
                    r.page = page;
                }
            }
            search_prev.set_sensitive(count > 0);
            search_next.set_sensitive(count > 0);
            search_count.set_text(&match (count, query.trim().is_empty()) {
                (0, true) => String::new(),
                (0, false) => "No matches".to_string(),
                (n, _) => format!("1 of {n}"),
            });
            if let Some(page) = first_page {
                goto_search_match(page, previous_page);
            }
        })
    };
    {
        let run_search = run_search.clone();
        search_entry.connect_activate(move |entry| run_search(&entry.text()));
    }
    {
        // Clear stale results (and the match highlight) as soon as the box is emptied,
        // rather than leaving them stuck until another search is run.
        let reader = reader.clone();
        let render = render.clone();
        let search_prev = search_prev.clone();
        let search_next = search_next.clone();
        let search_count = search_count.clone();
        search_entry.connect_search_changed(move |entry| {
            if entry.text().is_empty() {
                let cleared_page = {
                    let mut r = reader.borrow_mut();
                    let page = r.search_matches.get(r.search_current).map(|m| m.page);
                    r.search_matches.clear();
                    page
                };
                search_prev.set_sensitive(false);
                search_next.set_sensitive(false);
                search_count.set_text("");
                render();
                if let Some(page) = cleared_page {
                    render_continuous_page(&reader, page);
                }
            }
        });
    }
    {
        let reader = reader.clone();
        let goto_search_match = goto_search_match.clone();
        let search_count = search_count.clone();
        search_prev.connect_clicked(move |_| {
            let (previous_page, page) = {
                let mut r = reader.borrow_mut();
                if r.search_matches.is_empty() {
                    return;
                }
                let previous_page = r.search_matches[r.search_current].page;
                r.search_current = if r.search_current == 0 {
                    r.search_matches.len() - 1
                } else {
                    r.search_current - 1
                };
                let page = r.search_matches[r.search_current].page;
                r.page = page;
                search_count.set_text(&format!(
                    "{} of {}",
                    r.search_current + 1,
                    r.search_matches.len()
                ));
                (previous_page, page)
            };
            goto_search_match(page, Some(previous_page));
        });
    }
    {
        let reader = reader.clone();
        let goto_search_match = goto_search_match.clone();
        let search_count = search_count.clone();
        search_next.connect_clicked(move |_| {
            let (previous_page, page) = {
                let mut r = reader.borrow_mut();
                if r.search_matches.is_empty() {
                    return;
                }
                let previous_page = r.search_matches[r.search_current].page;
                r.search_current = (r.search_current + 1) % r.search_matches.len();
                let page = r.search_matches[r.search_current].page;
                r.page = page;
                search_count.set_text(&format!(
                    "{} of {}",
                    r.search_current + 1,
                    r.search_matches.len()
                ));
                (previous_page, page)
            };
            goto_search_match(page, Some(previous_page));
        });
    }

    // Save the current page back to the entry's Progress on close, so the next "Read" opens
    // where this session left off — the PDF-reader half of Tier 2a. `page`/`count` snapshot
    // out of `reader` up front since the RefCell isn't needed once we're just writing to the
    // library.
    {
        let state = state.clone();
        let key = key.to_string();
        let reader = reader.clone();
        dialog.connect_close_request(move |_| {
            let (page, count) = {
                let r = reader.borrow();
                (r.page as u32 + 1, r.count as u32)
            };
            let s = state.borrow();
            if let Some(library) = s.library.as_ref() {
                if let Ok(Some(mut note)) = library.load_note(&key) {
                    note.frontmatter.progress = Some(fond_bib::Progress { page, of: count });
                    let _ = library.write_note(&key, &note);
                }
            }
            glib::Propagation::Proceed
        });
    }

    dialog.present();
}

/// A small modal for adding a freestanding marginal note (`AnnotationKind::Note`) to the
/// PDF reader's *current* page — unlike Highlight/Underline/Strikeout, a note isn't tied to
/// a drawn region, so there's no drag gesture for it, just this prompt. Saves straight into
/// `reader`'s in-memory sidecar and to disk, the same `annots/<key>.json` the drag gesture
/// writes; doesn't need to trigger a re-render, since a note has no on-page mark to draw.
fn show_pdf_note_dialog(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    reader: &Rc<RefCell<ReaderState>>,
    pdf_hash: &str,
    undo_button: &gtk4::Button,
    redo_button: &gtk4::Button,
    rebuild_notes: Rc<dyn Fn()>,
) {
    let current_page = reader.borrow().page as u32 + 1;

    let dialog = adw::Window::new();
    dialog.set_title(Some(&format!("Note on page {current_page}")));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(420, 260);

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

    let text_view = gtk4::TextView::new();
    text_view.set_wrap_mode(gtk4::WrapMode::Word);
    text_view.set_margin_top(8);
    text_view.set_margin_bottom(8);
    text_view.set_margin_start(8);
    text_view.set_margin_end(8);

    // Pre-fill with the last "Select text" copy, quoted with its page number, if it was made
    // on this same page — consumed either way so a stale selection from another page doesn't
    // linger into some later, unrelated note.
    let selection_for_this_page = {
        let mut r = reader.borrow_mut();
        match r.last_selection.take() {
            Some((sel_page, text)) if sel_page == r.page => Some(text),
            _ => None,
        }
    };
    if let Some(sel_text) = selection_for_this_page {
        let buffer = text_view.buffer();
        buffer.set_text(&format!("p. {current_page}: \"{sel_text}\"\n\n"));
        let end = buffer.end_iter();
        buffer.place_cursor(&end);
    }

    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    scrolled.set_child(Some(&text_view));
    view.set_content(Some(&scrolled));
    dialog.set_content(Some(&view));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let dialog = dialog.clone();
        let widgets = widgets.clone();
        let state = state.clone();
        let reader = reader.clone();
        let pdf_hash = pdf_hash.to_string();
        let text_view = text_view.clone();
        let undo_button = undo_button.clone();
        let redo_button = redo_button.clone();
        save.connect_clicked(move |_| {
            let buffer = text_view.buffer();
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .trim()
                .to_string();
            if text.is_empty() {
                toast(&widgets, "Note is empty");
                return;
            }

            let annotation = fond_bib::Annotation::drawn(
                fond_bib::AnnotationKind::Note,
                current_page,
                Vec::new(),
                None,
                Some(text),
                None,
            );
            push_undo_snapshot(&reader);
            {
                let mut r = reader.borrow_mut();
                r.annotations.pdf_hash = Some(pdf_hash.clone());
                r.annotations.upsert(annotation);
            }
            let write_result = {
                let s = state.borrow();
                s.library
                    .as_ref()
                    .map(|lib| lib.write_annotations(&reader.borrow().annotations))
            };
            match write_result {
                Some(Ok(_)) => {
                    sync_undo_redo_buttons(&reader, &undo_button, &redo_button);
                    rebuild_notes();
                    toast(&widgets, "Note added");
                    dialog.close();
                }
                Some(Err(e)) => toast(&widgets, &friendly::bib_error(&e)),
                None => toast(&widgets, "No open library — note not saved"),
            }
        });
    }

    dialog.present();
}

/// Live state of an open EPUB reader window: the chapter list, current position, and this
/// entry's annotation sidecar (loaded once at open and rewritten to disk on every highlight
/// added — the same "hold it in memory, don't re-read the library each time" approach the
/// PDF reader's `ReaderState` uses).
struct EpubReaderState {
    /// Where this EPUB's contents were extracted to (content-addressed by attachment hash,
    /// so a repeat "Read" reuses the extraction rather than re-unzipping every time).
    cache_dir: PathBuf,
    /// Chapter files in reading order, as paths relative to `cache_dir` (same values as
    /// `fond_doc::EpubBook::spine`).
    spine: Vec<String>,
    index: usize,
    annotations: fond_bib::AnnotationSidecar,
    /// Snapshot-based undo/redo, same idiom as the PDF reader's `ReaderState` (see
    /// `push_undo_snapshot`/`UNDO_HISTORY_LIMIT`) — a full clone of `annotations` taken
    /// immediately before each mutation (add/edit/delete a mark).
    undo_stack: Vec<fond_bib::AnnotationSidecar>,
    redo_stack: Vec<fond_bib::AnnotationSidecar>,
}

/// Snapshot `reader`'s current annotations onto the undo stack and clear the redo stack —
/// same convention as the PDF reader's `push_undo_snapshot`, just typed to `EpubReaderState`.
fn push_epub_undo_snapshot(reader: &Rc<RefCell<EpubReaderState>>) {
    let mut r = reader.borrow_mut();
    let snapshot = r.annotations.clone();
    r.undo_stack.push(snapshot);
    if r.undo_stack.len() > UNDO_HISTORY_LIMIT {
        r.undo_stack.remove(0);
    }
    r.redo_stack.clear();
}

fn sync_epub_undo_redo_buttons(
    reader: &Rc<RefCell<EpubReaderState>>,
    undo_button: &gtk4::Button,
    redo_button: &gtk4::Button,
) {
    let r = reader.borrow();
    undo_button.set_sensitive(!r.undo_stack.is_empty());
    redo_button.set_sensitive(!r.redo_stack.is_empty());
}

/// One annotation as sent to the reader's highlight-apply JS: just enough to find it in the
/// rendered DOM (`snippet`, plus `prefix`/`suffix` context to disambiguate a snippet that
/// appears more than once in the chapter) and mark it (`id`, for the CSS class and as the
/// optional scroll target).
#[derive(serde::Serialize)]
struct EpubHighlightPayload<'a> {
    id: &'a str,
    snippet: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    prefix: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suffix: Option<&'a str>,
    /// Serializes lowercase (`"highlight"`/`"underline"`/`"strikeout"`) via
    /// `AnnotationKind`'s own `Serialize` impl — `EPUB_APPLY_HIGHLIGHTS_FN` switches on
    /// this to decide which CSS treatment to apply.
    kind: fond_bib::AnnotationKind,
    /// Highlight colour (hex, e.g. `#f6c344`) — meaningless for underline/strikeout,
    /// which always use the current text colour so they read correctly in dark mode too.
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<&'a str>,
}

/// What the reader's selection-capture JS reports back: either nothing was meaningfully
/// selected, or the selected text plus up to 40 characters of surrounding context on each
/// side — the same prefix/suffix disambiguation scheme the PDF sidecar already uses.
#[derive(serde::Deserialize)]
struct EpubSelectionCapture {
    empty: bool,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    prefix: Option<String>,
    #[serde(default)]
    suffix: Option<String>,
}

/// A JS function *expression* (no trailing call — callers append `(args)`) that finds each
/// given annotation's snippet text in the current document and wraps it in a
/// `<mark class="kartoteka-hl">`, clearing any marks left by a previous call first
/// (idempotent re-apply, so adding a highlight can just re-run this instead of reloading the
/// page). Scrolls the mark matching `scrollToId` into view, if given. Mirrors
/// `select_text_in_rect`'s multi-node-aware approach in `fond-doc/src/pdf.rs` — walk every
/// text node, find the target substring, map it back onto node+offset pairs — just over DOM
/// text nodes instead of PDF characters, since there's no PDFium text layer here.
const EPUB_APPLY_HIGHLIGHTS_FN: &str = r#"(function(annotations, scrollToId) {
  function findTextRange(root, snippet, prefix, suffix) {
    if (!snippet) return null;
    var walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, null);
    var nodes = [];
    var fullText = '';
    var node;
    while (node = walker.nextNode()) {
      nodes.push({ node: node, start: fullText.length });
      fullText += node.textContent;
    }
    var combined = (prefix || '') + snippet + (suffix || '');
    var idx, endIdx;
    var combinedIdx = combined.length > snippet.length ? fullText.indexOf(combined) : -1;
    if (combinedIdx !== -1) {
      idx = combinedIdx + (prefix || '').length;
      endIdx = idx + snippet.length;
    } else {
      var plainIdx = fullText.indexOf(snippet);
      if (plainIdx === -1) return null;
      idx = plainIdx;
      endIdx = idx + snippet.length;
    }
    var startNode = null, startOffset = 0, endNode = null, endOffset = 0;
    for (var i = 0; i < nodes.length; i++) {
      var n = nodes[i];
      var nEnd = n.start + n.node.textContent.length;
      if (startNode === null && idx >= n.start && idx < nEnd) {
        startNode = n.node; startOffset = idx - n.start;
      }
      if (endIdx > n.start && endIdx <= nEnd) {
        endNode = n.node; endOffset = endIdx - n.start;
      }
    }
    if (!startNode || !endNode) return null;
    var range = document.createRange();
    range.setStart(startNode, startOffset);
    range.setEnd(endNode, endOffset);
    return range;
  }

  document.querySelectorAll('mark.kartoteka-hl').forEach(function(m) {
    var parent = m.parentNode;
    if (!parent) return;
    while (m.firstChild) parent.insertBefore(m.firstChild, m);
    parent.removeChild(m);
    parent.normalize();
  });

  annotations.forEach(function(a) {
    var range = findTextRange(document.body, a.snippet, a.prefix, a.suffix);
    if (!range) return;
    var mark = document.createElement('mark');
    mark.className = 'kartoteka-hl';
    mark.dataset.annotationId = a.id;
    mark.dataset.kind = a.kind;
    // No background/foreground colour is hardcoded beyond the highlight tint itself
    // (which is the whole point of a highlight) — underline/strikeout use `currentColor`
    // so they read correctly against the page's own text colour in light or dark mode.
    if (a.kind === 'underline') {
      mark.style.background = 'transparent';
      mark.style.textDecoration = 'underline';
      mark.style.textDecorationColor = a.color || 'currentColor';
      mark.style.textDecorationThickness = '2px';
    } else if (a.kind === 'strikeout') {
      mark.style.background = 'transparent';
      mark.style.textDecoration = 'line-through';
      mark.style.textDecorationColor = a.color || 'currentColor';
    } else {
      mark.style.backgroundColor = a.color ? (a.color + '59') : 'rgba(246, 195, 68, 0.35)';
    }
    try {
      range.surroundContents(mark);
    } catch (e) {
      var contents = range.extractContents();
      mark.appendChild(contents);
      range.insertNode(mark);
    }
    if (scrollToId && a.id === scrollToId) {
      mark.scrollIntoView({ block: 'center' });
    }
  });
})"#;

/// JS that reports the `WebView`'s current text selection (if any) as JSON: `{empty: true}`
/// if nothing is meaningfully selected, else `{empty: false, text, prefix, suffix}` — the
/// selected text plus up to 40 characters of context on each side, computed by walking
/// `document.body`'s text nodes to find the selection's absolute character offset (the same
/// approach `EPUB_APPLY_HIGHLIGHTS_FN` uses in reverse, to re-locate a snippet later).
const EPUB_CAPTURE_SELECTION_JS: &str = r#"(function() {
  var sel = window.getSelection();
  if (!sel || sel.rangeCount === 0 || !sel.toString().trim()) {
    return JSON.stringify({ empty: true });
  }
  var text = sel.toString();
  var range = sel.getRangeAt(0);
  function textOffsetOf(node, offset) {
    var walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, null);
    var total = 0, n;
    while (n = walker.nextNode()) {
      if (n === node) return total + offset;
      total += n.textContent.length;
    }
    return total;
  }
  var fullText = document.body.textContent;
  var startIdx = textOffsetOf(range.startContainer, range.startOffset);
  var prefix = fullText.slice(Math.max(0, startIdx - 40), startIdx);
  var suffix = fullText.slice(startIdx + text.length, startIdx + text.length + 40);
  sel.removeAllRanges();
  return JSON.stringify({ empty: false, text: text, prefix: prefix, suffix: suffix });
})()"#;

/// Serialize the annotations anchored to `chapter` into the JSON array
/// `EPUB_APPLY_HIGHLIGHTS_FN` expects. An annotation with no `snippet` (shouldn't happen for
/// an EPUB one — `drawn_epub` always sets it — but the field is `Option` since the type is
/// shared with PDF annotations) is skipped rather than sent as an unfindable empty search.
fn epub_highlight_payload_json(sidecar: &fond_bib::AnnotationSidecar, chapter: &str) -> String {
    let items: Vec<EpubHighlightPayload> = sidecar
        .annotations
        .iter()
        .filter(|a| a.chapter.as_deref() == Some(chapter))
        .filter_map(|a| {
            a.snippet.as_deref().map(|s| EpubHighlightPayload {
                id: &a.id,
                snippet: s,
                prefix: a.snippet_prefix.as_deref(),
                suffix: a.snippet_suffix.as_deref(),
                kind: a.kind,
                color: a.color.as_deref(),
            })
        })
        .collect();
    serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string())
}

/// Run `EPUB_APPLY_HIGHLIGHTS_FN` against the currently-loaded chapter, scrolling
/// `scroll_to_id`'s mark into view if given. Called after every chapter load (so navigating
/// away and back keeps showing highlights) and right after adding a new highlight (so it
/// appears immediately, no reload needed).
fn epub_apply_highlights(
    view: &webkit6::WebView,
    state: &Rc<RefCell<EpubReaderState>>,
    scroll_to_id: Option<&str>,
) {
    let payload_json = {
        let r = state.borrow();
        match r.spine.get(r.index) {
            Some(chapter) => epub_highlight_payload_json(&r.annotations, chapter),
            None => return,
        }
    };
    let scroll_json = match scroll_to_id {
        Some(id) => serde_json::to_string(id).unwrap_or_else(|_| "null".to_string()),
        None => "null".to_string(),
    };
    let script = format!("{EPUB_APPLY_HIGHLIGHTS_FN}({payload_json}, {scroll_json})");
    view.evaluate_javascript(&script, None, None, gio::Cancellable::NONE, |_| {});
}

/// A built-in EPUB reader: renders each chapter with WebKitGTK (`webkit6`), which — unlike
/// PDFium for the PDF reader — handles an XHTML+CSS chapter's layout, images, and text
/// selection natively, so this only has to handle chapter/TOC navigation plus highlighting.
/// `hash` is the attachment's content hash (`blake3:…`), used to pick a stable extraction
/// cache directory — the EPUB zip is extracted to disk once per unique file (not re-unzipped
/// on every "Read") so chapter-relative links to sibling images/CSS resolve the normal
/// browser way over `file://`, rather than needing a custom URI scheme handler.
///
/// Highlighting (M5-SPEC.md 5C) reuses the `WebView`'s own native text selection — the user
/// drags to select the ordinary browser way, then "Highlight" captures it via
/// `EPUB_CAPTURE_SELECTION_JS` and saves a `fond_bib::Annotation::drawn_epub` (chapter +
/// snippet + context; no page/quadpoints — there's no fixed page grid to hang those on) into
/// the same `annots/<key>.json` sidecar the PDF reader writes. Applying saved highlights back
/// onto the page is a live DOM search-and-wrap (`epub_apply_highlights`) run after every
/// chapter load, not a raster blend like the PDF reader's `blend_highlights` — there's no
/// bitmap to blend into here, WebKit owns the actual rendering.
///
/// `start_annotation_id`, if given, opens on that annotation's chapter and scrolls it into
/// view once highlights are applied — the EPUB equivalent of the PDF reader's `start_page`.
fn show_epub_reader(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    key: &str,
    hash: &str,
    blob: &std::path::Path,
    title: &str,
    start_annotation_id: Option<&str>,
) {
    let window = &widgets.window;

    let book = match fond_doc::open_book(blob) {
        Ok(b) => b,
        Err(e) => {
            gtk4::AlertDialog::builder()
                .message("Could not open EPUB")
                .detail(e.to_string())
                .build()
                .show(Some(window));
            return;
        }
    };

    let hex = hash.split_once(':').map(|(_, h)| h).unwrap_or(hash);
    let cache_dir = glib::user_cache_dir()
        .join("kartoteka")
        .join("epub")
        .join(hex);
    if !cache_dir.exists() {
        let extracted = std::fs::create_dir_all(&cache_dir)
            .map_err(|e| e.to_string())
            .and_then(|_| fond_doc::extract_epub(blob, &cache_dir).map_err(|e| e.to_string()));
        if let Err(e) = extracted {
            gtk4::AlertDialog::builder()
                .message("Could not open EPUB")
                .detail(e)
                .build()
                .show(Some(window));
            return;
        }
    }

    let annotations = state
        .borrow()
        .library
        .as_ref()
        .and_then(|lib| lib.load_annotations(key).ok().flatten())
        .unwrap_or_else(|| fond_bib::AnnotationSidecar::new(key));

    let start_index = start_annotation_id
        .and_then(|id| annotations.annotations.iter().find(|a| a.id == id))
        .and_then(|a| a.chapter.as_deref())
        .and_then(|chapter| book.spine.iter().position(|p| p == chapter))
        .unwrap_or(0);

    let reader = Rc::new(RefCell::new(EpubReaderState {
        cache_dir,
        spine: book.spine,
        index: start_index,
        annotations,
        undo_stack: Vec::new(),
        redo_stack: Vec::new(),
    }));

    let dialog = adw::Window::new();
    dialog.set_title(Some(title));
    dialog.set_transient_for(Some(window));
    dialog.set_default_size(1000, 820);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");

    let prev = gtk4::Button::from_icon_name("go-previous-symbolic");
    prev.set_tooltip_text(Some("Previous chapter"));
    let next = gtk4::Button::from_icon_name("go-next-symbolic");
    next.set_tooltip_text(Some("Next chapter"));
    let chapter_label = gtk4::Label::new(None);
    chapter_label.add_css_class("dim-label");
    let nav = gtk4::Box::new(Orientation::Horizontal, 6);
    nav.append(&prev);
    nav.append(&chapter_label);
    nav.append(&next);
    header.set_title_widget(Some(&nav));

    let web_view = webkit6::WebView::new();
    web_view.set_vexpand(true);
    web_view.set_hexpand(true);
    // Scale text size only, not the page layout/images — "zoom" on a WebView otherwise
    // scales everything, which reads as zooming a picture rather than adjusting font size.
    if let Some(settings) = webkit6::prelude::WebViewExt::settings(&web_view) {
        settings.set_zoom_text_only(true);
    }

    let hint = gtk4::Label::new(Some("Select text, choose a kind, then click Apply"));
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    hint.set_margin_top(4);
    hint.set_margin_bottom(4);

    // In-chapter search: WebKit's own `FindController` — highlights and cycles matches in
    // the currently loaded chapter, same as Ctrl+F in a browser. Chapter-scoped rather than
    // whole-book (per EPUB-READER-PLAN.md's recommendation: cheap, matches "one chapter
    // loaded at a time" reality; whole-book would need pre-extracting every chapter's text).
    let search_toggle = gtk4::ToggleButton::new();
    search_toggle.set_icon_name("edit-find-symbolic");
    search_toggle.set_tooltip_text(Some("Search this chapter (Ctrl+F)"));
    let search_bar_entry = gtk4::SearchEntry::new();
    search_bar_entry.set_placeholder_text(Some("Search this chapter"));
    search_bar_entry.set_hexpand(true);
    let search_prev = gtk4::Button::from_icon_name("go-up-symbolic");
    search_prev.set_tooltip_text(Some("Previous match"));
    let search_next = gtk4::Button::from_icon_name("go-down-symbolic");
    search_next.set_tooltip_text(Some("Next match"));
    let search_count = gtk4::Label::new(None);
    search_count.add_css_class("dim-label");
    search_count.add_css_class("caption");
    let search_row = gtk4::Box::new(Orientation::Horizontal, 6);
    search_row.set_margin_top(6);
    search_row.set_margin_bottom(6);
    search_row.set_margin_start(8);
    search_row.set_margin_end(8);
    search_row.append(&search_bar_entry);
    search_row.append(&search_count);
    search_row.append(&search_prev);
    search_row.append(&search_next);
    let search_revealer = gtk4::Revealer::new();
    search_revealer.set_reveal_child(false);
    search_revealer.set_child(Some(&search_row));

    let content = gtk4::Box::new(Orientation::Vertical, 0);
    content.append(&hint);
    content.append(&search_revealer);
    content.append(&web_view);

    // Sidebar toggles (Contents, if the EPUB has a TOC; Notes always) — persistent Paned
    // sidebar, not popovers, matching the PDF reader's own house sidebar style (see
    // `show_pdf_reader`'s `sidebar_toggle`/`notes_toggle` pair, and CLAUDE.md's UI
    // standard). `Apply`/mode/colour stay at the end of the header, same relative position
    // "Highlight" used to occupy.
    let sidebar_toggle = (!book.toc.is_empty()).then(|| {
        let button = gtk4::ToggleButton::new();
        button.set_icon_name("sidebar-show-symbolic");
        button.set_tooltip_text(Some("Show the table of contents"));
        button
    });
    let notes_toggle = gtk4::ToggleButton::new();
    notes_toggle.set_icon_name("view-list-symbolic");
    notes_toggle.set_tooltip_text(Some("Show notes and highlights"));

    // Undo/redo: same snapshot-based idiom as the PDF reader (see `EpubReaderState`'s
    // `undo_stack`/`redo_stack` and `push_epub_undo_snapshot`).
    let undo_button = gtk4::Button::from_icon_name("edit-undo-symbolic");
    undo_button.set_tooltip_text(Some("Undo (Ctrl+Z)"));
    undo_button.set_sensitive(false);
    let redo_button = gtk4::Button::from_icon_name("edit-redo-symbolic");
    redo_button.set_tooltip_text(Some("Redo (Ctrl+Shift+Z)"));
    redo_button.set_sensitive(false);

    let mode_labels: Vec<&str> = EPUB_MARK_KIND_OPTIONS.iter().map(|(l, _)| *l).collect();
    let mode_drop = gtk4::DropDown::from_strings(&mode_labels);
    mode_drop.set_tooltip_text(Some("What kind of mark to apply to the selection"));
    let color_labels: Vec<&str> = COLOR_PRESETS.iter().map(|(l, _)| *l).collect();
    let color_drop = gtk4::DropDown::from_strings(&color_labels);
    color_drop.set_tooltip_text(Some("Highlight colour"));
    let apply_button = gtk4::Button::with_label("Apply");
    apply_button.set_tooltip_text(Some("Mark the selected text"));

    // Font size: text-only zoom (see `set_zoom_text_only` above), stepped like the PDF
    // reader's own zoom buttons. Not persisted across sessions (unlike window/pane sizing)
    // — a book-length reading choice you're more likely to want to readjust per-book than
    // to lock in globally.
    let font_zoom: Rc<Cell<f64>> = Rc::new(Cell::new(1.0));
    let zoom_out_button = gtk4::Button::from_icon_name("zoom-out-symbolic");
    zoom_out_button.add_css_class("flat");
    zoom_out_button.set_tooltip_text(Some("Smaller text"));
    let zoom_in_button = gtk4::Button::from_icon_name("zoom-in-symbolic");
    zoom_in_button.add_css_class("flat");
    zoom_in_button.set_tooltip_text(Some("Larger text"));
    {
        let web_view = web_view.clone();
        let font_zoom = font_zoom.clone();
        zoom_out_button.connect_clicked(move |_| {
            let z = (font_zoom.get() - 0.1).max(0.5);
            font_zoom.set(z);
            web_view.set_zoom_level(z);
        });
    }
    {
        let web_view = web_view.clone();
        let font_zoom = font_zoom.clone();
        zoom_in_button.connect_clicked(move |_| {
            let z = (font_zoom.get() + 0.1).min(3.0);
            font_zoom.set(z);
            web_view.set_zoom_level(z);
        });
    }

    // pack_end order is the reverse of visual order (same gotcha CLAUDE.md notes for the
    // hamburger menu) — Apply packed first so it ends up rightmost: Mode, Colour, Apply,
    // Font size. Sidebar toggles, search, and undo/redo go at the header's start, per house
    // style (undo/redo in the same relative position as the PDF reader's own pair).
    header.pack_end(&apply_button);
    header.pack_end(&color_drop);
    header.pack_end(&mode_drop);
    header.pack_end(&zoom_in_button);
    header.pack_end(&zoom_out_button);
    if let Some(sidebar_toggle) = &sidebar_toggle {
        header.pack_start(sidebar_toggle);
    }
    header.pack_start(&notes_toggle);
    header.pack_start(&search_toggle);
    header.pack_start(&undo_button);
    header.pack_start(&redo_button);
    view.add_top_bar(&header);

    // Contents sidebar (only built if the EPUB has a TOC).
    let contents_scroll = sidebar_toggle.as_ref().map(|_| {
        let rows = gtk4::Box::new(Orientation::Vertical, 2);
        rows.set_margin_top(6);
        rows.set_margin_bottom(6);
        rows.set_margin_start(6);
        rows.set_margin_end(6);
        let last = book.toc.len().saturating_sub(1);
        for (i, entry) in book.toc.iter().enumerate() {
            let row = popover_button(&entry.label, false);
            if let Some(lbl) = row.child().and_then(|w| w.downcast::<gtk4::Label>().ok()) {
                lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            }
            {
                let reader = reader.clone();
                let view = web_view.clone();
                let prev = prev.clone();
                let next = next.clone();
                let chapter_label = chapter_label.clone();
                let target = entry.target.clone();
                row.connect_clicked(move |_| {
                    epub_go_to(&reader, &view, &prev, &next, &chapter_label, &target);
                });
            }
            rows.append(&row);
            if i != last {
                rows.append(&popover_separator());
            }
        }
        let scroll = gtk4::ScrolledWindow::new();
        scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        scroll.set_child(Some(&rows));
        scroll
    });

    // Notes/highlights sidebar: every annotation on this EPUB, in reading order, readable
    // prose rather than just an in-text mark — same pattern as the PDF reader's own notes
    // sidebar (`show_pdf_reader`), rebuilt via the same self-referential-cell idiom so a
    // row's own delete button can trigger a fresh rebuild of the list it lives in.
    let notes_rows = gtk4::Box::new(Orientation::Vertical, 2);
    notes_rows.set_margin_top(6);
    notes_rows.set_margin_bottom(6);
    notes_rows.set_margin_start(6);
    notes_rows.set_margin_end(6);
    let notes_scroll = gtk4::ScrolledWindow::new();
    notes_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    notes_scroll.set_child(Some(&notes_rows));

    // Pending scroll target for the *next* chapter load — set right before calling
    // `epub_go_to` by anything that wants the freshly-loaded chapter to scroll to a
    // specific annotation (the initial `start_annotation_id`, or a notes-sidebar jump);
    // left `None` for plain prev/next/TOC navigation, which just lands at the top.
    let pending_scroll: Rc<RefCell<Option<String>>> =
        Rc::new(RefCell::new(start_annotation_id.map(|s| s.to_string())));

    let rebuild_notes_cell: RebuildCell = Rc::new(RefCell::new(None));
    {
        let notes_rows = notes_rows.clone();
        let state = state.clone();
        let widgets = widgets.clone();
        let reader = reader.clone();
        let view = web_view.clone();
        let prev = prev.clone();
        let next = next.clone();
        let chapter_label = chapter_label.clone();
        let pending_scroll = pending_scroll.clone();
        let rebuild_notes_cell_inner = rebuild_notes_cell.clone();
        let builder = move || {
            while let Some(child) = notes_rows.first_child() {
                notes_rows.remove(&child);
            }
            let mut all: Vec<fond_bib::Annotation> = reader
                .borrow()
                .annotations
                .annotations
                .iter()
                .filter(|a| a.chapter.is_some())
                .cloned()
                .collect();
            all.sort_by_key(|a| {
                let r = reader.borrow();
                let spine_pos = a
                    .chapter
                    .as_deref()
                    .and_then(|c| r.spine.iter().position(|p| p == c))
                    .unwrap_or(usize::MAX);
                (spine_pos, a.created.clone())
            });
            if all.is_empty() {
                let label = gtk4::Label::new(Some("No notes or highlights yet"));
                label.add_css_class("dim-label");
                label.set_margin_top(6);
                label.set_margin_bottom(6);
                notes_rows.append(&label);
                return;
            }
            let last = all.len().saturating_sub(1);
            for (i, annotation) in all.into_iter().enumerate() {
                let Some(chapter) = annotation.chapter.clone() else {
                    continue;
                };
                let chapter_num = reader
                    .borrow()
                    .spine
                    .iter()
                    .position(|p| p == &chapter)
                    .map(|i| i + 1)
                    .unwrap_or(0);
                let kind_label = match annotation.kind {
                    fond_bib::AnnotationKind::Highlight => "Highlight",
                    fond_bib::AnnotationKind::Underline => "Underline",
                    fond_bib::AnnotationKind::Strikeout => "Strikeout",
                    fond_bib::AnnotationKind::Note => "Note",
                };
                let outer = gtk4::Box::new(Orientation::Vertical, 2);

                let header_box = gtk4::Box::new(Orientation::Horizontal, 6);
                let header_label =
                    gtk4::Label::new(Some(&format!("Ch. {chapter_num} — {kind_label}")));
                header_label.set_xalign(0.0);
                header_label.set_hexpand(true);
                header_label.add_css_class("dim-label");
                header_label.add_css_class("caption-heading");
                header_box.append(&header_label);
                let delete_button = gtk4::Button::from_icon_name("user-trash-symbolic");
                delete_button.add_css_class("flat");
                delete_button.set_tooltip_text(Some("Delete this annotation"));
                header_box.append(&delete_button);
                outer.append(&header_box);

                {
                    let jump = gtk4::GestureClick::new();
                    let reader = reader.clone();
                    let view = view.clone();
                    let prev = prev.clone();
                    let next = next.clone();
                    let chapter_label = chapter_label.clone();
                    let pending_scroll = pending_scroll.clone();
                    let id = annotation.id.clone();
                    let chapter = chapter.clone();
                    jump.connect_released(move |_gesture, _n, _x, _y| {
                        *pending_scroll.borrow_mut() = Some(id.clone());
                        epub_go_to(&reader, &view, &prev, &next, &chapter_label, &chapter);
                    });
                    header_label.add_controller(jump);
                }

                if let Some(snippet) = &annotation.snippet {
                    let snippet_label = gtk4::Label::new(Some(snippet));
                    snippet_label.set_xalign(0.0);
                    snippet_label.set_wrap(true);
                    snippet_label.add_css_class("dim-label");
                    snippet_label.add_css_class("caption");
                    outer.append(&snippet_label);
                }

                let note_entry = gtk4::Entry::new();
                note_entry.set_placeholder_text(Some("No note"));
                if let Some(note) = &annotation.note {
                    note_entry.set_text(note);
                }
                outer.append(&note_entry);

                let save_note = {
                    let state = state.clone();
                    let widgets = widgets.clone();
                    let reader = reader.clone();
                    let id = annotation.id.clone();
                    move |text: &str| {
                        let text = text.trim();
                        let current_note = reader
                            .borrow()
                            .annotations
                            .annotations
                            .iter()
                            .find(|a| a.id == id)
                            .and_then(|a| a.note.clone());
                        if current_note.as_deref().unwrap_or("") == text {
                            return;
                        }
                        push_epub_undo_snapshot(&reader);
                        {
                            let mut r = reader.borrow_mut();
                            if let Some(a) =
                                r.annotations.annotations.iter_mut().find(|a| a.id == id)
                            {
                                a.note = (!text.is_empty()).then(|| text.to_string());
                            }
                        }
                        let write_result = {
                            let s = state.borrow();
                            s.library
                                .as_ref()
                                .map(|lib| lib.write_annotations(&reader.borrow().annotations))
                        };
                        if let Some(Err(e)) = write_result {
                            toast(&widgets, &friendly::bib_error(&e));
                        }
                    }
                };
                {
                    let save_note = save_note.clone();
                    note_entry.connect_activate(move |e| save_note(&e.text()));
                }
                {
                    let focus = gtk4::EventControllerFocus::new();
                    let save_note = save_note.clone();
                    let note_entry_weak = note_entry.downgrade();
                    focus.connect_leave(move |_| {
                        if let Some(e) = note_entry_weak.upgrade() {
                            save_note(&e.text());
                        }
                    });
                    note_entry.add_controller(focus);
                }

                {
                    let state = state.clone();
                    let widgets = widgets.clone();
                    let reader = reader.clone();
                    let view = view.clone();
                    let id = annotation.id.clone();
                    let rebuild_notes_cell = rebuild_notes_cell_inner.clone();
                    delete_button.connect_clicked(move |_| {
                        push_epub_undo_snapshot(&reader);
                        reader
                            .borrow_mut()
                            .annotations
                            .annotations
                            .retain(|a| a.id != id);
                        let write_result = {
                            let s = state.borrow();
                            s.library
                                .as_ref()
                                .map(|lib| lib.write_annotations(&reader.borrow().annotations))
                        };
                        match write_result {
                            Some(Ok(_)) => {
                                epub_apply_highlights(&view, &reader, None);
                                toast(&widgets, "Annotation deleted");
                                if let Some(f) = rebuild_notes_cell.borrow().as_ref() {
                                    f();
                                }
                            }
                            Some(Err(e)) => toast(&widgets, &friendly::bib_error(&e)),
                            None => toast(&widgets, "No open library"),
                        }
                    });
                }

                notes_rows.append(&outer);
                if i != last {
                    notes_rows.append(&popover_separator());
                }
            }
        };
        *rebuild_notes_cell.borrow_mut() = Some(Rc::new(builder));
    }
    let rebuild_notes: Rc<dyn Fn()> = {
        let cell = rebuild_notes_cell.clone();
        Rc::new(move || {
            let f = cell.borrow().clone();
            if let Some(f) = f {
                f();
            }
        })
    };

    let sidebar_stack = gtk4::Stack::new();
    if let Some(contents_scroll) = &contents_scroll {
        sidebar_stack.add_named(contents_scroll, Some("contents"));
    }
    sidebar_stack.add_named(&notes_scroll, Some("notes"));
    sidebar_stack.set_size_request(60, -1);
    sidebar_stack.set_vexpand(true);

    let paned = gtk4::Paned::new(Orientation::Horizontal);
    paned.set_start_child(gtk4::Widget::NONE);
    paned.set_resize_start_child(false);
    paned.set_shrink_start_child(true);
    paned.set_end_child(Some(&content));
    paned.set_vexpand(true);
    paned.set_hexpand(true);
    paned.set_position(220);
    view.set_content(Some(&paned));
    dialog.set_content(Some(&view));

    // Undo/redo: pop a snapshot and refresh the current chapter's highlights plus the notes
    // sidebar — cheap, since there's no per-page render state to rebuild the way PDF's
    // continuous mode has.
    let epub_undo = {
        let reader = reader.clone();
        let state = state.clone();
        let widgets = widgets.clone();
        let view = web_view.clone();
        let rebuild_notes = rebuild_notes.clone();
        let undo_button = undo_button.clone();
        let redo_button = redo_button.clone();
        Rc::new(move || {
            let popped = {
                let mut r = reader.borrow_mut();
                match r.undo_stack.pop() {
                    Some(prev) => {
                        let current = r.annotations.clone();
                        r.redo_stack.push(current);
                        r.annotations = prev;
                        true
                    }
                    None => false,
                }
            };
            if !popped {
                toast(&widgets, "Nothing to undo");
                return;
            }
            let write_result = {
                let s = state.borrow();
                s.library
                    .as_ref()
                    .map(|lib| lib.write_annotations(&reader.borrow().annotations))
            };
            match write_result {
                Some(Ok(_)) => {
                    epub_apply_highlights(&view, &reader, None);
                    rebuild_notes();
                    toast(&widgets, "Undid last annotation change");
                }
                Some(Err(e)) => toast(&widgets, &format!("Could not undo: {e}")),
                None => toast(&widgets, "No open library"),
            }
            sync_epub_undo_redo_buttons(&reader, &undo_button, &redo_button);
        })
    };
    let epub_redo = {
        let reader = reader.clone();
        let state = state.clone();
        let widgets = widgets.clone();
        let view = web_view.clone();
        let rebuild_notes = rebuild_notes.clone();
        let undo_button = undo_button.clone();
        let redo_button = redo_button.clone();
        Rc::new(move || {
            let popped = {
                let mut r = reader.borrow_mut();
                match r.redo_stack.pop() {
                    Some(next) => {
                        let current = r.annotations.clone();
                        r.undo_stack.push(current);
                        r.annotations = next;
                        true
                    }
                    None => false,
                }
            };
            if !popped {
                toast(&widgets, "Nothing to redo");
                return;
            }
            let write_result = {
                let s = state.borrow();
                s.library
                    .as_ref()
                    .map(|lib| lib.write_annotations(&reader.borrow().annotations))
            };
            match write_result {
                Some(Ok(_)) => {
                    epub_apply_highlights(&view, &reader, None);
                    rebuild_notes();
                    toast(&widgets, "Redid annotation change");
                }
                Some(Err(e)) => toast(&widgets, &format!("Could not redo: {e}")),
                None => toast(&widgets, "No open library"),
            }
            sync_epub_undo_redo_buttons(&reader, &undo_button, &redo_button);
        })
    };
    {
        let epub_undo = epub_undo.clone();
        undo_button.connect_clicked(move |_| epub_undo());
    }
    {
        let epub_redo = epub_redo.clone();
        redo_button.connect_clicked(move |_| epub_redo());
    }

    // In-chapter search wiring: toggling `search_toggle` reveals the bar and focuses the
    // entry; turning it off clears the query and WebKit's highlight state
    // (`search_finish`) rather than leaving stale highlights visible.
    {
        let search_revealer = search_revealer.clone();
        let search_bar_entry = search_bar_entry.clone();
        let search_count = search_count.clone();
        let view = web_view.clone();
        search_toggle.connect_toggled(move |btn| {
            let on = btn.is_active();
            search_revealer.set_reveal_child(on);
            if on {
                search_bar_entry.grab_focus();
            } else {
                search_bar_entry.set_text("");
                search_count.set_text("");
                if let Some(fc) = webkit6::prelude::WebViewExt::find_controller(&view) {
                    fc.search_finish();
                }
            }
        });
    }
    {
        let view = web_view.clone();
        let search_count = search_count.clone();
        search_bar_entry.connect_search_changed(move |entry| {
            let text = entry.text();
            let Some(fc) = webkit6::prelude::WebViewExt::find_controller(&view) else {
                return;
            };
            if text.is_empty() {
                fc.search_finish();
                search_count.set_text("");
                return;
            }
            let options =
                (webkit6::FindOptions::CASE_INSENSITIVE | webkit6::FindOptions::WRAP_AROUND).bits();
            fc.search(&text, options, 1000);
        });
    }
    {
        let view = web_view.clone();
        search_prev.connect_clicked(move |_| {
            if let Some(fc) = webkit6::prelude::WebViewExt::find_controller(&view) {
                fc.search_previous();
            }
        });
    }
    {
        let view = web_view.clone();
        search_next.connect_clicked(move |_| {
            if let Some(fc) = webkit6::prelude::WebViewExt::find_controller(&view) {
                fc.search_next();
            }
        });
    }
    if let Some(fc) = webkit6::prelude::WebViewExt::find_controller(&web_view) {
        let count_label = search_count.clone();
        fc.connect_found_text(move |_, count| {
            count_label.set_text(&format!("{count} found"));
        });
        let count_label = search_count.clone();
        fc.connect_failed_to_find_text(move |_| {
            count_label.set_text("No matches");
        });
    }

    {
        let key_controller = gtk4::EventControllerKey::new();
        let epub_undo = epub_undo.clone();
        let epub_redo = epub_redo.clone();
        let search_toggle = search_toggle.clone();
        key_controller.connect_key_pressed(move |_, keyval, _keycode, modifiers| {
            if keyval == gdk::Key::z && modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
                if modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
                    epub_redo();
                } else {
                    epub_undo();
                }
                return glib::Propagation::Stop;
            }
            if keyval == gdk::Key::f && modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
                search_toggle.set_active(!search_toggle.is_active());
                return glib::Propagation::Stop;
            }
            if keyval == gdk::Key::Escape && search_toggle.is_active() {
                search_toggle.set_active(false);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        dialog.add_controller(key_controller);
    }

    if let Some(sidebar_toggle) = &sidebar_toggle {
        let paned = paned.clone();
        let sidebar_stack = sidebar_stack.clone();
        let notes_toggle = notes_toggle.clone();
        sidebar_toggle.connect_toggled(move |btn| {
            if btn.is_active() {
                notes_toggle.set_active(false);
                sidebar_stack.set_visible_child_name("contents");
                paned.set_start_child(Some(&sidebar_stack));
            } else if !notes_toggle.is_active() {
                paned.set_start_child(gtk4::Widget::NONE);
            }
        });
    }
    {
        let paned = paned.clone();
        let sidebar_stack = sidebar_stack.clone();
        let sidebar_toggle = sidebar_toggle.clone();
        let rebuild_notes = rebuild_notes.clone();
        notes_toggle.connect_toggled(move |btn| {
            if btn.is_active() {
                if let Some(st) = &sidebar_toggle {
                    st.set_active(false);
                }
                rebuild_notes();
                sidebar_stack.set_visible_child_name("notes");
                paned.set_start_child(Some(&sidebar_stack));
            } else if sidebar_toggle
                .as_ref()
                .map(|b| !b.is_active())
                .unwrap_or(true)
            {
                paned.set_start_child(gtk4::Widget::NONE);
            }
        });
    }

    // Re-apply saved highlights after every chapter load (initial load, TOC jump,
    // prev/next, a notes-sidebar jump — all funnel through `epub_go_to`'s `load_uri`, so
    // one handler here covers all of them), consuming `pending_scroll` if the navigation
    // that triggered this load set one.
    {
        let reader = reader.clone();
        let pending_scroll = pending_scroll.clone();
        web_view.connect_load_changed(move |view, event| {
            if event == webkit6::LoadEvent::Finished {
                let scroll_to = pending_scroll.borrow_mut().take();
                epub_apply_highlights(view, &reader, scroll_to.as_deref());
            }
        });
    }

    // Load the first chapter up front (the TOC/prev/next/notes-jump handlers all reuse
    // this same navigation path for consistency, but chapter 0 has to start somewhere).
    let first_chapter = reader.borrow().spine.get(start_index).cloned();
    if let Some(first) = first_chapter {
        epub_go_to(&reader, &web_view, &prev, &next, &chapter_label, &first);
    }

    {
        let reader = reader.clone();
        let view = web_view.clone();
        let prev_for_handler = prev.clone();
        let next = next.clone();
        let chapter_label = chapter_label.clone();
        prev.connect_clicked(move |_| {
            let target = {
                let r = reader.borrow();
                (r.index > 0).then(|| r.spine[r.index - 1].clone())
            };
            if let Some(target) = target {
                epub_go_to(
                    &reader,
                    &view,
                    &prev_for_handler,
                    &next,
                    &chapter_label,
                    &target,
                );
            }
        });
    }
    {
        let reader = reader.clone();
        let view = web_view.clone();
        let prev = prev.clone();
        let next_for_handler = next.clone();
        let chapter_label = chapter_label.clone();
        next.connect_clicked(move |_| {
            let target = {
                let r = reader.borrow();
                (r.index + 1 < r.spine.len()).then(|| r.spine[r.index + 1].clone())
            };
            if let Some(target) = target {
                epub_go_to(
                    &reader,
                    &view,
                    &prev,
                    &next_for_handler,
                    &chapter_label,
                    &target,
                );
            }
        });
    }

    {
        let reader = reader.clone();
        let view = web_view.clone();
        let state = state.clone();
        let widgets = widgets.clone();
        let mode_drop = mode_drop.clone();
        let color_drop = color_drop.clone();
        let rebuild_notes = rebuild_notes.clone();
        apply_button.connect_clicked(move |_| {
            let reader = reader.clone();
            let view_for_apply = view.clone();
            let state = state.clone();
            let widgets = widgets.clone();
            let kind = EPUB_MARK_KIND_OPTIONS
                .get(mode_drop.selected() as usize)
                .map(|(_, k)| *k)
                .unwrap_or(fond_bib::AnnotationKind::Highlight);
            let color = COLOR_PRESETS
                .get(color_drop.selected() as usize)
                .map(|(_, hex)| hex.to_string());
            let rebuild_notes = rebuild_notes.clone();
            view.evaluate_javascript(
                EPUB_CAPTURE_SELECTION_JS,
                None,
                None,
                gio::Cancellable::NONE,
                move |result| {
                    let raw = match result {
                        Ok(v) => v.to_str().to_string(),
                        Err(e) => {
                            toast(&widgets, &format!("Could not read selection: {e}"));
                            return;
                        }
                    };
                    let capture: EpubSelectionCapture = match serde_json::from_str(&raw) {
                        Ok(c) => c,
                        Err(_) => {
                            toast(&widgets, "Could not read selection");
                            return;
                        }
                    };
                    let snippet = capture.text.filter(|t| !t.trim().is_empty());
                    let Some(snippet) = (!capture.empty).then_some(snippet).flatten() else {
                        toast(&widgets, "Select some text first");
                        return;
                    };

                    let chapter = {
                        let r = reader.borrow();
                        r.spine.get(r.index).cloned()
                    };
                    let Some(chapter) = chapter else {
                        return;
                    };

                    let mut annotation = fond_bib::Annotation::drawn_epub(
                        kind,
                        chapter,
                        snippet,
                        capture.prefix,
                        capture.suffix,
                        None,
                    );
                    if kind == fond_bib::AnnotationKind::Highlight {
                        annotation.color = color.clone();
                    }
                    let id = annotation.id.clone();
                    push_epub_undo_snapshot(&reader);
                    reader.borrow_mut().annotations.upsert(annotation);

                    let write_result = {
                        let s = state.borrow();
                        s.library
                            .as_ref()
                            .map(|lib| lib.write_annotations(&reader.borrow().annotations))
                    };
                    match write_result {
                        Some(Ok(_)) => {
                            epub_apply_highlights(&view_for_apply, &reader, Some(&id));
                            toast(&widgets, "Added");
                            rebuild_notes();
                        }
                        Some(Err(e)) => toast(&widgets, &friendly::bib_error(&e)),
                        None => toast(&widgets, "No open library — not saved"),
                    }
                },
            );
        });
    }

    dialog.present();
}

/// Navigate the EPUB reader's `WebView` to `target` (a zip-internal path, optionally with a
/// `#fragment` for an in-chapter anchor — the same shape `fond_doc::EpubBook::spine`/`toc`
/// entries use). Updates `state.index` when `target`'s path (fragment stripped) matches a
/// spine entry, so the chapter label and prev/next sensitivity stay correct whether the jump
/// came from a TOC entry, the prev/next buttons, or the initial chapter-0 load — all three
/// funnel through here rather than duplicating the URI-building and label/button refresh.
fn epub_go_to(
    state: &Rc<RefCell<EpubReaderState>>,
    view: &webkit6::WebView,
    prev: &gtk4::Button,
    next: &gtk4::Button,
    chapter_label: &gtk4::Label,
    target: &str,
) {
    let (path, fragment) = target
        .split_once('#')
        .map_or((target, None), |(p, f)| (p, Some(f)));

    let cache_dir = {
        let mut r = state.borrow_mut();
        if let Some(idx) = r.spine.iter().position(|p| p == path) {
            r.index = idx;
        }
        r.cache_dir.clone()
    };

    let mut uri = gio::File::for_path(cache_dir.join(path)).uri().to_string();
    if let Some(fragment) = fragment {
        uri.push('#');
        uri.push_str(fragment);
    }
    view.load_uri(&uri);

    let r = state.borrow();
    chapter_label.set_text(&format!("Chapter {} of {}", r.index + 1, r.spine.len()));
    prev.set_sensitive(r.index > 0);
    next.set_sensitive(r.index + 1 < r.spine.len());
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
                (Some(page), Some(of)) => Some(fond_bib::Progress { page, of }),
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
