//! The main application window: a sidebar list of entries with a live filter, and a detail
//! pane showing the selected entry's YAML and note. All data comes from `fond-bib`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{gio, glib, Orientation};
use libadwaita as adw;
use libadwaita::prelude::*;

use fond_bib::{entry as bibentry, Library};

use crate::config::Config;

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
}

struct Widgets {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    subtitle: adw::WindowTitle,
    listbox: gtk4::ListBox,
    detail: gtk4::TextView,
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

    // Sidebar: search entry over a scrolled list.
    let sidebar = gtk4::Box::new(Orientation::Vertical, 0);
    sidebar.set_width_request(320);
    let search = gtk4::SearchEntry::new();
    search.set_placeholder_text(Some("Search — author: title: tag: type: year:"));
    search.set_margin_top(6);
    search.set_margin_bottom(6);
    search.set_margin_start(6);
    search.set_margin_end(6);
    let listbox = gtk4::ListBox::new();
    listbox.set_selection_mode(gtk4::SelectionMode::Single);
    listbox.add_css_class("navigation-sidebar");
    let list_scroll = gtk4::ScrolledWindow::new();
    list_scroll.set_child(Some(&listbox));
    list_scroll.set_vexpand(true);
    sidebar.append(&search);
    sidebar.append(&list_scroll);

    // Detail pane.
    let detail = gtk4::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .left_margin(12)
        .right_margin(12)
        .top_margin(12)
        .bottom_margin(12)
        .build();
    let detail_scroll = gtk4::ScrolledWindow::new();
    detail_scroll.set_child(Some(&detail));
    detail_scroll.set_hexpand(true);
    detail_scroll.set_vexpand(true);

    let paned = gtk4::Paned::new(Orientation::Horizontal);
    paned.set_start_child(Some(&sidebar));
    paned.set_end_child(Some(&detail_scroll));
    paned.set_resize_start_child(false);
    paned.set_position(320);

    toolbar_view.set_content(Some(&paned));

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&toolbar_view));
    window.set_content(Some(&toasts));

    let widgets = Rc::new(Widgets {
        window: window.clone(),
        toasts,
        subtitle: title,
        listbox: listbox.clone(),
        detail,
    });

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
    actions.append(Some("Acquire…"), Some("win.acquire"));
    actions.append(Some("Reindex search"), Some("win.reindex"));
    menu.append_section(None, &actions);

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
    let s = state.borrow();
    let Some(library) = s.library.as_ref() else {
        toast(widgets, "Open a library first");
        return;
    };
    let dir = library.root().join(".kartoteka").join("index");
    match fond_index::SearchIndex::rebuild(library, &dir, |_| None) {
        Ok(_) => toast(widgets, "Search index rebuilt"),
        Err(e) => toast(widgets, &format!("Reindex failed: {e}")),
    }
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

    // Build the full-text index for field-scoped search (metadata + notes + annotations;
    // PDF text is added by the CLI `reindex`). Failure is non-fatal — search falls back to
    // a substring filter.
    let index_dir = library.root().join(".kartoteka").join("index");
    let index = match fond_index::SearchIndex::rebuild(&library, &index_dir, |_| None) {
        Ok(idx) => Some(idx),
        Err(e) => {
            toast(widgets, &format!("Search index unavailable: {e}"));
            None
        }
    };

    {
        let mut s = state.borrow_mut();
        s.library = Some(library);
        s.entries = entries;
        s.key_to_index = key_to_index;
        s.index = index;
        s.query.clear();
    }
    widgets.subtitle.set_subtitle(&format!(
        "{} — {count} entries",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("library")
    ));
    refresh_list(state, widgets);
}

fn refresh_list(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    // Recompute the visible set: empty query → all; otherwise the tantivy index (field
    // scoping: author: title: tag: type: year:), falling back to a substring match if the
    // index is absent or the query doesn't parse.
    {
        let mut s = state.borrow_mut();
        let query = s.query.trim().to_string();
        let visible: Vec<usize> = if query.is_empty() {
            (0..s.entries.len()).collect()
        } else {
            let index_hits = s
                .index
                .as_ref()
                .and_then(|idx| idx.search(&query, 1000).ok());
            match index_hits {
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
            }
        };
        s.visible = visible;
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
        widgets.detail.buffer().set_text("");
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
    let key = &s.entries[entry_idx].key;

    let mut text = library.read_entry_raw(key).unwrap_or_default();
    if let Ok(Some(note)) = library.load_note(key) {
        if !note.body.trim().is_empty() || !note.frontmatter.tags.is_empty() {
            text.push_str("\n--- note ---\n");
            if !note.frontmatter.tags.is_empty() {
                text.push_str(&format!("tags: {}\n\n", note.frontmatter.tags.join(", ")));
            }
            text.push_str(note.body.trim());
            text.push('\n');
        }
    }
    widgets.detail.buffer().set_text(&text);
}

fn toast(widgets: &Rc<Widgets>, message: &str) {
    widgets.toasts.add_toast(adw::Toast::new(message));
}
