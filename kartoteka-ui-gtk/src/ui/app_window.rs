//! The main application window: a sidebar list of entries with a live filter, and a detail
//! pane showing the selected entry's YAML and note. All data comes from `fond-bib`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{gio, Orientation};
use libadwaita as adw;
use libadwaita::prelude::*;

use fond_bib::{entry as bibentry, Library};

use crate::config::Config;

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

    let reload_button = gtk4::Button::from_icon_name("view-refresh-symbolic");
    reload_button.set_tooltip_text(Some("Reload library"));
    header.pack_end(&reload_button);

    toolbar_view.add_top_bar(&header);

    // Sidebar: search entry over a scrolled list.
    let sidebar = gtk4::Box::new(Orientation::Vertical, 0);
    sidebar.set_width_request(320);
    let search = gtk4::SearchEntry::new();
    search.set_placeholder_text(Some("Filter by author, title, or key"));
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

    // Restore the last-opened library.
    if let Some(path) = config.borrow().library_path.clone() {
        if path.is_dir() {
            open_library(&state, &widgets, path);
        }
    }

    window
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
    {
        let mut s = state.borrow_mut();
        s.library = Some(library);
        s.entries = entries;
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
    // Recompute the visible set.
    {
        let mut s = state.borrow_mut();
        let q = s.query.to_lowercase();
        let visible: Vec<usize> = s
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                q.is_empty()
                    || e.title.to_lowercase().contains(&q)
                    || e.author.to_lowercase().contains(&q)
                    || e.key.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect();
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
