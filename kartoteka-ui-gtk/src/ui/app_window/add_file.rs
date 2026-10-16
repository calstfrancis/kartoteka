//! The "Add this file" dialog — a PDF or EPUB handed over by another app (Pereplyot's
//! "Add to Kartoteka"), added with the person in the loop.
//!
//! The dialog opens at once and reads the file in the background (a DOI or ISBN in its text,
//! else its embedded metadata). Whatever that finds fills the fields below; if it finds nothing,
//! or the wrong thing, the lookup box takes a DOI, ISBN, link, BibTeX record or a title and
//! replaces the fields, and every field can be typed over by hand. Nothing is added until
//! "Add" is pressed, so a bad guess is never filed away unseen.
//!
//! A fetched record keeps its fields the form doesn't show (the abstract, a series, …): the
//! edits are applied on top of it (`apply_fields_to_yaml`) rather than rebuilding the entry
//! from the form.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use fond_bib::acquire::Candidate;
use fond_bib::entry::{apply_fields_to_yaml, read_fields, EntryFields};
use fond_bib::item_kind::{kind_of, retype, ITEM_KINDS};
use fond_bib::Identified;
use gtk4::prelude::*;
use gtk4::Orientation;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::add_box::{result_meta, Lookup, Target};
use super::{
    build_entry_yaml, close_on_escape, epub_entry_yaml, identify_pdf, labeled, reload_current,
    scrape_url, select_key, toast, AppState, NewItemFields, Widgets,
};
use crate::ui::creator_editor::CreatorListEditor;
use crate::ui::{friendly, worker};

/// What to ask for in the background.
enum Request {
    /// Read the file itself.
    File,
    /// Fetch what an identifier names — from a search result, if it was picked from one.
    Target(Target, Option<Candidate>),
    Url(String),
    BibTeX(String),
}

/// A record found: the entry as YAML under a placeholder key, the file's page count if reading
/// the file produced one, and whether only a search result's own details could be had.
struct Fetched {
    record: String,
    pages: Option<u32>,
    partial: bool,
}

/// Why nothing was found: a sentence for the person and, optionally, the technical detail.
type Failure = (String, Option<String>);

const FILE_UNREADABLE: &str = "Couldn't tell what this is from the file itself. Look it up above, \
                               or fill in the details by hand.";

/// The first entry of `entries` as YAML under a placeholder key.
macro_rules! first_record {
    ($entries:expr) => {{
        let entries = $entries;
        match entries.first() {
            Some(entry) => fond_bib::entry::serialize_entry_as(entry, "new-item")
                .map_err(|e| (friendly::bib_error(&e), None)),
            None => Err(("That record had no entries in it.".to_string(), None)),
        }
    }};
}

/// Worker-thread body of every request.
fn fetch(request: Request, path: &Path) -> Result<Fetched, Failure> {
    match request {
        Request::File => {
            let is_epub = path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("epub"));
            let unreadable = |detail: String| (FILE_UNREADABLE.to_string(), Some(detail));
            if is_epub {
                let yaml = epub_entry_yaml(path).map_err(unreadable)?;
                let entries = fond_bib::entry::parse_all(&yaml, Path::new("<record>"))
                    .map_err(|e| unreadable(e.to_string()))?;
                Ok(Fetched {
                    record: first_record!(entries)?,
                    pages: None,
                    partial: false,
                })
            } else {
                let (is_bibtex, payload, pages) = identify_pdf(path).map_err(unreadable)?;
                let entries = if is_bibtex {
                    fond_bib::entry::parse_bibtex(&payload)
                } else {
                    fond_bib::entry::parse_all(&payload, Path::new("<record>"))
                }
                .map_err(|e| unreadable(e.to_string()))?;
                Ok(Fetched {
                    record: first_record!(entries)?,
                    pages,
                    partial: false,
                })
            }
        }
        Request::Target(target, candidate) => match Lookup::run(&target, candidate.as_ref()) {
            Lookup::Found { entries, partial } => Ok(Fetched {
                record: first_record!(entries)?,
                pages: None,
                partial,
            }),
            Lookup::NotFound(detail) => Err((target.failure_message().to_string(), Some(detail))),
            Lookup::Unreadable(e) => Err((friendly::bib_error(&e), None)),
        },
        Request::Url(url) => {
            let scraped = scrape_url(&url).map_err(|e| {
                (
                    "Couldn't get a reference from that page.".to_string(),
                    Some(e),
                )
            })?;
            let entries = fond_bib::entry::parse_all(&scraped.yaml, Path::new("<record>"))
                .map_err(|e| (friendly::bib_error(&e), None))?;
            Ok(Fetched {
                record: first_record!(entries)?,
                pages: None,
                partial: false,
            })
        }
        Request::BibTeX(text) => {
            let entries = fond_bib::entry::parse_bibtex(&text)
                .map_err(|e| (friendly::bib_error(&e), None))?;
            Ok(Fetched {
                record: first_record!(entries)?,
                pages: None,
                partial: false,
            })
        }
    }
}

struct Ui {
    state: Rc<RefCell<AppState>>,
    widgets: Rc<Widgets>,
    path: PathBuf,
    /// Weak, so the dialog can be freed once closed (the dialog owns this struct).
    dialog: glib::WeakRef<adw::Window>,
    add_button: gtk4::Button,
    lookup: gtk4::Entry,
    lookup_button: gtk4::Button,
    hint: gtk4::Label,
    spinner: gtk4::Spinner,
    results_scroll: gtk4::ScrolledWindow,
    results: gtk4::ListBox,
    candidates: RefCell<Vec<Candidate>>,
    type_drop: gtk4::DropDown,
    title: gtk4::Entry,
    creators_holder: gtk4::Box,
    creators: RefCell<CreatorListEditor>,
    year: gtk4::Entry,
    container: gtk4::Entry,
    publisher: gtk4::Entry,
    location: gtk4::Entry,
    doi: gtk4::Entry,
    isbn: gtk4::Entry,
    url: gtk4::Entry,
    volume: gtk4::Entry,
    issue: gtk4::Entry,
    pages: gtk4::Entry,
    /// The record last found, and the fields as they were read from it.
    record: RefCell<Option<(String, EntryFields)>>,
    /// The kind the found record is, so a type left alone is not retyped.
    record_kind: Cell<Option<usize>>,
    /// Page count of the file, when reading it produced one.
    file_pages: Cell<Option<u32>>,
    /// The library entry this file's DOI/ISBN already belongs to, once the person has been told.
    existing: RefCell<Option<String>>,
    busy: Cell<bool>,
}

impl Ui {
    fn close(&self) {
        if let Some(dialog) = self.dialog.upgrade() {
            dialog.close();
        }
    }

    fn say(&self, text: &str, detail: Option<&str>, error: bool) {
        self.hint.set_text(text);
        self.hint.set_tooltip_text(detail);
        if error {
            self.hint.add_css_class("error");
        } else {
            self.hint.remove_css_class("error");
        }
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.spinner.set_spinning(busy);
        self.lookup.set_sensitive(!busy);
        self.lookup_button.set_sensitive(!busy);
        self.results.set_sensitive(!busy);
        self.add_button.set_sensitive(!busy);
    }

    /// The fields as they stand in the form, relative to `base` (the found record's own fields,
    /// or blank): a value that only differs by surrounding space counts as untouched.
    fn edited_fields(&self, base: &EntryFields) -> EntryFields {
        let same = |text: gtk4::glib::GString, was: &str| {
            if text.trim() == was.trim() {
                was.to_string()
            } else {
                text.trim().to_string()
            }
        };
        let selected = self.type_drop.selected() as usize;
        let mut fields = base.clone();
        fields.title = same(self.title.text(), &base.title);
        fields.creators = self.creators.borrow().creators();
        fields.year = same(self.year.text(), &base.year);
        fields.container = same(self.container.text(), &base.container);
        fields.publisher = same(self.publisher.text(), &base.publisher);
        fields.location = same(self.location.text(), &base.location);
        fields.doi = same(self.doi.text(), &base.doi);
        fields.isbn = same(self.isbn.text(), &base.isbn);
        fields.url = same(self.url.text(), &base.url);
        fields.volume = same(self.volume.text(), &base.volume);
        fields.issue = same(self.issue.text(), &base.issue);
        fields.pages = same(self.pages.text(), &base.pages);
        if self.record_kind.get() != Some(selected) {
            if let Some(kind) = ITEM_KINDS.get(selected) {
                retype(&mut fields, kind);
            }
        }
        fields
    }

    /// Put a found record into the form.
    fn fill(&self, fetched: &Fetched) {
        let fields = fond_bib::entry::parse_all(&fetched.record, Path::new("<record>"))
            .ok()
            .and_then(|entries| entries.first().map(read_fields))
            .unwrap_or_default();
        self.title.set_text(&fields.title);
        self.year.set_text(&fields.year);
        self.container.set_text(&fields.container);
        self.publisher.set_text(&fields.publisher);
        self.location.set_text(&fields.location);
        self.doi.set_text(&fields.doi);
        self.isbn.set_text(&fields.isbn);
        self.url.set_text(&fields.url);
        self.volume.set_text(&fields.volume);
        self.issue.set_text(&fields.issue);
        self.pages.set_text(&fields.pages);

        let kind = kind_of(&fields);
        if let Some(i) = kind {
            self.type_drop.set_selected(i as u32);
        }
        self.record_kind.set(kind);

        let editor = CreatorListEditor::new(&fields.creators);
        while let Some(child) = self.creators_holder.first_child() {
            self.creators_holder.remove(&child);
        }
        self.creators_holder.append(&editor.widget);
        *self.creators.borrow_mut() = editor;

        if fetched.pages.is_some() {
            self.file_pages.set(fetched.pages);
        }
        *self.record.borrow_mut() = Some((fetched.record.clone(), fields));
        self.existing.replace(None);
        self.add_button.set_label("Add");
    }
}

pub(super) fn show(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, path: PathBuf) {
    let dialog = adw::Window::new();
    dialog.set_title(Some("Add this file"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(640, -1);
    close_on_escape(&dialog);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let add_button = gtk4::Button::with_label("Add");
    add_button.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&add_button);
    view.add_top_bar(&header);

    let content = gtk4::Box::new(Orientation::Vertical, 8);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);

    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let file_label = gtk4::Label::new(Some(&file_name));
    file_label.set_xalign(0.0);
    file_label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
    file_label.add_css_class("title-4");
    file_label.set_tooltip_text(Some(&path.to_string_lossy()));

    let lookup = gtk4::Entry::builder()
        .placeholder_text("Look up: a DOI, link, ISBN, BibTeX record — or a title")
        .hexpand(true)
        .build();
    let lookup_button = gtk4::Button::with_label("Look up");
    let lookup_row = gtk4::Box::new(Orientation::Horizontal, 6);
    lookup_row.append(&lookup);
    lookup_row.append(&lookup_button);

    let hint = gtk4::Label::new(Some("Reading the file…"));
    hint.set_wrap(true);
    hint.set_xalign(0.0);
    hint.set_hexpand(true);
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    let spinner = gtk4::Spinner::new();
    let hint_row = gtk4::Box::new(Orientation::Horizontal, 8);
    hint_row.append(&hint);
    hint_row.append(&spinner);

    let results = gtk4::ListBox::new();
    results.set_selection_mode(gtk4::SelectionMode::Single);
    results.add_css_class("fond-list");
    let results_scroll = gtk4::ScrolledWindow::new();
    results_scroll.add_css_class("fond-ground");
    results_scroll.set_child(Some(&results));
    results_scroll.set_min_content_height(180);
    results_scroll.set_visible(false);

    let type_labels: Vec<&str> = ITEM_KINDS.iter().map(|k| k.label).collect();
    let type_drop = gtk4::DropDown::from_strings(&type_labels);
    let text = |placeholder: &str| gtk4::Entry::builder().placeholder_text(placeholder).build();
    let title = text("");
    let year = text("e.g. 2026, or 2026-07-31");
    let container = text("Journal, or the book it is a chapter of");
    let publisher = text("");
    let location = text("City of publication, e.g. London");
    let doi = text("");
    let isbn = text("");
    let url = text("");
    let volume = text("");
    let issue = text("");
    let pages = text("e.g. 6-13");
    let creators_holder = gtk4::Box::new(Orientation::Vertical, 0);
    let editor = CreatorListEditor::new(&[]);
    creators_holder.append(&editor.widget);

    let details_heading = gtk4::Label::new(Some("Details"));
    details_heading.set_xalign(0.0);
    details_heading.add_css_class("heading");
    details_heading.set_margin_top(6);

    content.append(&file_label);
    content.append(&lookup_row);
    content.append(&hint_row);
    content.append(&results_scroll);
    content.append(&details_heading);
    content.append(&labeled("Type", &type_drop));
    content.append(&labeled("Title", &title));
    content.append(&labeled("Creator(s)", &creators_holder));
    content.append(&labeled("Date", &year));
    content.append(&labeled("Published in", &container));
    content.append(&labeled("Publisher", &publisher));
    content.append(&labeled("DOI", &doi));
    content.append(&labeled("ISBN", &isbn));
    content.append(&labeled("URL", &url));
    let more = gtk4::Expander::new(Some("More details"));
    let more_box = gtk4::Box::new(Orientation::Vertical, 8);
    more_box.set_margin_top(8);
    more_box.append(&labeled("Place of publication", &location));
    more_box.append(&labeled("Volume", &volume));
    more_box.append(&labeled("Issue", &issue));
    more_box.append(&labeled("Pages", &pages));
    more.set_child(Some(&more_box));
    content.append(&more);

    let scroller = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .max_content_height(640)
        .propagate_natural_height(true)
        .child(&content)
        .build();
    view.set_content(Some(&scroller));
    dialog.set_content(Some(&view));

    let ui = Rc::new(Ui {
        state: state.clone(),
        widgets: widgets.clone(),
        path,
        dialog: dialog.downgrade(),
        add_button: add_button.clone(),
        lookup: lookup.clone(),
        lookup_button: lookup_button.clone(),
        hint,
        spinner,
        results_scroll,
        results: results.clone(),
        candidates: RefCell::new(Vec::new()),
        type_drop,
        title,
        creators_holder,
        creators: RefCell::new(editor),
        year,
        container,
        publisher,
        location,
        doi: doi.clone(),
        isbn: isbn.clone(),
        url,
        volume,
        issue,
        pages,
        record: RefCell::new(None),
        record_kind: Cell::new(None),
        file_pages: Cell::new(None),
        existing: RefCell::new(None),
        busy: Cell::new(false),
    });
    // The dialog owns the state; the widgets' handlers below hold it only weakly.
    unsafe { dialog.set_data("add-file", ui.clone()) };

    let weak = Rc::downgrade(&ui);
    let with_ui = move |f: fn(&Rc<Ui>)| {
        let weak = weak.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                f(&ui);
            }
        }
    };
    {
        let go = with_ui(look_up);
        lookup.connect_activate(move |_| go());
    }
    {
        let go = with_ui(look_up);
        lookup_button.connect_clicked(move |_| go());
    }
    {
        // Enter on a search result takes it; so does a double click.
        let go = with_ui(look_up);
        results.connect_row_activated(move |_, _| go());
    }
    {
        let go = with_ui(submit);
        add_button.connect_clicked(move |_| go());
    }
    {
        // A different DOI or ISBN is a different question: forget "it is already in the library".
        for e in [&doi, &isbn] {
            let reset = with_ui(|ui| {
                if ui.existing.borrow_mut().take().is_some() {
                    ui.add_button.set_label("Add");
                    ui.say("", None, false);
                }
            });
            e.connect_changed(move |_| reset());
        }
    }
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }

    dialog.present();
    title_focus_later(&ui);
    start(&ui, Request::File);
}

fn title_focus_later(ui: &Rc<Ui>) {
    ui.lookup.grab_focus();
}

/// Ask for `request` on a worker thread and put what comes back into the form.
fn start(ui: &Rc<Ui>, request: Request) {
    ui.set_busy(true);
    let from_file = matches!(request, Request::File);
    ui.say(
        if from_file {
            "Reading the file…"
        } else {
            "Looking it up…"
        },
        None,
        false,
    );
    let (sender, receiver) = worker::channel::<Result<Fetched, Failure>>();
    let path = ui.path.clone();
    std::thread::spawn(move || {
        let _ = sender.send(fetch(request, &path));
    });

    let weak = Rc::downgrade(ui);
    receiver.attach(move |result| {
        let Some(ui) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        ui.set_busy(false);
        match result {
            Ok(fetched) => {
                ui.results_scroll.set_visible(false);
                ui.fill(&fetched);
                let note = if fetched.partial {
                    "Only the details from the search could be fetched — check them over."
                } else if from_file {
                    "Details read from the file — check them over, or look it up above."
                } else {
                    "Found. Check the details below, then add."
                };
                ui.say(note, None, false);
            }
            Err((message, detail)) => {
                if from_file && ui.title.text().is_empty() {
                    if let Some(stem) = ui.path.file_stem() {
                        ui.title.set_text(&stem.to_string_lossy());
                    }
                }
                ui.say(&message, detail.as_deref(), !from_file);
            }
        }
        glib::ControlFlow::Break
    });
}

/// The Look up button / Enter in the lookup box.
fn look_up(ui: &Rc<Ui>) {
    if ui.busy.get() {
        return;
    }
    // With search results showing, take the highlighted one.
    if ui.results_scroll.is_visible() {
        if let Some(row) = ui.results.selected_row() {
            let candidate = ui.candidates.borrow().get(row.index() as usize).cloned();
            if let Some(candidate) = candidate {
                start(
                    ui,
                    Request::Target(Target::from_candidate(&candidate), Some(candidate)),
                );
                return;
            }
        }
    }
    match fond_bib::identify(&ui.lookup.text()) {
        Identified::Empty => ui.say(
            "Paste a DOI, a link, an ISBN or a BibTeX record — or type a title to search for it.",
            None,
            false,
        ),
        Identified::Text(query) => search(ui, query),
        Identified::Doi(doi) => start(ui, Request::Target(Target::Doi(doi), None)),
        Identified::Arxiv(id) => start(
            ui,
            Request::Target(Target::Arxiv(fond_bib::arxiv_base(&id).to_string()), None),
        ),
        Identified::Isbn(isbn) => start(ui, Request::Target(Target::Isbn(isbn), None)),
        Identified::Url(url) => start(ui, Request::Url(url)),
        Identified::BibTeX(record) => start(ui, Request::BibTeX(record)),
    }
}

/// Look `query` up by title and list the matches to choose from.
fn search(ui: &Rc<Ui>, query: String) {
    ui.set_busy(true);
    ui.say("Searching…", None, false);
    let (sender, receiver) = worker::channel::<Result<Vec<Candidate>, String>>();
    std::thread::spawn(move || {
        let found = fond_bib::acquire::search_works(&query, 6).map_err(|e| e.to_string());
        let _ = sender.send(found);
    });

    let weak = Rc::downgrade(ui);
    receiver.attach(move |result| {
        let Some(ui) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        ui.set_busy(false);
        match result {
            Ok(found) if found.is_empty() => {
                ui.say(
                    "Nothing matched. Try fewer or different words, or fill in the details by hand.",
                    None,
                    false,
                );
            }
            Ok(found) => show_results(&ui, found),
            Err(detail) => ui.say(
                "Couldn't search — check that you're online, then try again.",
                Some(&detail),
                true,
            ),
        }
        glib::ControlFlow::Break
    });
}

fn show_results(ui: &Rc<Ui>, found: Vec<Candidate>) {
    while let Some(child) = ui.results.first_child() {
        ui.results.remove(&child);
    }
    for candidate in &found {
        let row_box = gtk4::Box::new(Orientation::Vertical, 2);
        row_box.set_margin_top(6);
        row_box.set_margin_bottom(6);
        row_box.set_margin_start(8);
        row_box.set_margin_end(8);
        let title = gtk4::Label::new(Some(&candidate.title));
        title.set_xalign(0.0);
        title.set_wrap(true);
        title.add_css_class("fond-row-title");
        let meta = gtk4::Label::new(Some(&result_meta(candidate)));
        meta.set_xalign(0.0);
        meta.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        meta.add_css_class("fond-row-meta");
        row_box.append(&title);
        row_box.append(&meta);
        let row = gtk4::ListBoxRow::new();
        row.add_css_class("fond-row");
        row.set_child(Some(&row_box));
        ui.results.append(&row);
    }
    *ui.candidates.borrow_mut() = found;
    ui.say(
        "Choose the one you mean, then press Look up (or Enter).",
        None,
        false,
    );
    ui.results_scroll.set_visible(true);
    if let Some(first) = ui.results.row_at_index(0) {
        ui.results.select_row(Some(&first));
        first.grab_focus();
    }
}

/// The Add button.
fn submit(ui: &Rc<Ui>) {
    if ui.busy.get() {
        return;
    }
    let record = ui.record.borrow().clone();
    let base = record
        .as_ref()
        .map(|(_, fields)| fields.clone())
        .unwrap_or_default();
    let fields = ui.edited_fields(&base);
    if fields.title.trim().is_empty() {
        ui.say("A title is required.", None, true);
        ui.title.grab_focus();
        return;
    }

    // Already there? Offer to put this file on that entry instead of adding a second one.
    let pending = ui.existing.borrow().clone();
    if let Some(key) = pending {
        attach_to_existing(ui, &key);
        return;
    }
    let existing = {
        let s = ui.state.borrow();
        s.library.as_ref().and_then(|lib| {
            let by_doi = (!fields.doi.is_empty())
                .then(|| lib.find_entry_by_doi(&fields.doi).ok().flatten())
                .flatten();
            by_doi.or_else(|| {
                (!fields.isbn.is_empty())
                    .then(|| lib.find_entry_by_isbn(&fields.isbn).ok().flatten())
                    .flatten()
            })
        })
    };
    if let Some(key) = existing {
        ui.say(
            &format!("This is already in your library as {key}. Attach the file to it, or change the DOI/ISBN to add it as a separate reference."),
            None,
            true,
        );
        ui.add_button.set_label("Attach to it");
        *ui.existing.borrow_mut() = Some(key);
        return;
    }

    let yaml = match &record {
        Some((record_yaml, base)) => apply_fields_to_yaml(record_yaml, base, &fields),
        None => {
            let kind = &ITEM_KINDS[(ui.type_drop.selected() as usize).min(ITEM_KINDS.len() - 1)];
            Ok(build_entry_yaml(&NewItemFields {
                ty: kind.entry_type,
                title: &fields.title,
                creators: fields.creators.clone(),
                date: &fields.year,
                container: &fields.container,
                publisher: &fields.publisher,
                location: &fields.location,
                doi: &fields.doi,
                isbn: &fields.isbn,
                url: &fields.url,
                volume: &fields.volume,
                issue: &fields.issue,
                pages: &fields.pages,
                parent_type: kind.parent_type,
                genre: kind.genre.unwrap_or_default(),
                ..Default::default()
            }))
        }
    };
    let added = yaml.and_then(|yaml| {
        let s = ui.state.borrow();
        let library = s.library.as_ref().expect("library open");
        library.add_from_yaml(&yaml)
    });
    match added {
        Ok(keys) if !keys.is_empty() => {
            let key = keys[0].clone();
            let attached = {
                let s = ui.state.borrow();
                let library = s.library.as_ref().expect("library open");
                library.store_attachment(&key, &ui.path, ui.file_pages.get())
            };
            match attached {
                Ok(_) => toast(&ui.widgets, &format!("Added {key} with its file")),
                Err(e) => toast(
                    &ui.widgets,
                    &format!(
                        "Added {key}, but couldn't attach the file: {}",
                        friendly::bib_error(&e)
                    ),
                ),
            }
            ui.close();
            reload_current(&ui.state, &ui.widgets);
            select_key(&ui.state, &ui.widgets, &key);
        }
        Ok(_) => ui.say("No reference was created from that.", None, true),
        Err(e) => ui.say(&friendly::bib_error(&e), None, true),
    }
}

fn attach_to_existing(ui: &Rc<Ui>, key: &str) {
    let attached = {
        let s = ui.state.borrow();
        let library = s.library.as_ref().expect("library open");
        library.store_attachment(key, &ui.path, ui.file_pages.get())
    };
    match attached {
        Ok(_) => {
            ui.close();
            toast(&ui.widgets, &format!("Attached the file to {key}"));
            reload_current(&ui.state, &ui.widgets);
            select_key(&ui.state, &ui.widgets, key);
        }
        Err(e) => ui.say(
            &format!("Couldn't attach the file: {}", friendly::bib_error(&e)),
            None,
            true,
        ),
    }
}
