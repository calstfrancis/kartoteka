//! The Add box — one place to put a reference into the library.
//!
//! Paste whatever you have: a DOI, a link (to an article, an arXiv paper, any web page), an
//! ISBN, a BibTeX record — or just type a title and pick the right match from a list. The box
//! works out which it is (`fond_bib::identify`), says so in plain words, and does the right
//! thing. It never adds on a guess: free text only *searches*, and you choose the result.
//!
//! Something already in the library is not added twice — the box says so and jumps to it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use fond_bib::acquire::{Candidate, CandidateId};
use fond_bib::Identified;
use gtk4::prelude::*;
use gtk4::Orientation;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::{
    add_from_url, close_on_escape, reload_current, select_key, show_add_epub, show_add_pdf,
    show_new_item_dialog, toast, AppState, Widgets,
};
use crate::ui::{friendly, worker};

/// What the box says under the text field, what its main button is called, and whether it can
/// act yet — for the text currently in the field.
#[derive(Debug, PartialEq, Eq)]
struct Reading {
    hint: String,
    button: &'static str,
    ready: bool,
}

fn describe(identified: &Identified) -> Reading {
    let (hint, button, ready) = match identified {
        Identified::Empty => (
            "Paste a DOI, a link, an ISBN or a BibTeX record — or type a title to search for it."
                .to_string(),
            "Add",
            false,
        ),
        Identified::Doi(doi) => (format!("DOI {doi}"), "Add", true),
        Identified::Arxiv(id) => (format!("arXiv paper {id}"), "Add", true),
        Identified::Isbn(isbn) => (format!("ISBN {isbn}"), "Add", true),
        Identified::Url(_) => (
            "A web page — Kartoteka reads its citation details, and fetches its PDF if it links one."
                .to_string(),
            "Add",
            true,
        ),
        Identified::BibTeX(_) => (
            "A BibTeX record — it is added as written.".to_string(),
            "Add",
            true,
        ),
        Identified::Text(_) => (
            "Press Search to look this up by title.".to_string(),
            "Search",
            true,
        ),
    };
    Reading {
        hint,
        button,
        ready,
    }
}

/// An identifier the box can fetch a reference for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Doi(String),
    /// Without its `vN` version.
    Arxiv(String),
    Isbn(String),
}

impl Target {
    fn from_candidate(candidate: &Candidate) -> Target {
        match &candidate.id {
            CandidateId::Doi(doi) => Target::Doi(doi.clone()),
            CandidateId::Isbn(isbn) => Target::Isbn(isbn.clone()),
        }
    }

    /// The DOI an arXiv paper is registered under, which is what an earlier add of it recorded.
    fn arxiv_doi(id: &str) -> String {
        format!("10.48550/arXiv.{id}")
    }

    /// What to tell someone when this couldn't be fetched, and what to try instead.
    fn failure_message(&self) -> &'static str {
        match self {
            Target::Doi(_) => {
                "Couldn't find that DOI online. Check it's complete and try again, or enter the \
                 reference by hand."
            }
            Target::Arxiv(_) => {
                "Couldn't find that arXiv paper. Older papers may not have a record yet — try \
                 searching by title, or enter it by hand."
            }
            Target::Isbn(_) => {
                "No book found for that ISBN. Try searching by title, or enter the book by hand."
            }
        }
    }
}

/// The result of a lookup that fetched a record: its text, and whether that is BibTeX (else
/// Hayagriva YAML).
type Fetched = (bool, String);

struct Ui {
    state: Rc<RefCell<AppState>>,
    widgets: Rc<Widgets>,
    /// Weak, so the dialog can be freed once closed (the dialog owns this struct).
    dialog: glib::WeakRef<adw::Window>,
    entry: gtk4::Entry,
    hint: gtk4::Label,
    spinner: gtk4::Spinner,
    button: gtk4::Button,
    results_scroll: gtk4::ScrolledWindow,
    results: gtk4::ListBox,
    candidates: RefCell<Vec<Candidate>>,
    busy: Cell<bool>,
}

impl Ui {
    fn close(&self) {
        if let Some(dialog) = self.dialog.upgrade() {
            dialog.close();
        }
    }
}

pub(super) fn show(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>) {
    if state.borrow().library.is_none() {
        toast(widgets, "Open a library first");
        return;
    }

    let dialog = adw::Window::new();
    dialog.set_title(Some("Add a reference"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(&widgets.window));
    dialog.set_default_size(580, -1);
    close_on_escape(&dialog);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk4::Button::with_label("Cancel");
    let button = gtk4::Button::with_label("Add");
    button.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&button);
    view.add_top_bar(&header);

    let content = gtk4::Box::new(Orientation::Vertical, 10);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let entry = gtk4::Entry::builder()
        .placeholder_text("DOI, link, ISBN, BibTeX — or a title")
        .hexpand(true)
        .build();
    entry.add_css_class("title-4");

    let hint = gtk4::Label::new(None);
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
    results_scroll.set_min_content_height(250);
    results_scroll.set_visible(false);

    // Other ways in, for when pasting isn't what you have.
    let others = gtk4::Box::new(Orientation::Horizontal, 6);
    let pdf = gtk4::Button::with_label("Add a PDF…");
    let epub = gtk4::Button::with_label("Add an EPUB…");
    let by_hand = gtk4::Button::with_label("Enter by hand…");
    for b in [&pdf, &epub, &by_hand] {
        b.add_css_class("flat");
        others.append(b);
    }
    let tip = gtk4::Label::new(Some(
        "You can also drop PDFs, EPUBs or a folder straight onto the main window.",
    ));
    tip.set_xalign(0.0);
    tip.set_wrap(true);
    tip.add_css_class("dim-label");
    tip.add_css_class("caption");

    content.append(&entry);
    content.append(&hint_row);
    content.append(&results_scroll);
    content.append(&others);
    content.append(&tip);
    view.set_content(Some(&content));
    dialog.set_content(Some(&view));

    let ui = Rc::new(Ui {
        state: state.clone(),
        widgets: widgets.clone(),
        dialog: dialog.downgrade(),
        entry: entry.clone(),
        hint,
        spinner,
        button: button.clone(),
        results_scroll,
        results: results.clone(),
        candidates: RefCell::new(Vec::new()),
        busy: Cell::new(false),
    });
    // The dialog owns the state; the widgets' handlers below hold it only weakly.
    unsafe { dialog.set_data("add-box", ui.clone()) };
    refresh(&ui);

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
        let on_change = with_ui(|ui| {
            // Editing the text starts a fresh question: stale search results go.
            ui.candidates.borrow_mut().clear();
            ui.results_scroll.set_visible(false);
            refresh(ui);
        });
        entry.connect_changed(move |_| on_change());
    }
    {
        let go = with_ui(go);
        entry.connect_activate(move |_| go());
    }
    {
        let go = with_ui(go);
        button.connect_clicked(move |_| go());
    }
    {
        let go = with_ui(go);
        results.connect_row_activated(move |_, _| go());
    }
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    for (b, action) in [
        (
            &pdf,
            show_add_pdf as fn(&Rc<RefCell<AppState>>, &Rc<Widgets>),
        ),
        (&epub, show_add_epub),
        (&by_hand, show_new_item_dialog),
    ] {
        let dialog = dialog.clone();
        let state = state.clone();
        let widgets = widgets.clone();
        b.connect_clicked(move |_| {
            dialog.close();
            action(&state, &widgets);
        });
    }

    dialog.present();
    entry.grab_focus();
}

/// Bring the hint and main button in line with the text in the field.
fn refresh(ui: &Rc<Ui>) {
    let reading = describe(&fond_bib::identify(&ui.entry.text()));
    ui.hint.set_text(&reading.hint);
    ui.hint.set_tooltip_text(None);
    ui.hint.remove_css_class("error");
    ui.button.set_label(reading.button);
    ui.button.set_sensitive(reading.ready && !ui.busy.get());
}

/// Lock the box while something is being looked up, and unlock it after.
fn set_busy(ui: &Rc<Ui>, busy: bool) {
    ui.busy.set(busy);
    ui.entry.set_sensitive(!busy);
    ui.results.set_sensitive(!busy);
    ui.spinner.set_spinning(busy);
    if busy {
        ui.button.set_sensitive(false);
    } else {
        ui.button.set_sensitive(!ui.entry.text().trim().is_empty());
    }
}

/// Say something went wrong, right where the person is looking (a toast would sit behind this
/// dialog), and leave the box ready to try again.
fn show_error(ui: &Rc<Ui>, message: &str, detail: Option<&str>) {
    set_busy(ui, false);
    ui.hint.set_text(message);
    ui.hint.set_tooltip_text(detail);
    ui.hint.add_css_class("error");
    ui.entry.grab_focus();
}

/// The main button / Enter.
fn go(ui: &Rc<Ui>) {
    if ui.busy.get() {
        return;
    }
    // With search results showing, Enter takes the highlighted one.
    if ui.results_scroll.is_visible() {
        if let Some(row) = ui.results.selected_row() {
            let target = ui
                .candidates
                .borrow()
                .get(row.index() as usize)
                .map(Target::from_candidate);
            if let Some(target) = target {
                add_target(ui, target);
                return;
            }
        }
    }
    match fond_bib::identify(&ui.entry.text()) {
        Identified::Empty => {}
        Identified::Text(query) => search(ui, query),
        Identified::Doi(doi) => add_target(ui, Target::Doi(doi)),
        Identified::Arxiv(id) => {
            add_target(ui, Target::Arxiv(fond_bib::arxiv_base(&id).to_string()))
        }
        Identified::Isbn(isbn) => add_target(ui, Target::Isbn(isbn)),
        Identified::Url(url) => {
            ui.close();
            add_from_url(&ui.state, &ui.widgets, url);
        }
        Identified::BibTeX(record) => {
            let added = {
                let s = ui.state.borrow();
                s.library.as_ref().map(|lib| lib.add_bibtex(&record))
            };
            match added {
                Some(Ok(keys)) if !keys.is_empty() => finish_added(ui, &keys),
                Some(Ok(_)) => show_error(ui, "That record had no entries in it.", None),
                Some(Err(e)) => show_error(ui, &friendly::bib_error(&e), None),
                None => {}
            }
        }
    }
}

/// Add the reference `target` names — unless the library already has it.
fn add_target(ui: &Rc<Ui>, target: Target) {
    let existing = {
        let s = ui.state.borrow();
        s.library.as_ref().and_then(|lib| {
            match &target {
                Target::Doi(doi) => lib.find_entry_by_doi(doi),
                Target::Arxiv(id) => lib.find_entry_by_doi(&Target::arxiv_doi(id)),
                Target::Isbn(isbn) => lib.find_entry_by_isbn(isbn),
            }
            .ok()
            .flatten()
        })
    };
    if let Some(key) = existing {
        ui.close();
        toast(&ui.widgets, &format!("Already in your library as {key}"));
        select_key(&ui.state, &ui.widgets, &key);
        return;
    }

    set_busy(ui, true);
    ui.hint.set_text("Looking it up…");
    let (sender, receiver) = worker::channel::<Result<Fetched, String>>();
    let for_worker = target.clone();
    std::thread::spawn(move || {
        let fetched = match &for_worker {
            Target::Doi(doi) => fond_bib::acquire::fetch_doi_bibtex(doi).map(|s| (true, s)),
            Target::Arxiv(id) => fond_bib::acquire::fetch_arxiv_bibtex(id).map(|s| (true, s)),
            Target::Isbn(isbn) => fond_bib::acquire::fetch_isbn_yaml(isbn).map(|s| (false, s)),
        };
        let _ = sender.send(fetched.map_err(|e| e.to_string()));
    });

    let weak = Rc::downgrade(ui);
    receiver.attach(move |result| {
        let Some(ui) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        match result {
            Ok((is_bibtex, payload)) => {
                let added = {
                    let s = ui.state.borrow();
                    s.library.as_ref().map(|lib| {
                        if is_bibtex {
                            lib.add_bibtex(&payload)
                        } else {
                            lib.add_from_yaml(&payload)
                        }
                    })
                };
                match added {
                    Some(Ok(keys)) if !keys.is_empty() => finish_added(&ui, &keys),
                    Some(Ok(_)) => show_error(&ui, target.failure_message(), None),
                    Some(Err(e)) => show_error(&ui, &friendly::bib_error(&e), None),
                    None => {}
                }
            }
            Err(detail) => show_error(&ui, target.failure_message(), Some(&detail)),
        }
        glib::ControlFlow::Break
    });
}

/// Close the box, say what was added, and show it in the library.
fn finish_added(ui: &Rc<Ui>, keys: &[String]) {
    let message = match keys {
        [one] => format!("Added {one}"),
        many => format!("Added {} references", many.len()),
    };
    ui.close();
    toast(&ui.widgets, &message);
    reload_current(&ui.state, &ui.widgets);
    if let Some(first) = keys.first() {
        select_key(&ui.state, &ui.widgets, first);
    }
}

/// Look `query` up by title and list the matches to choose from.
fn search(ui: &Rc<Ui>, query: String) {
    set_busy(ui, true);
    ui.hint.set_text("Searching…");
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
        match result {
            Ok(found) if found.is_empty() => {
                set_busy(&ui, false);
                ui.hint.set_text(
                    "Nothing matched. Try fewer or different words, or enter it by hand.",
                );
                ui.entry.grab_focus();
            }
            Ok(found) => show_results(&ui, found),
            Err(detail) => show_error(
                &ui,
                "Couldn't search — check that you're online, then try again.",
                Some(&detail),
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

    set_busy(ui, false);
    ui.hint
        .set_text("Choose the one you mean, then press Add (or Enter).");
    ui.button.set_label("Add");
    ui.button.set_sensitive(true);
    ui.results_scroll.set_visible(true);
    if let Some(first) = ui.results.row_at_index(0) {
        ui.results.select_row(Some(&first));
        first.grab_focus();
    }
}

/// `Article · Cone, James H. · 1970 · Christianity and Crisis`
fn result_meta(candidate: &Candidate) -> String {
    let byline = candidate.byline();
    if byline.is_empty() {
        candidate.kind.clone()
    } else {
        format!("{} · {byline}", candidate.kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fond_bib::identify;

    #[test]
    fn each_kind_of_input_gets_a_plain_description_and_the_right_button() {
        let say = |s: &str| describe(&identify(s));

        let empty = say("");
        assert!(!empty.ready);
        assert!(empty.hint.contains("Paste a DOI"));

        assert_eq!(say("10.1038/171737a0").hint, "DOI 10.1038/171737a0");
        assert_eq!(
            say("https://arxiv.org/abs/1706.03762v5").hint,
            "arXiv paper 1706.03762v5"
        );
        assert_eq!(say("ISBN 978-0-14-044913-6").hint, "ISBN 9780140449136");
        assert!(say("https://plato.stanford.edu/entries/x/")
            .hint
            .starts_with("A web page"));
        assert!(say("@book{x, title={T}}")
            .hint
            .starts_with("A BibTeX record"));

        // Words only ever search; everything else adds.
        let words = say("black theology and black power");
        assert_eq!((words.button, words.ready), ("Search", true));
        for s in ["10.1038/171737a0", "9780140449136", "https://example.org/a"] {
            assert_eq!(say(s).button, "Add", "{s}");
        }
    }

    #[test]
    fn failures_say_what_to_try_instead() {
        for target in [
            Target::Doi("10.1/x".into()),
            Target::Arxiv("1706.03762".into()),
            Target::Isbn("9780140449136".into()),
        ] {
            let message = target.failure_message();
            assert!(
                message.contains("by hand"),
                "{target:?} gives no way forward: {message}"
            );
        }
    }

    #[test]
    fn an_arxiv_paper_is_found_again_by_the_doi_it_was_added_under() {
        assert_eq!(Target::arxiv_doi("1706.03762"), "10.48550/arXiv.1706.03762");
    }

    #[test]
    fn a_search_result_is_fetched_by_the_identifier_it_carries() {
        let hit = |id| Candidate {
            id,
            title: "T".into(),
            authors: vec!["Cone, James H.".into()],
            year: "1970".into(),
            container: "Christianity and Crisis".into(),
            kind: "Article".into(),
        };
        assert_eq!(
            Target::from_candidate(&hit(CandidateId::Doi("10.1/x".into()))),
            Target::Doi("10.1/x".into())
        );
        assert_eq!(
            Target::from_candidate(&hit(CandidateId::Isbn("9780140449136".into()))),
            Target::Isbn("9780140449136".into())
        );
        assert_eq!(
            result_meta(&hit(CandidateId::Doi("10.1/x".into()))),
            "Article · Cone, James H. · 1970 · Christianity and Crisis"
        );
    }
}
