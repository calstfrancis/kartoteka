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
        .menu_model(&build_menu())
        .tooltip_text("Main menu")
        .build();
    header.pack_end(&menu_button);

    let reload_button = gtk4::Button::from_icon_name("view-refresh-symbolic");
    reload_button.set_tooltip_text(Some("Reload library"));
    header.pack_end(&reload_button);

    toolbar_view.add_top_bar(&header);

    // Collections pane (leftmost): "All entries" + one row per collection, with a + to
    // create a new one.
    let collections_box = gtk4::Box::new(Orientation::Vertical, 0);
    collections_box.set_width_request(190);
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
    collections_listbox.add_css_class("navigation-sidebar");
    let coll_scroll = gtk4::ScrolledWindow::new();
    coll_scroll.set_child(Some(&collections_listbox));
    coll_scroll.set_vexpand(true);
    collections_box.append(&coll_header);
    collections_box.append(&coll_scroll);

    // Sidebar: search entry over a scrolled list.
    let sidebar = gtk4::Box::new(Orientation::Vertical, 0);
    sidebar.set_width_request(300);
    let search = gtk4::SearchEntry::new();
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
    listbox.add_css_class("navigation-sidebar");
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

fn build_menu() -> gio::Menu {
    let menu = gio::Menu::new();

    let actions = gio::Menu::new();
    actions.append(Some("Cite…"), Some("win.cite"));
    actions.append(Some("New item…"), Some("win.new-item"));
    actions.append(Some("Acquire…"), Some("win.acquire"));
    actions.append(Some("Add PDF…"), Some("win.add-pdf"));
    actions.append(Some("Add folder of PDFs…"), Some("win.add-folder"));
    actions.append(Some("Add from URL…"), Some("win.add-url"));
    actions.append(Some("Import…"), Some("win.import"));
    actions.append(Some("Export bibliography…"), Some("win.export-bib"));
    actions.append(Some("Find duplicates…"), Some("win.duplicates"));
    actions.append(Some("Manage tags…"), Some("win.tags"));
    actions.append(Some("Nodes…"), Some("win.nodes"));
    menu.append_section(None, &actions);

    let library = gio::Menu::new();
    library.append(Some("Save current search…"), Some("win.save-search"));
    library.append(Some("Back up (git commit)…"), Some("win.backup"));
    library.append(Some("Sign in to GitHub…"), Some("win.github-signin"));
    library.append(Some("Back up to WebDAV…"), Some("win.webdav-backup"));
    library.append(Some("Reindex search"), Some("win.reindex"));
    menu.append_section(None, &library);

    let theme = gio::Menu::new();
    theme.append(Some("System"), Some("win.theme::system"));
    theme.append(Some("Light"), Some("win.theme::light"));
    theme.append(Some("Dark"), Some("win.theme::dark"));
    menu.append_submenu(Some("Theme"), &theme);

    let about = gio::Menu::new();
    about.append(Some("About Kartoteka"), Some("win.about"));
    menu.append_section(None, &about);

    menu
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
        fond_index::SearchIndex::rebuild(library, &dir, |_| None)
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
    view.add_top_bar(&adw::HeaderBar::new());

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

/// Worker-thread identification: sniff a DOI from the PDF text, else build a minimal entry
/// from embedded metadata. Returns `(is_bibtex, payload, page_count)`.
fn identify_pdf(path: &std::path::Path) -> Result<(bool, String, Option<u32>), String> {
    let pdfium = fond_doc::bind_pdfium().map_err(|e| format!("PDFium unavailable: {e}"))?;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let pages = fond_doc::page_count(&pdfium, &bytes).ok().map(|n| n as u32);

    if let Ok(text) = fond_doc::extract_text(&pdfium, &bytes) {
        if let Some(doi) = fond_doc::find_doi(&text.full_text()) {
            let bibtex = fond_bib::acquire::fetch_doi_bibtex(&doi).map_err(|e| e.to_string())?;
            return Ok((true, bibtex, pages));
        }
    }

    let meta = fond_doc::extract_metadata(&pdfium, &bytes).map_err(|e| e.to_string())?;
    if let Some(title) = meta.title {
        let yaml = fond_bib::acquire::minimal_book_yaml(&title, meta.author.as_deref())
            .map_err(|e| e.to_string())?;
        return Ok((false, yaml, pages));
    }

    Err("could not identify the PDF (no DOI in text, no embedded title)".to_string())
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
    let yaml = build_entry_yaml(
        &meta.entry_type,
        &meta.title,
        &meta.authors.join("; "),
        &meta.year,
        &meta.container,
        &meta.publisher,
        &meta.doi,
        &meta.isbn,
        url,
    );

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

/// Build a one-entry Hayagriva YAML snippet from the manual form. The placeholder key is
/// replaced with a generated one by `add_from_yaml`. `authors` is split on `;`/newlines.
#[allow(clippy::too_many_arguments)]
fn build_entry_yaml(
    ty: &str,
    title: &str,
    authors: &str,
    year: &str,
    container: &str,
    publisher: &str,
    doi: &str,
    isbn: &str,
    url: &str,
) -> String {
    let mut out = String::from("new-item:\n");
    out.push_str(&format!("  type: {ty}\n"));
    if !title.trim().is_empty() {
        out.push_str(&format!("  title: {}\n", yaml_quote(title.trim())));
    }
    let names: Vec<&str> = authors
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
    if !year.trim().is_empty() {
        // Hayagriva accepts a bare year as a date.
        out.push_str(&format!("  date: {}\n", year.trim()));
    }
    if !publisher.trim().is_empty() {
        out.push_str(&format!("  publisher: {}\n", yaml_quote(publisher.trim())));
    }
    if !url.trim().is_empty() {
        out.push_str(&format!("  url: {}\n", yaml_quote(url.trim())));
    }
    let doi = doi.trim();
    let isbn = isbn.trim();
    if !doi.is_empty() || !isbn.is_empty() {
        out.push_str("  serial-number:\n");
        if !doi.is_empty() {
            out.push_str(&format!("    doi: {}\n", yaml_quote(doi)));
        }
        if !isbn.is_empty() {
            out.push_str(&format!("    isbn: {}\n", yaml_quote(isbn)));
        }
    }
    if !container.trim().is_empty() {
        let parent_ty = match ty {
            "chapter" | "anthology" => "anthology",
            "conference" => "proceedings",
            _ => "periodical",
        };
        out.push_str("  parent:\n");
        out.push_str(&format!("    type: {parent_ty}\n"));
        out.push_str(&format!("    title: {}\n", yaml_quote(container.trim())));
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
    let search = gtk4::SearchEntry::new();
    search.set_placeholder_text(Some("Search to cite (@key → clipboard)"));
    search.set_width_chars(32);
    header.set_title_widget(Some(&search));
    view.add_top_bar(&header);

    let listbox = gtk4::ListBox::new();
    listbox.set_selection_mode(gtk4::SelectionMode::Single);
    listbox.add_css_class("navigation-sidebar");
    let scroll = gtk4::ScrolledWindow::new();
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
                title.add_css_class("heading");
                let meta = gtk4::Label::new(Some(sub));
                meta.set_halign(gtk4::Align::Start);
                meta.set_xalign(0.0);
                meta.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                meta.add_css_class("dim-label");
                meta.add_css_class("caption");
                vbox.append(&title);
                vbox.append(&meta);
                let row = gtk4::ListBoxRow::new();
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
            let yaml = build_entry_yaml(
                ty,
                &title.text(),
                &authors.text(),
                &year.text(),
                &container.text(),
                &publisher.text(),
                &doi.text(),
                &isbn.text(),
                &url.text(),
            );
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
    view.add_top_bar(&adw::HeaderBar::new());

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
    let index = if index_dir.join("meta.json").exists() {
        fond_index::SearchIndex::open(&index_dir).ok()
    } else {
        match fond_index::SearchIndex::rebuild(&library, &index_dir, |_| None) {
            Ok(idx) => Some(idx),
            Err(e) => {
                toast(widgets, &format!("Search index unavailable: {e}"));
                None
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
    text.set_xalign(0.0);
    text.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    hbox.append(&image);
    hbox.append(&text);
    let row = gtk4::ListBoxRow::new();
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
    view.add_top_bar(&adw::HeaderBar::new());

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
    view.add_top_bar(&adw::HeaderBar::new());

    let list = gtk4::Box::new(Orientation::Vertical, 6);
    list.set_margin_top(14);
    list.set_margin_bottom(14);
    list.set_margin_start(16);
    list.set_margin_end(16);

    for (tag, count) in &tags {
        let row = gtk4::Box::new(Orientation::Horizontal, 8);
        let entry = gtk4::Entry::builder().text(tag).hexpand(true).build();
        let count_label = gtk4::Label::new(Some(&format!("{count}")));
        count_label.add_css_class("dim-label");
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
        row.append(&entry);
        row.append(&count_label);
        row.append(&apply);
        list.append(&row);
    }

    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_child(Some(&list));
    scroll.set_vexpand(true);
    view.set_content(Some(&scroll));
    dialog.set_content(Some(&view));
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
    let scroll = gtk4::ScrolledWindow::new();
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
                let m = gtk4::Label::new(Some(&sub));
                m.set_halign(gtk4::Align::Start);
                m.set_xalign(0.0);
                m.add_css_class("dim-label");
                m.add_css_class("caption");
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
        for &idx in &s.visible {
            let e = &s.entries[idx];
            widgets.listbox.append(&make_row(e));
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
    title.add_css_class("heading");

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
    meta.add_css_class("dim-label");
    meta.add_css_class("caption");

    vbox.append(&title);
    vbox.append(&meta);

    let row = gtk4::ListBoxRow::new();
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

    // First present PDF attachment (for the Open button).
    let present_pdf = note.as_ref().and_then(|n| {
        n.frontmatter.attachments.iter().find_map(|att| {
            let hex = att
                .hash
                .split_once(':')
                .map(|(_, h)| h)
                .unwrap_or(&att.hash);
            let path = library.attachment_blob_path(hex);
            path.exists().then(|| (path, att.filename.clone()))
        })
    });

    let doi = library
        .load_entry(&key)
        .ok()
        .and_then(|p| p.entry.doi().map(|d| d.to_string()));

    // Action row: edit note, open PDF.
    let actions = gtk4::Box::new(Orientation::Horizontal, 8);
    actions.set_margin_top(6);
    let edit_button = gtk4::Button::with_label("Edit note");
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let key = key.clone();
        edit_button.connect_clicked(move |_| show_note_editor(&state, &widgets, &key));
    }
    actions.append(&edit_button);
    let cite_button = gtk4::Button::with_label("Cite");
    cite_button.set_tooltip_text(Some("Copy the Typst citation (@key)"));
    {
        let widgets = widgets.clone();
        let key = key.clone();
        cite_button.connect_clicked(move |_| copy_citation(&widgets, &key));
    }
    actions.append(&cite_button);
    if let Some((path, filename)) = present_pdf {
        let read_button = gtk4::Button::with_label("Read");
        read_button.set_tooltip_text(Some("Open the built-in PDF reader"));
        {
            let window = widgets.window.clone();
            let path = path.clone();
            let title = title_text.to_string();
            read_button.connect_clicked(move |_| show_pdf_reader(&window, &path, &title));
        }
        actions.append(&read_button);
        let open_button = gtk4::Button::with_label("Open externally");
        let window = widgets.window.clone();
        open_button.connect_clicked(move |_| open_pdf(&window, &path, &filename));
        actions.append(&open_button);
    } else if let Some(doi) = doi.clone() {
        // No PDF yet, but we have a DOI — offer an Unpaywall lookup.
        let find_button = gtk4::Button::with_label("Find PDF");
        find_button.set_tooltip_text(Some("Search Unpaywall for an open-access PDF"));
        let state = state.clone();
        let widgets = widgets.clone();
        let key = key.clone();
        find_button.connect_clicked(move |_| find_pdf_unpaywall(&state, &widgets, &key, &doi));
        actions.append(&find_button);
    }
    let collect_button = gtk4::Button::with_label("Collections…");
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let key = key.clone();
        collect_button.connect_clicked(move |_| membership_dialog(&state, &widgets, &key));
    }
    actions.append(&collect_button);
    let related_button = gtk4::Button::with_label("Relations…");
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let key = key.clone();
        related_button.connect_clicked(move |_| relations_dialog(&state, &widgets, &key));
    }
    actions.append(&related_button);
    // Author → node: create/link a person node for each author (feature §1 author IDs).
    if !summary.author.is_empty() {
        let author_button = gtk4::Button::with_label("Link author…");
        author_button.set_tooltip_text(Some("Create or link a person node for each author"));
        let state = state.clone();
        let widgets = widgets.clone();
        let key = key.clone();
        author_button.connect_clicked(move |_| link_authors_dialog(&state, &widgets, &key));
        actions.append(&author_button);
    }
    // "Locate" menu: open DOI / on the web (feature 10).
    let locate = gtk4::MenuButton::builder().label("Locate").build();
    {
        let doi = doi.clone();
        let title_q = summary.title.clone();
        let menu = gio::Menu::new();
        if doi.is_some() {
            menu.append(Some("Open DOI"), Some("locate.doi"));
        }
        menu.append(Some("Google Scholar"), Some("locate.scholar"));
        let group = gio::SimpleActionGroup::new();
        if let Some(doi) = doi {
            let a = gio::SimpleAction::new("doi", None);
            let window = widgets.window.clone();
            a.connect_activate(move |_, _| open_uri(&window, &format!("https://doi.org/{doi}")));
            group.add_action(&a);
        }
        {
            let a = gio::SimpleAction::new("scholar", None);
            let window = widgets.window.clone();
            a.connect_activate(move |_, _| {
                let q = urlencode(&title_q);
                open_uri(
                    &window,
                    &format!("https://scholar.google.com/scholar?q={q}"),
                );
            });
            group.add_action(&a);
        }
        locate.insert_action_group("locate", Some(&group));
        locate.set_menu_model(Some(&menu));
    }
    actions.append(&locate);
    b.append(&actions);

    let fields = gtk4::Box::new(Orientation::Vertical, 4);
    fields.set_margin_top(8);

    // Structured fields from the entry.
    if let Ok(parsed) = library.load_entry(&key) {
        let e = &parsed.entry;
        fields.append(&field_row(
            "Type",
            &format!("{:?}", e.entry_type()).to_lowercase(),
        ));
        fields.append(&field_row("Key", &key));
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
            fields.append(&field_row("Tags", &note.frontmatter.tags.join(", ")));
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
    b.append(&fields);

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

/// Live state of an open PDF reader window.
struct ReaderState {
    pdfium: fond_doc::Pdfium,
    bytes: Vec<u8>,
    page: u16,
    count: u16,
    /// Render width in px = `BASE_WIDTH * zoom`.
    zoom: f64,
}

const READER_BASE_WIDTH: f64 = 820.0;

/// A built-in PDF reader: renders pages with PDFium to RGBA textures, with page navigation
/// and zoom. No Poppler (GPL) — pure PDFium (BSD), the same binding used for text extraction.
fn show_pdf_reader(window: &adw::ApplicationWindow, blob: &std::path::Path, title: &str) {
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

    let reader = Rc::new(RefCell::new(ReaderState {
        pdfium,
        bytes,
        page: 0,
        count,
        zoom: 1.0,
    }));

    let dialog = adw::Window::new();
    dialog.set_title(Some(title));
    dialog.set_transient_for(Some(window));
    dialog.set_default_size(900, 820);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();

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
    header.pack_end(&zoom_in);
    header.pack_end(&zoom_out);
    view.add_top_bar(&header);

    let picture = gtk4::Picture::new();
    picture.set_halign(gtk4::Align::Center);
    picture.set_valign(gtk4::Align::Start);
    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_child(Some(&picture));
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    view.set_content(Some(&scroll));
    dialog.set_content(Some(&view));

    // Render the current page into the Picture and refresh the page label.
    let render = {
        let reader = reader.clone();
        let picture = picture.clone();
        let page_label = page_label.clone();
        let prev = prev.clone();
        let next = next.clone();
        Rc::new(move || {
            let r = reader.borrow();
            let width = (READER_BASE_WIDTH * r.zoom) as u32;
            match fond_doc::render_page(&r.pdfium, &r.bytes, r.page, width) {
                Ok(rp) => {
                    let data = glib::Bytes::from(&rp.rgba);
                    let texture = gdk::MemoryTexture::new(
                        rp.width as i32,
                        rp.height as i32,
                        gdk::MemoryFormat::R8g8b8a8,
                        &data,
                        (rp.width * 4) as usize,
                    );
                    picture.set_paintable(Some(&texture));
                    picture.set_size_request(rp.width as i32, rp.height as i32);
                }
                Err(_) => picture.set_paintable(gdk::Paintable::NONE),
            }
            page_label.set_text(&format!("Page {} of {}", r.page + 1, r.count));
            prev.set_sensitive(r.page > 0);
            next.set_sensitive(r.page + 1 < r.count);
        })
    };
    render();

    {
        let reader = reader.clone();
        let render = render.clone();
        prev.connect_clicked(move |_| {
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
        next.connect_clicked(move |_| {
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
        let reader = reader.clone();
        let render = render.clone();
        zoom_in.connect_clicked(move |_| {
            {
                let mut r = reader.borrow_mut();
                r.zoom = (r.zoom * 1.25).min(4.0);
            }
            render();
        });
    }
    {
        let reader = reader.clone();
        let render = render.clone();
        zoom_out.connect_clicked(move |_| {
            {
                let mut r = reader.borrow_mut();
                r.zoom = (r.zoom / 1.25).max(0.35);
            }
            render();
        });
    }

    dialog.present();
}

/// Edit an entry's note: tags, read status, rating, and prose. Writes `notes/<key>.md`.
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
fn rebuild_index_silent(state: &Rc<RefCell<AppState>>) {
    let rebuilt = {
        let s = state.borrow();
        s.library.as_ref().map(|lib| {
            let dir = lib.root().join(".kartoteka").join("index");
            fond_index::SearchIndex::rebuild(lib, &dir, |_| None)
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
    listbox.add_css_class("navigation-sidebar");
    listbox.set_selection_mode(gtk4::SelectionMode::Single);
    let scroll = gtk4::ScrolledWindow::new();
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
                let b = gtk4::Box::new(Orientation::Vertical, 2);
                b.set_margin_top(6);
                b.set_margin_bottom(6);
                b.set_margin_start(8);
                b.set_margin_end(8);
                let title = gtk4::Label::new(Some(&fm.label));
                title.add_css_class("heading");
                title.set_xalign(0.0);
                title.set_halign(gtk4::Align::Start);
                let sub = gtk4::Label::new(Some(&format!(
                    "{} · {}",
                    node_type_label(fm.node_type),
                    slug
                )));
                sub.add_css_class("dim-label");
                sub.add_css_class("caption");
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
        new_btn.connect_clicked(move |_| show_node_editor(&state, &widgets, None, populate.clone()));
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
                s.library.as_ref().map(|lib| lib.write_node(&target_slug, &node))
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
    name_label.set_width_chars(11);
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
