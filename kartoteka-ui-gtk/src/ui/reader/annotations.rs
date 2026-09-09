//! The annotations dialog: an entry's highlights, underlines, strikeouts and marginal
//! notes, each jumpable into the reader, editable, or deletable, plus Markdown export.
//!
//! Reads and writes the same sidecar the readers do, through [`ReaderHost`] — it re-reads
//! on each action rather than holding a snapshot, so it never disagrees with a reader open
//! on the same document.

use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{gio, Orientation};
use libadwaita as adw;
use libadwaita::prelude::*;

use super::epub::show_epub_reader;
use super::pdf::show_pdf_reader;
use crate::ui::reader::ReaderHost;

/// List an entry's annotations — page, kind, note — so each can be jumped to in the reader,
/// have its note edited, or be deleted. Reads and writes the same `annots/<key>.json`
/// sidecar `show_pdf_reader`'s drag-to-highlight writes to.
pub(crate) fn show_annotations_dialog(
    host: &Rc<dyn ReaderHost>,
    parent: &adw::ApplicationWindow,
    // Filename stem for an exported annotation file, and nothing else — the reader never
    // interprets it. Kartoteka passes the citation key; Sputnik will pass whatever names
    // the document on its side.
    document_id: &str,
    // Independent per-format attachment info (hash, blob path) — an entry can have both a
    // PDF and an EPUB attached, so each annotation row's "Go to" routes to whichever of these
    // matches that specific annotation's anchor (`page` → PDF, `chapter` → EPUB), not a
    // single kind fixed for the whole dialog (M5-SPEC.md Tier 4).
    pdf_attachment: Option<(String, std::path::PathBuf)>,
    epub_attachment: Option<(String, std::path::PathBuf)>,
    reader_title: &str,
) {
    // An absent sidecar and one with every annotation deleted are both "nothing to show"
    // here. Before the `ReaderHost` boundary these were distinguishable (a missing file gave
    // this toast; an empty file opened an empty dialog); now both give the toast, since the
    // host reports "no sidecar yet" as an empty one so the readers don't have to care.
    let sidecar = host.load_annotations();
    if sidecar.annotations.is_empty() {
        host.notify("No annotations for this entry");
        return;
    }

    // The PDF's own printed page numbers, if any — same resolution `show_pdf_reader` uses
    // (native `/PageLabels` first, falling back to a manual `page_label_override` on the
    // entry's note when the PDF declares none), so a "Page N" here always matches what the
    // reader itself shows. Best-effort: any failure to open the PDF just leaves this empty,
    // and every row/export falls back to the raw page number as before.
    let page_labels: Vec<Option<String>> = pdf_attachment
        .as_ref()
        .and_then(|(_, blob)| {
            let bytes = std::fs::read(blob).ok()?;
            let pdfium = fond_doc::bind_pdfium().ok()?;
            let native = fond_doc::page_labels(&pdfium, &bytes).unwrap_or_default();
            if native.iter().any(|l| l.is_some()) {
                return Some(native);
            }
            let count = fond_doc::page_count(&pdfium, &bytes).unwrap_or(0);
            let override_value = host.page_label_override();
            Some(override_value.map(|ov| ov.apply(count)).unwrap_or(native))
        })
        .unwrap_or_default();

    let dialog = adw::Window::new();
    dialog.set_title(Some("Annotations"));
    dialog.set_modal(true);
    dialog.set_transient_for(Some(parent));
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
        let host = host.clone();
        let document_id = document_id.to_string();
        let parent = parent.clone();
        let reader_title = reader_title.to_string();
        let page_labels = page_labels.clone();
        export_button.connect_clicked(move |_| {
            // Reload fresh rather than reuse the dialog's own `sidecar` capture — the list
            // above can go stale if a note was edited or an annotation deleted earlier in
            // this same dialog session (each of those reloads independently, not through
            // this closure's binding), so an export should reflect what's actually on disk.
            let sidecar = host.load_annotations();
            if sidecar.annotations.is_empty() {
                host.notify("No annotations for this entry");
                return;
            }
            let markdown = sidecar.to_markdown(&reader_title, Some(&page_labels));

            let default_name = format!("{document_id}-annotations.md");
            let save = gtk4::FileDialog::builder()
                .title("Export annotations")
                .initial_name(&default_name)
                .build();
            let host = host.clone();
            save.save(Some(&parent), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        match std::fs::write(&path, &markdown) {
                            Ok(()) => host.notify(&format!("Exported to {}", path.display())),
                            Err(e) => host.notify(&format!("Could not write file: {e}")),
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
            (Some(p), _) => {
                let printed = page_labels
                    .get((p as usize).saturating_sub(1))
                    .and_then(|l| l.clone());
                format!("Page {}", printed.unwrap_or_else(|| p.to_string()))
            }
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
                let host = host.clone();
                let parent = parent.clone();
                let title = reader_title.to_string();
                let page = annotation.page;
                let annotation_id = annotation.id.clone();
                goto_button.connect_clicked(move |_| {
                    if is_pdf {
                        // `page` is always `Some` for a PDF-anchored annotation;
                        // `unwrap_or(1)` is just a defensive fallback, not an expected path.
                        show_pdf_reader(&host, &parent, &hash, &blob, &title, page.unwrap_or(1))
                    } else {
                        show_epub_reader(
                            &host,
                            &parent,
                            &hash,
                            &blob,
                            &title,
                            Some(&annotation_id),
                            None,
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
            let host = host.clone();
            let id = annotation.id.clone();
            move |text: &str| {
                let text = text.trim();
                let mut sidecar = host.load_annotations();
                let Some(a) = sidecar.annotations.iter_mut().find(|a| a.id == id) else {
                    return;
                };
                a.note = (!text.is_empty()).then(|| text.to_string());
                if let Err(e) = host.save_annotations(&sidecar) {
                    host.notify(&e);
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
            let host = host.clone();
            let id = annotation.id.clone();
            let list = list.clone();
            let row = row.clone();
            delete_button.connect_clicked(move |_| {
                let result = {
                    let mut sidecar = host.load_annotations();
                    sidecar.annotations.retain(|a| a.id != id);
                    host.save_annotations(&sidecar)
                };
                match result {
                    Ok(()) => {
                        list.remove(&row);
                        host.notify("Annotation deleted");
                    }
                    Err(e) => host.notify(&e),
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
