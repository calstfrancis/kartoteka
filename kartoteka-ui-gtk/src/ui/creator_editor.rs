//! A structured multi-creator (author/editor/translator/…) list editor. Each row picks a
//! role, a two-field (Last/First) or single-field name, and can be reordered or removed —
//! the first row of each role group becomes that role's list in the saved entry (see
//! `fond_bib::creator`). Built as a plain "build function + `Rc<RefCell<Vec<Row>>>` state"
//! widget, matching this codebase's existing dynamic-list pattern (the Tasks editor in
//! `app_window.rs`) rather than a GObject-subclassed custom widget.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::Orientation;

use fond_bib::{Creator, CreatorRole};

/// The editor's change-notification slot: `None` until [`CreatorListEditor::connect_changed`]
/// registers a callback.
type OnChange = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// The widgets making up one creator row. Kept alongside the row in
/// [`CreatorListEditor::rows`] so the current state can be read back at save time and so
/// up/down reordering can find "this row"'s position.
pub struct CreatorRowWidgets {
    row: gtk4::ListBoxRow,
    role_dropdown: gtk4::DropDown,
    family_entry: gtk4::Entry,
    given_entry: gtk4::Entry,
    single_entry: gtk4::Entry,
    single_field: Rc<RefCell<bool>>,
    /// The creator's `prefix`/`suffix`/`comma_suffix`/`alias`, carried through unedited —
    /// not exposed as fields in this editor (see the plan), but preserved on save.
    passthrough: (Option<String>, Option<String>, bool, Option<String>),
}

/// A creator-list editor: an "Add creator" button plus a scrollable, reorderable list of
/// rows. `widget` is the whole thing, ready to append into a dialog's content area. `Clone`
/// is cheap (every field is a GTK object handle or an `Rc`, same as cloning a plain
/// `gtk4::Entry`) — useful for capturing into more than one signal-handler closure.
#[derive(Clone)]
pub struct CreatorListEditor {
    pub widget: gtk4::Box,
    rows: Rc<RefCell<Vec<CreatorRowWidgets>>>,
    on_change: OnChange,
}

fn fire_on_change(on_change: &OnChange) {
    if let Some(f) = on_change.borrow().as_ref() {
        f();
    }
}

fn role_labels() -> Vec<&'static str> {
    CreatorRole::ALL.iter().map(|r| r.label()).collect()
}

fn role_at(index: u32) -> CreatorRole {
    CreatorRole::ALL
        .get(index as usize)
        .copied()
        .unwrap_or(CreatorRole::Author)
}

fn role_index(role: CreatorRole) -> u32 {
    CreatorRole::ALL
        .iter()
        .position(|r| *r == role)
        .unwrap_or(0) as u32
}

/// Re-append every row into `list` in `rows`' order, so the visual order always matches the
/// order [`creators_from_rows`] will read back — simpler and safer than trying to track
/// `ListBoxRow` index math directly, and cheap enough for the handful of rows a real entry
/// has.
fn resync_order(list: &gtk4::ListBox, rows: &[CreatorRowWidgets]) {
    for r in rows {
        list.remove(&r.row);
    }
    for r in rows {
        list.append(&r.row);
    }
}

fn set_single_toggle_label(button: &gtk4::Button, single: bool) {
    let label = gtk4::Label::new(None);
    label.set_use_markup(true);
    label.set_markup(if single {
        "<b>Single field</b>"
    } else {
        "Single field"
    });
    button.set_child(Some(&label));
}

fn build_row(
    list: &gtk4::ListBox,
    scroll: &gtk4::ScrolledWindow,
    rows: &Rc<RefCell<Vec<CreatorRowWidgets>>>,
    on_change: &OnChange,
    creator: Option<&Creator>,
    default_role: CreatorRole,
) -> CreatorRowWidgets {
    let hbox = gtk4::Box::new(Orientation::Horizontal, 6);
    hbox.set_margin_top(4);
    hbox.set_margin_bottom(4);
    hbox.set_margin_start(8);
    hbox.set_margin_end(8);

    let role = creator.map(|c| c.role).unwrap_or(default_role);
    let role_dropdown = gtk4::DropDown::from_strings(&role_labels());
    role_dropdown.set_selected(role_index(role));

    // `width_chars` sets a minimum request, not a cap — `hexpand` still lets these grow with
    // the row. Without it, a row crowded with the role dropdown plus five buttons squeezes
    // these down to just a few pixels wide in a narrower dialog (e.g. "New item"), making the
    // name unreadable while typing.
    let family_entry = gtk4::Entry::builder()
        .placeholder_text("Last name")
        .hexpand(true)
        .width_chars(8)
        .build();
    let given_entry = gtk4::Entry::builder()
        .placeholder_text("First name")
        .hexpand(true)
        .width_chars(8)
        .build();
    let single_entry = gtk4::Entry::builder()
        .placeholder_text("Name (e.g. an organization)")
        .hexpand(true)
        .width_chars(12)
        .build();

    let single_field = creator.map(|c| c.single_field).unwrap_or(false);
    if let Some(c) = creator {
        family_entry.set_text(&c.family);
        given_entry.set_text(&c.given);
        single_entry.set_text(&c.family);
    }

    let two_field_box = gtk4::Box::new(Orientation::Horizontal, 4);
    two_field_box.set_hexpand(true);
    two_field_box.append(&family_entry);
    two_field_box.append(&given_entry);
    two_field_box.set_visible(!single_field);
    single_entry.set_visible(single_field);

    let name_area = gtk4::Box::new(Orientation::Horizontal, 4);
    name_area.set_hexpand(true);
    name_area.append(&two_field_box);
    name_area.append(&single_entry);

    let single_toggle = gtk4::Button::new();
    single_toggle.add_css_class("flat");
    single_toggle.set_tooltip_text(Some(
        "Switch between separate Last/First fields and one single name field \
         (for an organization or a name with no clear first/last split)",
    ));
    set_single_toggle_label(&single_toggle, single_field);

    let swap = gtk4::Button::from_icon_name("object-flip-horizontal-symbolic");
    swap.add_css_class("flat");
    swap.set_tooltip_text(Some("Swap which name is entered first"));
    swap.set_sensitive(!single_field);

    let up = gtk4::Button::from_icon_name("go-up-symbolic");
    up.add_css_class("flat");
    up.set_tooltip_text(Some("Move up"));
    let down = gtk4::Button::from_icon_name("go-down-symbolic");
    down.add_css_class("flat");
    down.set_tooltip_text(Some("Move down"));
    let remove = gtk4::Button::from_icon_name("user-trash-symbolic");
    remove.add_css_class("flat");
    remove.set_tooltip_text(Some("Remove this creator"));

    hbox.append(&role_dropdown);
    hbox.append(&name_area);
    hbox.append(&single_toggle);
    hbox.append(&swap);
    hbox.append(&up);
    hbox.append(&down);
    hbox.append(&remove);

    let row = gtk4::ListBoxRow::new();
    row.add_css_class("fond-row");
    row.set_activatable(false);
    row.set_child(Some(&hbox));
    list.append(&row);
    // `scroll`'s propagate-natural-height size gets cached on first measure and does not
    // reliably re-query when a row is added/removed later — a known GTK4 ScrolledWindow
    // quirk, confirmed live: `list`'s own `measure()` correctly grows (e.g. 60px -> 120px
    // for 2 rows) but the ScrolledWindow's allocation stayed at 60px, leaving every row past
    // the first allocated 0 height (invisible) even though it was really in the `ListBox`.
    // Forcing a resize here (and in the remove handler below) makes it re-measure.
    scroll.queue_resize();

    let single_field_state = Rc::new(RefCell::new(single_field));

    // Save-on-blur/Enter, matching the outer citation form's per-field autosave — plus
    // save-on-click for every structural change below (role, single/two-field, swap,
    // reorder, remove), since those have no "blur" of their own to hang a save off of.
    for entry in [&family_entry, &given_entry, &single_entry] {
        let oc_activate = on_change.clone();
        entry.connect_activate(move |_| fire_on_change(&oc_activate));
        let oc_focus = on_change.clone();
        let focus = gtk4::EventControllerFocus::new();
        focus.connect_leave(move |_| fire_on_change(&oc_focus));
        entry.add_controller(focus);
    }
    {
        let on_change = on_change.clone();
        role_dropdown.connect_selected_notify(move |_| fire_on_change(&on_change));
    }

    {
        let two_field_box = two_field_box.clone();
        let single_entry = single_entry.clone();
        let family_entry = family_entry.clone();
        let given_entry = given_entry.clone();
        let swap = swap.clone();
        let state = single_field_state.clone();
        let on_change = on_change.clone();
        single_toggle.connect_clicked(move |btn| {
            let mut single = state.borrow_mut();
            if *single {
                // Switching to two fields: no guessing — the whole string goes into Last
                // name, First left blank for the user to redistribute.
                family_entry.set_text(&single_entry.text());
                given_entry.set_text("");
            } else {
                // Switching to single field: join as "Given Family".
                let joined = format!("{} {}", given_entry.text(), family_entry.text());
                single_entry.set_text(joined.trim());
            }
            *single = !*single;
            two_field_box.set_visible(!*single);
            single_entry.set_visible(*single);
            swap.set_sensitive(!*single);
            set_single_toggle_label(btn, *single);
            fire_on_change(&on_change);
        });
    }

    {
        let family_entry = family_entry.clone();
        let given_entry = given_entry.clone();
        let on_change = on_change.clone();
        swap.connect_clicked(move |_| {
            let f = family_entry.text();
            let g = given_entry.text();
            family_entry.set_text(&g);
            given_entry.set_text(&f);
            fire_on_change(&on_change);
        });
    }

    {
        let list = list.clone();
        let scroll = scroll.clone();
        let rows = rows.clone();
        let row_ref = row.clone();
        let on_change = on_change.clone();
        remove.connect_clicked(move |_| {
            list.remove(&row_ref);
            rows.borrow_mut().retain(|r| r.row != row_ref);
            scroll.queue_resize();
            fire_on_change(&on_change);
        });
    }

    {
        let list = list.clone();
        let rows = rows.clone();
        let row_ref = row.clone();
        let on_change = on_change.clone();
        up.connect_clicked(move |_| {
            let mut rows_mut = rows.borrow_mut();
            if let Some(idx) = rows_mut.iter().position(|r| r.row == row_ref) {
                if idx > 0 {
                    rows_mut.swap(idx, idx - 1);
                    resync_order(&list, &rows_mut);
                    fire_on_change(&on_change);
                }
            }
        });
    }

    {
        let list = list.clone();
        let rows = rows.clone();
        let row_ref = row.clone();
        let on_change = on_change.clone();
        down.connect_clicked(move |_| {
            let mut rows_mut = rows.borrow_mut();
            if let Some(idx) = rows_mut.iter().position(|r| r.row == row_ref) {
                if idx + 1 < rows_mut.len() {
                    rows_mut.swap(idx, idx + 1);
                    resync_order(&list, &rows_mut);
                    fire_on_change(&on_change);
                }
            }
        });
    }

    CreatorRowWidgets {
        row,
        role_dropdown,
        family_entry,
        given_entry,
        single_entry,
        single_field: single_field_state,
        passthrough: creator
            .map(|c| {
                (
                    c.prefix.clone(),
                    c.suffix.clone(),
                    c.comma_suffix,
                    c.alias.clone(),
                )
            })
            .unwrap_or_default(),
    }
}

impl CreatorListEditor {
    /// Build a new editor, seeded with `initial` (typically from
    /// `fond_bib::entry::read_fields(entry).creators` or
    /// `fond_bib::creator::parse_creators(entry)`).
    pub fn new(initial: &[Creator]) -> Self {
        let widget = gtk4::Box::new(Orientation::Vertical, 6);

        let list = gtk4::ListBox::new();
        list.set_selection_mode(gtk4::SelectionMode::None);
        list.add_css_class("fond-list");

        let scroll = gtk4::ScrolledWindow::new();
        scroll.add_css_class("fond-ground");
        scroll.set_child(Some(&list));
        scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        // Not `propagate_natural_height` + `max_content_height`: that combination caches its
        // measurement on first layout and does not reliably re-measure when a row is added
        // or removed later — confirmed live (a second "Add creator" row landed in the
        // `ListBox` with the right data, `list.measure()` correctly reported the taller
        // size, but the `ScrolledWindow`'s own allocation never grew to show it, even after
        // `queue_resize()`). A fixed `min_content_height` sidesteps the whole renegotiation:
        // the box always occupies this height and scrolls internally (GTK4's overlay
        // scrollbar, invisible until hovered) for anything past it, which GTK recomputes
        // correctly on every allocation regardless of *when* the row was added. ~168px fits
        // three rows before scrolling — covers the common cases (single/dual author, or a
        // translator/editor credit alongside the author) without needing to scroll at all.
        scroll.set_min_content_height(168);

        let rows: Rc<RefCell<Vec<CreatorRowWidgets>>> = Rc::new(RefCell::new(Vec::new()));
        let on_change: OnChange = Rc::new(RefCell::new(None));
        for creator in initial {
            let row = build_row(
                &list,
                &scroll,
                &rows,
                &on_change,
                Some(creator),
                CreatorRole::Author,
            );
            rows.borrow_mut().push(row);
        }

        let add_button = gtk4::Button::from_icon_name("list-add-symbolic");
        add_button.set_label("Add creator");
        add_button.set_halign(gtk4::Align::Start);
        {
            let list = list.clone();
            let scroll = scroll.clone();
            let rows = rows.clone();
            let on_change = on_change.clone();
            add_button.connect_clicked(move |_| {
                // New rows default to the previous row's role — convenient when entering
                // several creators of the same (non-author) type in a row, e.g. translators.
                let default_role = rows
                    .borrow()
                    .last()
                    .map(|r| role_at(r.role_dropdown.selected()))
                    .unwrap_or(CreatorRole::Author);
                let new_row = build_row(&list, &scroll, &rows, &on_change, None, default_role);
                new_row.family_entry.grab_focus();
                rows.borrow_mut().push(new_row);
                fire_on_change(&on_change);
            });
        }

        widget.append(&scroll);
        widget.append(&add_button);

        CreatorListEditor {
            widget,
            rows,
            on_change,
        }
    }

    /// Register a callback to run after any change to the editor's content — every field's
    /// blur/Enter, and every structural change (add/remove/reorder/role/single-field-toggle).
    /// Used by the detail-pane inline editor to fold creators into its existing
    /// per-field autosave.
    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        *self.on_change.borrow_mut() = Some(Rc::new(f));
    }

    /// Read the editor's current state back out, in display order. Rows with no name text
    /// at all (an "Add creator" click the user never filled in) are dropped.
    pub fn creators(&self) -> Vec<Creator> {
        self.rows
            .borrow()
            .iter()
            .map(|r| {
                let single_field = *r.single_field.borrow();
                let (prefix, suffix, comma_suffix, alias) = r.passthrough.clone();
                let (family, given) = if single_field {
                    (r.single_entry.text().trim().to_string(), String::new())
                } else {
                    (
                        r.family_entry.text().trim().to_string(),
                        r.given_entry.text().trim().to_string(),
                    )
                };
                Creator {
                    role: role_at(r.role_dropdown.selected()),
                    family,
                    given,
                    single_field,
                    prefix,
                    suffix,
                    comma_suffix,
                    alias,
                }
            })
            .filter(|c| !(c.family.is_empty() && c.given.is_empty()))
            .collect()
    }
}
