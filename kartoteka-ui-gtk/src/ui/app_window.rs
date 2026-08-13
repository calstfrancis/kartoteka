//! The main application window: a sidebar list of entries with a live filter, and a detail
//! pane showing the selected entry's YAML and note. All data comes from `fond-bib`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gtk4::prelude::*;
use gtk4::{gdk, gio, glib, Orientation};
use libadwaita as adw;
use libadwaita::prelude::*;
use webkit6::prelude::*;

use fond_bib::{entry as bibentry, Library};

use crate::config::Config;
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
}

/// How the visible list is ordered. `Default` keeps load order (by key) for an empty query
/// and relevance rank for a search; the others sort the visible set.
#[derive(Default, Clone, Copy, PartialEq)]
enum SortBy {
    #[default]
    Default,
    Title,
    Author,
    Year,
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
    /// Column the list is sorted by, and direction (`true` = descending).
    sort_by: SortBy,
    sort_desc: bool,
}

struct Widgets {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    subtitle: adw::WindowTitle,
    status_label: gtk4::Label,
    listbox: gtk4::ListBox,
    detail: gtk4::Box,
    collections_listbox: gtk4::ListBox,
    search: gtk4::SearchEntry,
}

pub fn build(app: &adw::Application, config: Config) -> adw::ApplicationWindow {
    let state = Rc::new(RefCell::new(AppState::default()));
    let config = Rc::new(RefCell::new(config));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Kartoteka")
        .default_width(1000)
        .default_height(680)
        .build();

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
    search.set_placeholder_text(Some("Search — author: title: tag: type: year:"));
    search.set_margin_top(6);
    search.set_margin_bottom(6);
    search.set_margin_start(6);
    search.set_margin_end(6);
    // Sort control: a "Sort by" dropdown and an ascending/descending toggle.
    let sort_row = gtk4::Box::new(Orientation::Horizontal, 6);
    sort_row.set_margin_start(6);
    sort_row.set_margin_end(6);
    sort_row.set_margin_bottom(6);
    let sort_drop = gtk4::DropDown::from_strings(&["Default", "Title", "Author", "Year"]);
    sort_drop.set_hexpand(true);
    sort_drop.set_tooltip_text(Some("Sort the list"));
    let sort_dir = gtk4::ToggleButton::new();
    sort_dir.set_icon_name("view-sort-descending-symbolic");
    sort_dir.set_tooltip_text(Some("Descending order"));
    sort_row.append(&sort_drop);
    sort_row.append(&sort_dir);

    let listbox = gtk4::ListBox::new();
    listbox.set_selection_mode(gtk4::SelectionMode::Single);
    listbox.add_css_class("fond-list");
    let list_scroll = gtk4::ScrolledWindow::new();
    list_scroll.set_child(Some(&listbox));
    list_scroll.set_vexpand(true);
    sidebar.append(&search);
    sidebar.append(&sort_row);
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
    inner_paned.set_resize_start_child(false);
    inner_paned.set_position(300);

    let paned = gtk4::Paned::new(Orientation::Horizontal);
    paned.set_start_child(Some(&collections_box));
    paned.set_end_child(Some(&inner_paned));
    paned.set_resize_start_child(false);
    paned.set_position(190);

    toolbar_view.set_content(Some(&paned));

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
        listbox: listbox.clone(),
        detail,
        collections_listbox: collections_listbox.clone(),
        search: search.clone(),
    });

    // Collection selection → set the filter and refresh the list.
    {
        let state = state.clone();
        let widgets = widgets.clone();
        collections_listbox.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let idx = row.index();
            if idx < 0 {
                return;
            }
            let idx = idx as usize;
            let (n_coll, saved) = {
                let s = state.borrow();
                (s.collections.len(), s.saved_searches.clone())
            };
            if idx == 0 {
                // All entries.
                state.borrow_mut().collection_filter = None;
                widgets.search.set_text("");
                refresh_list(&state, &widgets);
            } else if idx <= n_coll {
                let slug = state.borrow().collections.get(idx - 1).cloned();
                state.borrow_mut().collection_filter = slug;
                widgets.search.set_text("");
                refresh_list(&state, &widgets);
            } else if let Some((_, query)) = saved.get(idx - n_coll - 1) {
                // Saved search: clear collection filter, run the query.
                state.borrow_mut().collection_filter = None;
                widgets.search.set_text(query); // triggers refresh via search_changed
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

    // Row selection → show detail.
    {
        let state = state.clone();
        let widgets = widgets.clone();
        listbox.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                let idx = row.index();
                if idx >= 0 {
                    show_detail(&state, &widgets, idx as usize);
                }
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

    // Sort column.
    {
        let state = state.clone();
        let widgets = widgets.clone();
        sort_drop.connect_selected_notify(move |drop| {
            state.borrow_mut().sort_by = match drop.selected() {
                1 => SortBy::Title,
                2 => SortBy::Author,
                3 => SortBy::Year,
                _ => SortBy::Default,
            };
            refresh_list(&state, &widgets);
        });
    }
    // Sort direction.
    {
        let state = state.clone();
        let widgets = widgets.clone();
        sort_dir.connect_toggled(move |btn| {
            let desc = btn.is_active();
            btn.set_icon_name(if desc {
                "view-sort-descending-symbolic"
            } else {
                "view-sort-ascending-symbolic"
            });
            btn.set_tooltip_text(Some(if desc {
                "Descending order"
            } else {
                "Ascending order"
            }));
            state.borrow_mut().sort_desc = desc;
            refresh_list(&state, &widgets);
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
                return false;
            };
            if state.borrow().library.is_none() {
                toast(&widgets, "Open a library first");
                return false;
            }
            let mut handled = false;
            for file in files.files() {
                if let Some(path) = file.path() {
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
            }
            handled
        });
        window.add_controller(drop);
    }

    // Hamburger actions (win.acquire / win.reindex / win.theme / win.about).
    add_window_actions(&window, &state, &widgets, &config);

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

    window
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
    };

    activate_row(&rows, &popover, "New item…", "win.new-item");
    activate_row(&rows, &popover, "Acquire…", "win.acquire");
    activate_row(&rows, &popover, "Add PDF…", "win.add-pdf");
    activate_row(&rows, &popover, "Add EPUB…", "win.add-epub");
    activate_row(&rows, &popover, "Add folder of PDFs…", "win.add-folder");
    activate_row(&rows, &popover, "Add from URL…", "win.add-url");
    activate_row(&rows, &popover, "Import…", "win.import");
    rows.append(&popover_separator());
    activate_row(&rows, &popover, "Manage tags…", "win.tags");
    activate_row(&rows, &popover, "Nodes…", "win.nodes");
    activate_row(&rows, &popover, "Tasks…", "win.tasks");
    activate_row(&rows, &popover, "Find duplicates…", "win.duplicates");
    rows.append(&popover_separator());
    activate_row(&rows, &popover, "Cite…", "win.cite");
    activate_row(&rows, &popover, "Export bibliography…", "win.export-bib");
    rows.append(&popover_separator());
    activate_row(&rows, &popover, "Save current search…", "win.save-search");
    activate_row(&rows, &popover, "Back up (git commit)…", "win.backup");
    activate_row(&rows, &popover, "Sign in to GitHub…", "win.github-signin");
    activate_row(&rows, &popover, "Back up to WebDAV…", "win.webdav-backup");
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

    activate_row(&rows, &popover, "About Kartoteka", "win.about");

    popover
}

fn add_window_actions(
    window: &adw::ApplicationWindow,
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    config: &Rc<RefCell<Config>>,
) {
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
    if let Some(app) = window.application() {
        app.set_accels_for_action("win.cite", &["<Primary>k"]);
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
        let action = gio::SimpleAction::new("nodes", None);
        action.connect_activate(move |_, _| show_nodes_dialog(&state, &widgets));
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
                            Err(e) => {
                                toast(&widgets, &format!("Added {key}, but attach failed: {e}"))
                            }
                        }
                        reload_current(&state, &widgets);
                    }
                    Ok(_) => toast(&widgets, "The record produced no entry"),
                    Err(e) => toast(&widgets, &format!("Could not add entry: {e}")),
                }
            }
            Err(e) => toast(&widgets, &format!("PDF import failed: {e}")),
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
                            Err(e) => {
                                toast(&widgets, &format!("Added {key}, but attach failed: {e}"))
                            }
                        }
                        rebuild_index_silent(&state);
                        reload_current(&state, &widgets);
                    }
                    Ok(_) => toast(&widgets, "The record produced no entry"),
                    Err(e) => toast(&widgets, &format!("Could not add entry: {e}")),
                }
            }
            Err(e) => toast(&widgets, &format!("EPUB import failed: {e}")),
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
                    Err(e) => toast(&widgets, &format!("Could not add entry: {e}")),
                }
            }
            Err(e) => toast(&widgets, &format!("Add from URL failed: {e}")),
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
    content.append(&labeled("Container", &container));
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
                Err(e) => toast(&widgets, &format!("Could not create entry: {e}")),
            }
        });
    }

    dialog.present();
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
                Err(e) => toast(&widgets, &format!("Backup failed: {e}")),
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
fn select_key(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, key: &str) {
    let visible_pos = |state: &Rc<RefCell<AppState>>| -> Option<usize> {
        let s = state.borrow();
        s.visible.iter().position(|&i| s.entries[i].key == key)
    };

    if let Some(pos) = visible_pos(state) {
        if let Some(row) = widgets.listbox.row_at_index(pos as i32) {
            widgets.listbox.select_row(Some(&row));
        }
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
    if let Some(pos) = visible_pos(state) {
        if let Some(row) = widgets.listbox.row_at_index(pos as i32) {
            widgets.listbox.select_row(Some(&row));
        }
    }
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
        .placeholder_text("identifier, e.g. 10.1000/xyz")
        .activates_default(true)
        .hexpand(true)
        .build();
    let spinner = gtk4::Spinner::new();
    spinner.set_halign(gtk4::Align::End);

    content.append(&dropdown);
    content.append(&entry);
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
                                toast(&widgets, &format!("Could not parse record: {e}"));
                                add.set_sensitive(true);
                                entry.set_sensitive(true);
                            }
                        }
                    }
                    Err(e) => {
                        toast(&widgets, &format!("Lookup failed: {e}"));
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
            toast(widgets, &format!("Could not open library: {e}"));
            return;
        }
    };

    let mut entries = Vec::new();
    match library.keys_sorted() {
        Ok(keys) => {
            for key in keys {
                if let Ok(parsed) = library.load_entry(&key) {
                    entries.push(EntrySummary {
                        author: bibentry::author_names(&parsed.entry),
                        year: bibentry::year(&parsed.entry)
                            .map(|y| y.to_string())
                            .unwrap_or_default(),
                        title: bibentry::title_string(&parsed.entry).unwrap_or_default(),
                        key,
                    });
                }
            }
        }
        Err(e) => {
            toast(widgets, &format!("Could not read entries: {e}"));
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
    refresh_collections(state, widgets);
    refresh_list(state, widgets);
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

/// Rebuild the collections list: "All entries", each collection, then saved searches.
fn refresh_collections(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    let slugs = state
        .borrow()
        .library
        .as_ref()
        .and_then(|l| l.collection_slugs().ok())
        .unwrap_or_default();

    // Display name per slug.
    let mut rows: Vec<(String, String)> = Vec::new(); // (slug, display name)
    {
        let s = state.borrow();
        if let Some(lib) = s.library.as_ref() {
            for slug in &slugs {
                let name = lib
                    .load_collection(slug)
                    .map(|c| c.name)
                    .unwrap_or_else(|_| slug.clone());
                rows.push((slug.clone(), name));
            }
        }
    }
    state.borrow_mut().collections = rows.iter().map(|(s, _)| s.clone()).collect();

    let lb = &widgets.collections_listbox;
    while let Some(child) = lb.first_child() {
        lb.remove(&child);
    }
    lb.append(&collection_row("All entries", "view-list-symbolic"));
    for (_, name) in &rows {
        lb.append(&collection_row(name, "folder-symbolic"));
    }
    for (name, _) in &state.borrow().saved_searches {
        lb.append(&collection_row(name, "folder-saved-search-symbolic"));
    }
    // Select "All entries" without triggering a reload loop.
    if let Some(first) = lb.row_at_index(0) {
        lb.select_row(Some(&first));
    }
}

fn collection_row(label: &str, icon: &str) -> gtk4::ListBoxRow {
    let hbox = gtk4::Box::new(Orientation::Horizontal, 8);
    hbox.set_margin_top(5);
    hbox.set_margin_bottom(5);
    hbox.set_margin_start(8);
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

/// List duplicate groups (matched by DOI/ISBN/title+year) with a Merge button each.
fn show_duplicates_dialog(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    let groups = {
        let s = state.borrow();
        let Some(library) = s.library.as_ref() else {
            toast(widgets, "Open a library first");
            return;
        };
        library.find_duplicates().unwrap_or_default()
    };
    if groups.is_empty() {
        toast(widgets, "No duplicates found");
        return;
    }

    let dialog = adw::Window::new();
    dialog.set_title(Some(&format!("Duplicates ({})", groups.len())));
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

    for group in &groups {
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
            let dialog = dialog.clone();
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
                let _ = &dialog;
            });
        }
        inner.append(&merge);
        card.append(&inner);
        list.append(&card);
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
                    toast(&widgets, &format!("Could not save: {e}"));
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
                Some(Err(e)) => toast(&widgets, &format!("Could not save tags: {e}")),
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
        for slug in &slugs {
            let coll = lib.load_collection(slug).unwrap_or_default();
            let check = gtk4::CheckButton::with_label(if coll.name.is_empty() {
                slug
            } else {
                &coll.name
            });
            check.set_active(coll.keys.iter().any(|k| k == key));
            content.append(&check);
            checks.push((slug.clone(), check));
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
                    let mut coll = match lib.load_collection(slug) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    let present = coll.keys.iter().any(|k| k == &key);
                    if check.is_active() && !present {
                        coll.keys.push(key.clone());
                        let _ = lib.save_collection(slug, &coll);
                    } else if !check.is_active() && present {
                        coll.keys.retain(|k| k != &key);
                        let _ = lib.save_collection(slug, &coll);
                    }
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
                Err(e) => toast(&widgets, &format!("Could not update: {e}")),
            }
        });
    }

    dialog.present();
    search.grab_focus();
}

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
            let slug = fond_bib::zotero::slugify(&name);
            let result = {
                let s = state.borrow();
                let lib = s.library.as_ref().expect("library open");
                lib.save_collection(
                    &slug,
                    &fond_bib::Collection {
                        name: name.clone(),
                        description: None,
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
                Err(e) => toast(&widgets, &format!("Could not create: {e}")),
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

        // Apply the chosen sort over the visible set. `Default` leaves load/relevance order.
        let sort_by = s.sort_by;
        let desc = s.sort_desc;
        if sort_by != SortBy::Default {
            let st = &mut *s;
            let entries = &st.entries;
            st.visible.sort_by(|&a, &b| {
                let key = |i: usize| -> String {
                    let e = &entries[i];
                    match sort_by {
                        SortBy::Title => e.title.to_lowercase(),
                        SortBy::Author => e.author.to_lowercase(),
                        SortBy::Year => e.year.clone(),
                        SortBy::Default => String::new(),
                    }
                };
                key(a).cmp(&key(b))
            });
            if desc {
                st.visible.reverse();
            }
        }
    }

    // Rebuild rows.
    while let Some(child) = widgets.listbox.first_child() {
        widgets.listbox.remove(&child);
    }
    let has_rows = {
        let s = state.borrow();
        let last = s.visible.len().saturating_sub(1);
        for (i, &idx) in s.visible.iter().enumerate() {
            let e = &s.entries[idx];
            let row = make_row(e);
            if i == 0 {
                row.add_css_class("fond-card-first");
            }
            if i == last {
                row.add_css_class("fond-card-last");
            }
            widgets.listbox.append(&row);
        }
        !s.visible.is_empty()
    };

    // Select the top row so the detail pane always reflects the current list.
    if has_rows {
        if let Some(first) = widgets.listbox.row_at_index(0) {
            widgets.listbox.select_row(Some(&first));
        }
    } else {
        clear_box(&widgets.detail);
    }
}

fn make_row(e: &EntrySummary) -> gtk4::ListBoxRow {
    let vbox = gtk4::Box::new(Orientation::Vertical, 2);
    vbox.set_margin_top(6);
    vbox.set_margin_bottom(6);
    vbox.set_margin_start(8);
    vbox.set_margin_end(8);

    let title = gtk4::Label::new(Some(if e.title.is_empty() { &e.key } else { &e.title }));
    title.set_halign(gtk4::Align::Start);
    title.set_xalign(0.0);
    title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    title.add_css_class("fond-row-title");

    let meta_text = match (e.author.is_empty(), e.year.is_empty()) {
        (false, false) => format!("{} · {}", e.author, e.year),
        (false, true) => e.author.clone(),
        (true, false) => e.year.clone(),
        (true, true) => e.key.clone(),
    };
    let meta = gtk4::Label::new(Some(&meta_text));
    meta.set_halign(gtk4::Align::Start);
    meta.set_xalign(0.0);
    meta.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    meta.add_css_class("fond-row-meta");

    vbox.append(&title);
    vbox.append(&meta);

    let row = gtk4::ListBoxRow::new();
    row.add_css_class("fond-card");
    row.add_css_class("fond-row");
    row.set_child(Some(&vbox));
    row
}

fn show_detail(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, visible_index: usize) {
    let s = state.borrow();
    let Some(&entry_idx) = s.visible.get(visible_index) else {
        return;
    };
    let Some(library) = s.library.as_ref() else {
        return;
    };
    let summary = &s.entries[entry_idx];
    let key = summary.key.clone();

    let b = &widgets.detail;
    clear_box(b);

    // Load the note once (used for the action row, fields, and prose below).
    let note = library.load_note(&key).ok().flatten();

    // Title.
    let title_text = if summary.title.is_empty() {
        key.as_str()
    } else {
        summary.title.as_str()
    };
    let title = gtk4::Label::new(Some(title_text));
    title.add_css_class("title-2");
    title.set_wrap(true);
    title.set_xalign(0.0);
    title.set_halign(gtk4::Align::Start);
    title.set_selectable(true);
    b.append(&title);

    // Byline.
    let byline = match (summary.author.is_empty(), summary.year.is_empty()) {
        (false, false) => format!("{} · {}", summary.author, summary.year),
        (false, true) => summary.author.clone(),
        (true, false) => summary.year.clone(),
        (true, true) => String::new(),
    };
    if !byline.is_empty() {
        let label = gtk4::Label::new(Some(&byline));
        label.add_css_class("dim-label");
        label.set_wrap(true);
        label.set_xalign(0.0);
        label.set_halign(gtk4::Align::Start);
        b.append(&label);
    }

    // First present attachment Kartoteka has a built-in reader for (PDF or EPUB), typed by
    // filename extension. Previously this was an untyped `find_map` over *any* attachment
    // (named `present_pdf` but really "first present attachment of any kind") — an EPUB
    // attachment was picked up identically to a PDF one, so "Read" opened `show_pdf_reader`
    // against EPUB bytes, which PDFium can't parse: a blank "Page 1 of 1" window with no
    // error. Typing the lookup here is what lets Read/Annotations route to the right reader
    // per format (M5-SPEC.md 5A).
    let present_reader_attachment = note.as_ref().and_then(|n| {
        n.frontmatter.attachments.iter().find_map(|att| {
            let kind = ReaderAttachmentKind::from_filename(&att.filename)?;
            let hex = att
                .hash
                .split_once(':')
                .map(|(_, h)| h)
                .unwrap_or(&att.hash);
            let path = library.attachment_blob_path(hex);
            path.exists()
                .then(|| (path, att.filename.clone(), att.hash.clone(), kind))
        })
    });
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

    let doi = library
        .load_entry(&key)
        .ok()
        .and_then(|p| p.entry.doi().map(|d| d.to_string()));

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

    // Primary: the read action, contextual to whether a PDF/EPUB is attached or a DOI is
    // known, and to *which* of those formats is attached.
    if let Some((path, _filename, hash, kind)) = present_reader_attachment.clone() {
        match kind {
            ReaderAttachmentKind::Pdf => {
                let read_button = gtk4::Button::with_label("Read");
                // Resumes at the saved Progress page, if any — "Read" opening on page 1
                // every time despite a recorded reading position was the whole gap 5A/M5's
                // Tier 2 exists to close.
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
            ReaderAttachmentKind::Epub => {
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
        }
    } else if let Some(doi) = doi.clone() {
        let find_button = gtk4::Button::with_label("Find PDF");
        find_button.set_tooltip_text(Some("Search Unpaywall for an open-access PDF"));
        let state = state.clone();
        let widgets = widgets.clone();
        let key = key.clone();
        find_button.connect_clicked(move |_| find_pdf_unpaywall(&state, &widgets, &key, &doi));
        actions.append(&find_button);
    }

    // Edit: one button covering both edit surfaces — the bibliographic fields (title,
    // author, year…) and the personal note (tags, status, rating, prose) — via a small
    // popover, so neither has to lose out on being "the" primary edit action.
    let edit_button = gtk4::MenuButton::builder().label("Edit").build();
    {
        let (popover, rows) = popover_menu(190);
        let row = popover_button("Edit citation info…", false);
        row.set_tooltip_text(Some("Edit the bibliographic fields (title, author, year…)"));
        {
            let popover = popover.clone();
            let state = state.clone();
            let widgets = widgets.clone();
            let key = key.clone();
            row.connect_clicked(move |_| {
                popover.popdown();
                show_citation_editor(&state, &widgets, &key);
            });
        }
        rows.append(&row);
        let row = popover_button("Edit note…", false);
        row.set_tooltip_text(Some("Edit tags, status, rating, and your own notes"));
        {
            let popover = popover.clone();
            let state = state.clone();
            let widgets = widgets.clone();
            let key = key.clone();
            row.connect_clicked(move |_| {
                popover.popdown();
                show_note_editor(&state, &widgets, &key);
            });
        }
        rows.append(&row);
        edit_button.set_popover(Some(&popover));
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
    let has_annotations = present_reader_attachment.is_some()
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

        if has_annotations {
            if let Some((path, _filename, hash, kind)) = present_reader_attachment.clone() {
                let row = popover_button("Annotations…", false);
                row.set_tooltip_text(Some("Review, jump to, or delete highlights"));
                let popover = popover.clone();
                let state = state.clone();
                let widgets = widgets.clone();
                let key = key.clone();
                let title = title_text.to_string();
                row.connect_clicked(move |_| {
                    popover.popdown();
                    show_annotations_dialog(&state, &widgets, &key, kind, &hash, &path, &title);
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

    // Structured fields from the entry.
    if let Ok(parsed) = library.load_entry(&key) {
        let e = &parsed.entry;
        fields.append(&field_row(
            "Type",
            &format!("{:?}", e.entry_type()).to_lowercase(),
        ));
        let key_row = field_row("Citation key", &key);
        key_row.set_tooltip_text(Some(
            "Used to cite this work in a Typst document, e.g. @key",
        ));
        details_fields.append(&key_row);
        if let Some(doi) = e.doi() {
            fields.append(&field_row("DOI", doi));
        }
        if let Some(isbn) = e.isbn() {
            fields.append(&field_row("ISBN", isbn));
        }
    }

    // Note-derived state: tags, attachments, annotations, prose.
    let mut note_body = String::new();
    if let Some(note) = &note {
        if !note.frontmatter.tags.is_empty() {
            fields.append(&tags_row(&note.frontmatter.tags));
        }
        if let Some(status) = note.frontmatter.read_status {
            fields.append(&field_row("Status", &format!("{status:?}").to_lowercase()));
        }
        if let Some(rating) = note.frontmatter.rating {
            fields.append(&field_row("Rating", &"★".repeat(rating.min(5) as usize)));
        }
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
    if let Some(row) = widgets.listbox.selected_row() {
        let idx = row.index();
        if idx >= 0 {
            show_detail(state, widgets, idx as usize);
        }
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
    kind: ReaderAttachmentKind,
    hash: &str,
    blob: &std::path::Path,
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

        let goto_label = match kind {
            ReaderAttachmentKind::Pdf => "Go to page",
            ReaderAttachmentKind::Epub => "Go to chapter",
        };
        let goto_button = gtk4::Button::with_label(goto_label);
        {
            let state = state.clone();
            let widgets = widgets.clone();
            let key = key.to_string();
            let hash = hash.to_string();
            let blob = blob.to_path_buf();
            let title = reader_title.to_string();
            let page = annotation.page;
            let annotation_id = annotation.id.clone();
            goto_button.connect_clicked(move |_| match kind {
                ReaderAttachmentKind::Pdf => {
                    // `page` is always `Some` for a PDF-anchored annotation; `unwrap_or(1)`
                    // is just a defensive fallback, not an expected path.
                    show_pdf_reader(
                        &state,
                        &widgets,
                        &key,
                        &hash,
                        &blob,
                        &title,
                        page.unwrap_or(1),
                    )
                }
                ReaderAttachmentKind::Epub => show_epub_reader(
                    &state,
                    &widgets,
                    &key,
                    &hash,
                    &blob,
                    &title,
                    Some(&annotation_id),
                ),
            });
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
                    toast(&widgets, &format!("Could not save note: {e}"));
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
                    Some(Err(e)) => toast(&widgets, &format!("Could not delete: {e}")),
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
    /// via the separate "Note…" button instead.
    draw_kind: fond_bib::AnnotationKind,
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

/// The `DropDown`'s fixed option order — index into this, not into `AnnotationKind`
/// directly, since the drop-down deliberately excludes `Note` (drawn via its own button, not
/// a drag gesture).
const DRAW_KIND_OPTIONS: [(&str, fond_bib::AnnotationKind); 3] = [
    ("Highlight", fond_bib::AnnotationKind::Highlight),
    ("Underline", fond_bib::AnnotationKind::Underline),
    ("Strikeout", fond_bib::AnnotationKind::Strikeout),
];

const READER_BASE_WIDTH: f64 = 820.0;
/// Amber at ~35% opacity — a highlight tint, not a solid block.
const HIGHLIGHT_RGBA: [u8; 4] = [246, 195, 68, 90];
/// Blue at ~50% opacity — deliberately distinct from `HIGHLIGHT_RGBA` so the current search
/// match reads as "found this" and not as a saved highlight.
const SEARCH_MATCH_RGBA: [u8; 4] = [66, 133, 244, 130];
/// Below this, a drag reads as a stray click, not an intentional highlight.
const MIN_DRAG_PX: f64 = 6.0;
/// Vertical gap between pages in continuous-scroll mode.
const CONTINUOUS_PAGE_GAP: f64 = 8.0;

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

fn save_drag_annotation(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    reader: &Rc<RefCell<ReaderState>>,
    pdf_hash: &str,
    page: u16,
    geom: DragGeometry,
) -> bool {
    let DragGeometry {
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
        return false;
    }
    let scale_x = render_w as f64 / page_w_pts as f64;
    let scale_y = render_h as f64 / page_h_pts as f64;

    let px0 = start_x.min(end_x).clamp(0.0, render_w as f64);
    let px1 = start_x.max(end_x).clamp(0.0, render_w as f64);
    let py0 = start_y.min(end_y).clamp(0.0, render_h as f64);
    let py1 = start_y.max(end_y).clamp(0.0, render_h as f64);

    let x0 = px0 / scale_x;
    let x1 = px1 / scale_x;
    // PDF y is bottom-up; the drag's y is top-down pixel space.
    let y_top = page_h_pts as f64 - py0 / scale_y;
    let y_bottom = page_h_pts as f64 - py1 / scale_y;

    let (draw_kind, draw_color) = {
        let r = reader.borrow();
        (r.draw_kind, r.draw_color.clone())
    };

    // Prefer the actual text under the drag — one quad per line it spans, hugging the
    // glyphs — over the drag rectangle's own bounding box. Falls back to the plain
    // rectangle when the drag covers no text (a figure, a blank margin).
    let (quads, snippet) = {
        let r = reader.borrow();
        fond_doc::select_text_in_rect(
            &r.pdfium,
            &r.bytes,
            page,
            x0 as f32,
            y_bottom as f32,
            x1 as f32,
            y_top as f32,
        )
        .ok()
        .flatten()
    }
    .map(|sel| (sel.quads, Some(sel.text)))
    .unwrap_or_else(|| {
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
            toast(widgets, &format!("Could not save: {e}"));
            false
        }
        None => {
            toast(widgets, "No open library — not saved");
            false
        }
    }
}

/// Build continuous-scroll mode's per-page `Picture` widgets, if not already built, and
/// render every page eagerly. A no-op if `reader.continuous_pictures` is already populated
/// (from an earlier toggle-on this session).
///
/// Eager, not lazy/virtualized: for the page counts this app actually sees (academic
/// papers, book chapters — tens of pages, rarely hundreds), rendering everything on first
/// toggle-on is a one-time, well-under-a-second cost, and it keeps this far simpler than a
/// virtualized alternative — every page gets one *permanent* widget, so its drag-to-annotate
/// gesture can capture that page's index directly with no risk of a recycled widget later
/// belonging to a different page (the failure mode a `ListView`-based virtualized version
/// would have to guard against). A very large PDF would pay a real up-front cost here; not
/// addressed, since it isn't this app's common case.
fn build_continuous_view(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    reader: &Rc<RefCell<ReaderState>>,
    pdf_hash: &str,
    continuous_box: &gtk4::Box,
) {
    if !reader.borrow().continuous_pictures.is_empty() {
        return;
    }
    let (count, zoom) = {
        let r = reader.borrow();
        (r.count, r.zoom)
    };

    let mut pictures = Vec::with_capacity(count as usize);
    let mut offsets = Vec::with_capacity(count as usize + 1);
    let mut y = 0.0f64;

    for page in 0..count {
        let rendered = {
            let r = reader.borrow();
            render_pdf_page_texture(&r, page)
        };
        let picture = gtk4::Picture::new();
        picture.set_halign(gtk4::Align::Center);
        picture.set_can_target(true);

        let (w, h) = match &rendered {
            Some((texture, w, h, _)) => {
                picture.set_paintable(Some(texture));
                (*w, *h)
            }
            None => {
                // Rendering failed for this page — fall back to a plausible size from its
                // point dimensions alone, so the layout doesn't collapse to zero height.
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
                (w, h)
            }
        };
        picture.set_size_request(w as i32, h as i32);
        offsets.push(y);
        y += h as f64 + CONTINUOUS_PAGE_GAP;

        // Drag-to-annotate on this page's own permanent Picture — `page` is captured by
        // value, so (unlike a recycled `ListView` row) it can never go stale.
        {
            let drag = gtk4::GestureDrag::new();
            let state = state.clone();
            let widgets = widgets.clone();
            let reader = reader.clone();
            let pdf_hash = pdf_hash.to_string();
            let this_picture = picture.clone();
            drag.connect_drag_end(move |gesture, offset_x, offset_y| {
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
                let saved = save_drag_annotation(
                    &state,
                    &widgets,
                    &reader,
                    &pdf_hash,
                    page,
                    DragGeometry {
                        render_w,
                        render_h,
                        page_w_pts: page_pts.0,
                        page_h_pts: page_pts.1,
                        start_x,
                        start_y,
                        end_x,
                        end_y,
                    },
                );
                if saved {
                    render_continuous_page(&reader, page);
                }
            });
            picture.add_controller(drag);
        }

        continuous_box.append(&picture);
        pictures.push(picture);
    }
    offsets.push(y); // sentinel: total content height

    let mut r = reader.borrow_mut();
    r.continuous_pictures = pictures;
    r.continuous_offsets = offsets;
    r.continuous_rendered = vec![true; count as usize];
}

/// Tear down and rebuild continuous-scroll mode's widgets after a zoom change — page pixel
/// sizes all changed, so every offset is stale too. A no-op if continuous mode was never
/// built (the next toggle-on will build fresh at the new zoom already). Simpler than
/// resizing everything in place: zoom changes are infrequent, so paying a full rebuild is a
/// reasonable trade for not having two code paths (initial build vs. resize-in-place) to
/// keep in sync.
fn rebuild_continuous_view_for_zoom(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    reader: &Rc<RefCell<ReaderState>>,
    pdf_hash: &str,
    continuous_box: &gtk4::Box,
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
    build_continuous_view(state, widgets, reader, pdf_hash, continuous_box);
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
        draw_kind: fond_bib::AnnotationKind::Highlight,
        search_matches: Vec::new(),
        search_current: 0,
        draw_color: COLOR_PRESETS[0].1.to_string(),
        continuous_pictures: Vec::new(),
        continuous_offsets: Vec::new(),
        continuous_rendered: Vec::new(),
    }));

    let dialog = adw::Window::new();
    dialog.set_title(Some(title));
    dialog.set_transient_for(Some(window));
    dialog.set_default_size(900, 820);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");

    let prev = gtk4::Button::from_icon_name("go-previous-symbolic");
    prev.set_tooltip_text(Some("Previous page"));
    let next = gtk4::Button::from_icon_name("go-next-symbolic");
    next.set_tooltip_text(Some("Next page"));
    let page_label = gtk4::Label::new(None);
    page_label.add_css_class("dim-label");
    let nav = gtk4::Box::new(Orientation::Horizontal, 6);
    nav.append(&prev);
    nav.append(&page_label);
    nav.append(&next);
    header.set_title_widget(Some(&nav));

    let zoom_out = gtk4::Button::from_icon_name("zoom-out-symbolic");
    zoom_out.set_tooltip_text(Some("Zoom out"));
    let zoom_in = gtk4::Button::from_icon_name("zoom-in-symbolic");
    zoom_in.set_tooltip_text(Some("Zoom in"));

    let note_button = gtk4::Button::with_label("Note…");
    note_button.set_tooltip_text(Some("Add a marginal note on the current page"));

    let mode_labels: Vec<&str> = DRAW_KIND_OPTIONS.iter().map(|(l, _)| *l).collect();
    let mode_drop = gtk4::DropDown::from_strings(&mode_labels);
    mode_drop.set_tooltip_text(Some("What a drag on the page creates"));

    let color_labels: Vec<&str> = COLOR_PRESETS.iter().map(|(l, _)| *l).collect();
    let color_drop = gtk4::DropDown::from_strings(&color_labels);
    color_drop.set_tooltip_text(Some("Highlight colour"));

    // Only present when the PDF actually has an outline — most don't. Built and wired below
    // (after `render`/`reader` exist), but declared and packed here alongside the rest of
    // the header so the pack_end ordering comment covers it too.
    let contents_button = (!outline_entries.is_empty())
        .then(|| gtk4::MenuButton::builder().label("Contents").build());

    // The current page's annotations, inline — an alternative to the separate
    // "Annotations…" dialog (still available from the detail pane, and still the only way
    // to see the *whole document's* annotations at once) for the common case of "what's on
    // this page." Content is rebuilt fresh on every open (wired below), since it's
    // page-dependent and the page changes constantly while reading.
    let page_annots_button = gtk4::MenuButton::builder().label("This page").build();
    page_annots_button.set_tooltip_text(Some("Annotations on the current page"));

    let continuous_toggle = gtk4::ToggleButton::with_label("Continuous");
    continuous_toggle.set_tooltip_text(Some(
        "Scroll continuously through every page, instead of one page at a time",
    ));

    // pack_end order is the reverse of visual order (last-packed ends up leftmost) — same
    // gotcha CLAUDE.md notes for the hamburger menu. Visual order here, left to right:
    // Contents (if present), This page, Continuous, mode picker, colour picker, Note,
    // zoom in, zoom out.
    header.pack_end(&zoom_out);
    header.pack_end(&zoom_in);
    header.pack_end(&note_button);
    header.pack_end(&color_drop);
    header.pack_end(&mode_drop);
    header.pack_end(&continuous_toggle);
    header.pack_end(&page_annots_button);
    if let Some(contents_button) = &contents_button {
        header.pack_end(contents_button);
    }
    view.add_top_bar(&header);

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
    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_child(Some(&picture));
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
    view.set_content(Some(&content));
    dialog.set_content(Some(&view));

    // Render the current page into the Picture (via the shared helper both this view and
    // continuous-scroll mode use), and refresh the page label.
    let render = {
        let reader = reader.clone();
        let picture = picture.clone();
        let page_label = page_label.clone();
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
            page_label.set_text(&format!("Page {} of {}", r.page + 1, r.count));
            prev.set_sensitive(r.page > 0);
            next.set_sensitive(r.page + 1 < r.count);
        })
    };
    render();

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
        drag.connect_drag_end(move |gesture, offset_x, offset_y| {
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
            let saved = save_drag_annotation(
                &state,
                &widgets,
                &reader,
                &pdf_hash,
                page,
                DragGeometry {
                    render_w,
                    render_h,
                    page_w_pts,
                    page_h_pts,
                    start_x,
                    start_y,
                    end_x,
                    end_y,
                },
            );
            if saved {
                render();
                // Keep continuous mode's copy of this page in sync too, in case it was
                // already built from an earlier toggle-on and the user drew this highlight
                // after switching back to the paged view.
                render_continuous_page(&reader, page);
            }
        });
        picture.add_controller(drag);
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
        zoom_in.connect_clicked(move |_| {
            {
                let mut r = reader.borrow_mut();
                r.zoom = (r.zoom * 1.25).min(4.0);
            }
            render();
            rebuild_continuous_view_for_zoom(&state, &widgets, &reader, &pdf_hash, &continuous_box);
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
        zoom_out.connect_clicked(move |_| {
            {
                let mut r = reader.borrow_mut();
                r.zoom = (r.zoom / 1.25).max(0.35);
            }
            render();
            rebuild_continuous_view_for_zoom(&state, &widgets, &reader, &pdf_hash, &continuous_box);
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
        let page_label = page_label.clone();
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
                page_label.set_text(&format!("Page {} of {}", page + 1, r.count));
                prev.set_sensitive(page > 0);
                next.set_sensitive(page + 1 < r.count);
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
        continuous_toggle.connect_toggled(move |btn| {
            if btn.is_active() {
                build_continuous_view(&state, &widgets, &reader, &pdf_hash, &continuous_box);
                view_stack.set_visible_child_name("continuous");
                let page = reader.borrow().page;
                scroll_continuous_to_page(&reader, &continuous_scroll, page);
            } else {
                view_stack.set_visible_child_name("paged");
                render();
            }
        });
    }
    {
        let reader = reader.clone();
        let hint = hint.clone();
        mode_drop.connect_selected_notify(move |drop| {
            let idx = drop.selected() as usize;
            let Some((_, kind)) = DRAW_KIND_OPTIONS.get(idx) else {
                return;
            };
            reader.borrow_mut().draw_kind = *kind;
            let text = match kind {
                fond_bib::AnnotationKind::Highlight => "Drag over text to highlight it",
                fond_bib::AnnotationKind::Underline => "Drag over text to underline it",
                fond_bib::AnnotationKind::Strikeout => "Drag over text to strike it out",
                fond_bib::AnnotationKind::Note => "Drag over the page",
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
        note_button.connect_clicked(move |_| {
            show_pdf_note_dialog(&state, &widgets, &reader, &pdf_hash);
        });
    }
    {
        // Content is page-dependent and the page changes constantly while reading, so it's
        // rebuilt fresh on every `show` rather than once at reader-open time (unlike
        // Contents/TOC below, which is fixed for the life of the reader).
        let popover = gtk4::Popover::new();
        page_annots_button.set_popover(Some(&popover));
        let state = state.clone();
        let widgets = widgets.clone();
        let reader = reader.clone();
        let render = render.clone();
        popover.connect_show(move |popover| {
            let current_page = reader.borrow().page as u32 + 1;
            let mut this_page: Vec<fond_bib::Annotation> = reader
                .borrow()
                .annotations
                .annotations
                .iter()
                .filter(|a| a.page == Some(current_page))
                .cloned()
                .collect();
            this_page.sort_by(|a, b| a.created.cmp(&b.created));

            // Built directly (not via `popover_menu`, which bundles its own throwaway
            // `Popover` we'd only discard) — same row-box margins/width, wrapped in our own
            // scroller attached to the real, persistent `popover` this closure was given.
            let rows = gtk4::Box::new(Orientation::Vertical, 2);
            rows.set_margin_top(6);
            rows.set_margin_bottom(6);
            rows.set_margin_start(6);
            rows.set_margin_end(6);
            rows.set_width_request(260);
            if this_page.is_empty() {
                let label = gtk4::Label::new(Some("No annotations on this page"));
                label.add_css_class("dim-label");
                label.set_margin_top(6);
                label.set_margin_bottom(6);
                rows.append(&label);
            }
            let last = this_page.len().saturating_sub(1);
            for (i, annotation) in this_page.into_iter().enumerate() {
                let row_box = gtk4::Box::new(Orientation::Horizontal, 6);
                let label_text = format!(
                    "{:?}{}",
                    annotation.kind,
                    annotation
                        .note
                        .as_deref()
                        .map(|n| format!(" — {n}"))
                        .unwrap_or_default()
                );
                let label = gtk4::Label::new(Some(&label_text));
                label.set_xalign(0.0);
                label.set_hexpand(true);
                label.set_wrap(true);
                label.set_max_width_chars(28);
                row_box.append(&label);

                let delete_button = gtk4::Button::from_icon_name("user-trash-symbolic");
                delete_button.add_css_class("flat");
                delete_button.set_tooltip_text(Some("Delete this annotation"));
                {
                    let state = state.clone();
                    let widgets = widgets.clone();
                    let reader = reader.clone();
                    let render = render.clone();
                    let popover = popover.clone();
                    let id = annotation.id.clone();
                    delete_button.connect_clicked(move |_| {
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
                                let page = reader.borrow().page;
                                render_continuous_page(&reader, page);
                                toast(&widgets, "Annotation deleted");
                            }
                            Some(Err(e)) => toast(&widgets, &format!("Could not delete: {e}")),
                            None => toast(&widgets, "No open library"),
                        }
                        popover.popdown();
                    });
                }
                row_box.append(&delete_button);
                rows.append(&row_box);
                if i != last {
                    rows.append(&popover_separator());
                }
            }
            let scroller = gtk4::ScrolledWindow::new();
            scroller.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
            scroller.set_propagate_natural_height(true);
            scroller.set_max_content_height(360);
            scroller.set_child(Some(&rows));
            popover.set_child(Some(&scroller));
        });
    }
    if let Some(contents_button) = &contents_button {
        let (popover, rows) = popover_menu(260);
        let last = outline_entries.len().saturating_sub(1);
        for (i, entry) in outline_entries.iter().enumerate() {
            // Indent by depth with a plain prefix rather than margins — simplest way to show
            // nesting in a flat popover row list.
            let label = format!("{}{}", "    ".repeat(entry.depth as usize), entry.title);
            let row = popover_button(&label, false);
            if let Some(page) = entry.page {
                let popover = popover.clone();
                let reader = reader.clone();
                let render = render.clone();
                let continuous_toggle = continuous_toggle.clone();
                let continuous_scroll = continuous_scroll.clone();
                row.connect_clicked(move |_| {
                    popover.popdown();
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
            if i != last {
                rows.append(&popover_separator());
            }
        }
        contents_button.set_popover(Some(&popover));
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
                    toast(&widgets, "Note added");
                    dialog.close();
                }
                Some(Err(e)) => toast(&widgets, &format!("Could not save note: {e}")),
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
    mark.style.backgroundColor = 'rgba(246, 195, 68, 0.35)';
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
    }));

    let dialog = adw::Window::new();
    dialog.set_title(Some(title));
    dialog.set_transient_for(Some(window));
    dialog.set_default_size(900, 820);

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

    let hint = gtk4::Label::new(Some("Select text, then click Highlight"));
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    hint.set_margin_top(4);
    hint.set_margin_bottom(4);

    let content = gtk4::Box::new(Orientation::Vertical, 0);
    content.append(&hint);
    content.append(&web_view);

    // pack_end order is the reverse of visual order — Highlight is packed first so it ends
    // up rightmost, Contents to its left (same gotcha CLAUDE.md notes for the hamburger menu).
    let highlight_button = gtk4::Button::with_label("Highlight");
    highlight_button.set_tooltip_text(Some("Highlight the selected text"));
    header.pack_end(&highlight_button);

    if !book.toc.is_empty() {
        let contents_button = gtk4::MenuButton::builder().label("Contents").build();
        let (popover, rows) = popover_menu(260);
        let last = book.toc.len().saturating_sub(1);
        for (i, entry) in book.toc.iter().enumerate() {
            let row = popover_button(&entry.label, false);
            {
                let popover = popover.clone();
                let reader = reader.clone();
                let view = web_view.clone();
                let prev = prev.clone();
                let next = next.clone();
                let chapter_label = chapter_label.clone();
                let target = entry.target.clone();
                row.connect_clicked(move |_| {
                    popover.popdown();
                    epub_go_to(&reader, &view, &prev, &next, &chapter_label, &target);
                });
            }
            rows.append(&row);
            if i != last {
                rows.append(&popover_separator());
            }
        }
        contents_button.set_popover(Some(&popover));
        header.pack_end(&contents_button);
    }
    view.add_top_bar(&header);
    view.set_content(Some(&content));
    dialog.set_content(Some(&view));

    // Re-apply saved highlights after every chapter load (initial load, TOC jump, prev/next
    // — all funnel through `epub_go_to`'s `load_uri`, so one handler here covers all of
    // them), scrolling to `start_annotation_id`'s highlight the first time only.
    {
        let reader = reader.clone();
        let scroll_once = Rc::new(RefCell::new(start_annotation_id.map(|s| s.to_string())));
        web_view.connect_load_changed(move |view, event| {
            if event == webkit6::LoadEvent::Finished {
                let scroll_to = scroll_once.borrow_mut().take();
                epub_apply_highlights(view, &reader, scroll_to.as_deref());
            }
        });
    }

    // Load the first chapter up front (the TOC/prev/next handlers all reuse this same
    // navigation path for consistency, but chapter 0 has to start somewhere).
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
        highlight_button.connect_clicked(move |_| {
            let reader = reader.clone();
            let view_for_apply = view.clone();
            let state = state.clone();
            let widgets = widgets.clone();
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

                    let annotation = fond_bib::Annotation::drawn_epub(
                        fond_bib::AnnotationKind::Highlight,
                        chapter,
                        snippet,
                        capture.prefix,
                        capture.suffix,
                        None,
                    );
                    let id = annotation.id.clone();
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
                            toast(&widgets, "Highlight added");
                        }
                        Some(Err(e)) => toast(&widgets, &format!("Could not save highlight: {e}")),
                        None => toast(&widgets, "No open library — highlight not saved"),
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
/// The structured citation editor: a small form over the common bibliographic fields
/// (type, title, authors, year, publisher, DOI, ISBN). It edits only those fields — every
/// other field on the entry is preserved (see `Library::edit_fields`). On save it rewrites
/// the entry, rebuilds the search index, and refreshes the detail panel.
fn show_citation_editor(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, key: &str) {
    let current = {
        let s = state.borrow();
        let Some(lib) = s.library.as_ref() else {
            return;
        };
        match lib.load_entry(key) {
            Ok(parsed) => fond_bib::entry::read_fields(&parsed.entry),
            Err(e) => {
                drop(s);
                toast(widgets, &format!("Could not read entry: {e}"));
                return;
            }
        }
    };

    // Type choices: the shared ITEM_TYPES list, plus the entry's own type appended if it is
    // something not in that list (so an exotic type round-trips instead of being changed).
    let mut types: Vec<(String, String)> = ITEM_TYPES
        .iter()
        .map(|(l, t)| (l.to_string(), t.to_string()))
        .collect();
    if !current.entry_type.is_empty() && !types.iter().any(|(_, t)| t == &current.entry_type) {
        types.push((current.entry_type.clone(), current.entry_type.clone()));
    }

    let dialog = adw::Window::new();
    dialog.set_title(Some("Edit citation"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(480, -1);

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

    let type_labels: Vec<&str> = types.iter().map(|(l, _)| l.as_str()).collect();
    let type_drop = gtk4::DropDown::from_strings(&type_labels);
    type_drop.set_selected(
        types
            .iter()
            .position(|(_, t)| t == &current.entry_type)
            .unwrap_or(0) as u32,
    );
    let title = gtk4::Entry::builder().text(&current.title).build();
    // Authors: one "Family, Given" per line internally; shown compactly as "; "-separated.
    let authors = gtk4::Entry::builder()
        .text(current.authors.replace('\n', "; "))
        .placeholder_text("Last, First; Last, First")
        .build();
    let year = gtk4::Entry::builder().text(&current.year).build();
    let publisher = gtk4::Entry::builder().text(&current.publisher).build();
    let doi = gtk4::Entry::builder().text(&current.doi).build();
    let isbn = gtk4::Entry::builder().text(&current.isbn).build();

    content.append(&labeled("Type", &type_drop));
    content.append(&labeled("Title", &title));
    content.append(&labeled("Author(s)", &authors));
    content.append(&labeled("Year", &year));
    content.append(&labeled("Publisher", &publisher));
    content.append(&labeled("DOI", &doi));
    content.append(&labeled("ISBN", &isbn));

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
            let entry_type = types
                .get(type_drop.selected() as usize)
                .map(|(_, t)| t.clone())
                .unwrap_or_else(|| current.entry_type.clone());
            let authors_field = authors
                .text()
                .split([';', '\n'])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let edited = fond_bib::entry::EntryFields {
                entry_type,
                title: title.text().trim().to_string(),
                authors: authors_field,
                year: year.text().trim().to_string(),
                publisher: publisher.text().trim().to_string(),
                doi: doi.text().trim().to_string(),
                isbn: isbn.text().trim().to_string(),
            };

            let result = {
                let s = state.borrow();
                s.library.as_ref().map(|lib| lib.edit_fields(&key, &edited))
            };
            match result {
                Some(Ok(())) => {
                    rebuild_index_silent(&state);
                    reload_current(&state, &widgets);
                    select_key(&state, &widgets, &key);
                    toast(&widgets, "Citation updated");
                    dialog.close();
                }
                Some(Err(e)) => toast(&widgets, &format!("Could not save: {e}")),
                None => {}
            }
        });
    }

    dialog.present();
}

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

    let tags = gtk4::Entry::builder()
        .text(note.frontmatter.tags.join(", "))
        .placeholder_text("comma, separated, tags")
        .build();
    content.append(&labeled("Tags", &tags));

    let meta_row = gtk4::Box::new(Orientation::Horizontal, 12);
    let status = gtk4::DropDown::from_strings(&["(none)", "unread", "reading", "read"]);
    status.set_selected(match note.frontmatter.read_status {
        None => 0,
        Some(fond_bib::ReadStatus::Unread) => 1,
        Some(fond_bib::ReadStatus::Reading) => 2,
        Some(fond_bib::ReadStatus::Read) => 3,
    });
    let rating = gtk4::DropDown::from_strings(&["(none)", "1", "2", "3", "4", "5"]);
    rating.set_selected(note.frontmatter.rating.map(|r| r as u32).unwrap_or(0));
    meta_row.append(&labeled("Status", &status));
    meta_row.append(&labeled("Rating", &rating));
    content.append(&meta_row);

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
            let mut updated = note.clone();
            updated.frontmatter.tags = tags
                .text()
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
            updated.frontmatter.read_status = match status.selected() {
                1 => Some(fond_bib::ReadStatus::Unread),
                2 => Some(fond_bib::ReadStatus::Reading),
                3 => Some(fond_bib::ReadStatus::Read),
                _ => None,
            };
            updated.frontmatter.rating = match rating.selected() {
                n @ 1..=5 => Some(n as u8),
                _ => None,
            };

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
                Err(e) => toast(&widgets, &format!("Could not save note: {e}")),
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
            Err(e) => toast(&widgets, &format!("Could not delete {key}: {e}")),
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
            Err(e) => toast(&widgets, &format!("Could not delete {slug}: {e}")),
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
                Some(Err(e)) => toast(&widgets, &format!("Could not save node: {e}")),
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

/// The detail panel's Tags row, grouped by facet (`docs/M2-SPEC.md` §2's `facet:value`
/// convention — the same one `fond-index`'s search already scopes by `facet:`). Each facet
/// gets its own small caption and a wrapped row of chips; plain (unfaceted) tags are their
/// own trailing group with no caption. A flat comma list read fine at three tags; it stopped
/// scanning as soon as facets and plain topical tags were mixed in the same string.
fn tags_row(tags: &[String]) -> gtk4::Box {
    let row = gtk4::Box::new(Orientation::Horizontal, 10);
    let name_label = gtk4::Label::new(Some("Tags"));
    name_label.add_css_class("dim-label");
    name_label.set_xalign(1.0);
    name_label.set_width_chars(13);
    name_label.set_valign(gtk4::Align::Start);
    row.append(&name_label);

    let groups = gtk4::Box::new(Orientation::Vertical, 6);
    groups.set_hexpand(true);

    // Group into (facet, values), preserving first-seen facet order; unfaceted tags collect
    // into their own trailing, caption-less group.
    let mut faceted: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut plain: Vec<&str> = Vec::new();
    for tag in tags {
        match fond_bib::split_facet(tag) {
            (Some(facet), value) => match faceted.iter_mut().find(|(f, _)| *f == facet) {
                Some((_, values)) => values.push(value),
                None => faceted.push((facet, vec![value])),
            },
            (None, value) => plain.push(value),
        }
    }
    faceted.sort_by_key(|(facet, _)| *facet);

    for (facet, values) in &faceted {
        groups.append(&chip_group(Some(facet), values));
    }
    if !plain.is_empty() {
        groups.append(&chip_group(None, &plain));
    }

    row.append(&groups);
    row
}

/// One facet's worth of tag chips: an optional small caption, then a wrapped flow of chips.
fn chip_group(facet: Option<&str>, values: &[&str]) -> gtk4::Box {
    let col = gtk4::Box::new(Orientation::Vertical, 2);
    if let Some(facet) = facet {
        let caption = gtk4::Label::new(Some(facet));
        caption.add_css_class("dim-label");
        caption.add_css_class("caption");
        caption.set_xalign(0.0);
        col.append(&caption);
    }
    let flow = gtk4::FlowBox::new();
    flow.set_selection_mode(gtk4::SelectionMode::None);
    flow.set_row_spacing(4);
    flow.set_column_spacing(4);
    flow.set_homogeneous(false);
    flow.set_max_children_per_line(u32::MAX);
    for value in values {
        let chip = gtk4::Label::new(Some(value));
        chip.add_css_class("tag-chip");
        flow.insert(&chip, -1);
    }
    col.append(&flow);
    col
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
