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
    status_label: gtk4::Label,
    listbox: gtk4::ListBox,
    detail: gtk4::Box,
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

    let paned = gtk4::Paned::new(Orientation::Horizontal);
    paned.set_start_child(Some(&sidebar));
    paned.set_end_child(Some(&detail_scroll));
    paned.set_resize_start_child(false);
    paned.set_position(320);

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
    actions.append(Some("Acquire…"), Some("win.acquire"));
    actions.append(Some("Add PDF…"), Some("win.add-pdf"));
    actions.append(Some("Import…"), Some("win.import"));
    actions.append(Some("Export bibliography…"), Some("win.export-bib"));
    menu.append_section(None, &actions);

    let library = gio::Menu::new();
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
        let action = gio::SimpleAction::new("add-pdf", None);
        action.connect_activate(move |_, _| show_add_pdf(&state, &widgets));
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
        let action = gio::SimpleAction::new("import", None);
        action.connect_activate(move |_, _| show_import_dialog(&state, &widgets));
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
    widgets.status_label.set_text(&path.display().to_string());
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
    if let Some((path, filename)) = present_pdf {
        let open_button = gtk4::Button::with_label("Open PDF");
        let window = widgets.window.clone();
        open_button.connect_clicked(move |_| open_pdf(&window, &path, &filename));
        actions.append(&open_button);
    }
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
