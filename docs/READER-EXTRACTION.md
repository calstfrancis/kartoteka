# Reader extraction — survey and boundary

Status: **in progress.** Phase 1 of `sputnik/docs/ROADMAP.md`, executed here.

The goal is a `fond-read-gtk` crate holding the PDF and EPUB readers, consumed by both
Kartoteka and Sputnik, so the reader is never written twice. This document is step 1.1:
the survey that defines the boundary, written before anything moves.

The extraction is deliberately split into two commits with a shipped release between them
(`ROADMAP.md` Phase 1.3–1.4): **restructure in place first**, then relocate. If the reader
breaks, it broke in the commit that changed no logic.

---

## 1. What is being extracted

`kartoteka-ui-gtk/src/ui/app_window.rs` lines **9085–14003** — about 4,900 lines of a
15,825-line file.

| Region | Lines | Contents |
|---|---|---|
| Annotations dialog | 9085–9400 | `show_annotations_dialog` |
| PDF reader | 9401–12454 | `ReaderState`, drag/highlight, continuous scroll, context menu, page labels, `show_pdf_reader`, page-number and note dialogs |
| EPUB reader | 12455–14003 | `EpubReaderState`, whole-book search, WebKit highlight injection, `show_epub_reader`, `epub_go_to` |

`fond-doc` already owns rendering, text extraction, search and the annotation schema. What
moves is the **GTK widget layer over it** — not document logic.

### What stays behind (host-side glue, despite sitting adjacent)

Lines 9003–9084 are *not* part of the reader, even though they read like it:

- `ReaderAttachmentKind`, `detect_attachment_kind` — also called from `show_detail`
  (line 8077) to pick an attachment
- `attachment_presence` — called from `open_library` (line 4465) to decide whether the
  Read buttons are shown at all; takes a `Library` and a `Note`
- `open_pdf` — hands the blob to an external application (line 8438)

These answer "which attachment exists, and should the button appear," which is a library
question, not a reader question.

---

## 2. The coupling list

This is the whole of it. The reader reaches outside itself in exactly three ways.

### 2a. `Library` — five methods, all keyed by citation key

| Method | Calls | Used for |
|---|---:|---|
| `write_annotations(key, &sidecar)` | 15 | every highlight/underline/strike/note save |
| `load_annotations(key)` | 6 | sidecar loaded once at reader open |
| `load_note(key)` | 5 | **only** `frontmatter.page_label_override` |
| `write_note(key, &note)` | 3 | **only** `frontmatter.progress` and `frontmatter.page_label_override` |
| `attachment_blob_path(hash)` | 1 | resolving the blob to read |

The note access is the important finding: the reader never reads a note's prose, tags,
rating or relations. It touches **two frontmatter fields**. So the trait can expose those
two directly and the reader need never learn what a note or a citation key is.

### 2b. `AppState` — one field

`open_readers: HashMap<String, adw::Window>`, keyed by attachment hash. Prevents opening
two readers on the same document, which would each hold an independent in-memory sidecar
snapshot and clobber each other's saves.

This is reader-lifecycle bookkeeping, not application state — it **moves into the crate**
as its own registry rather than becoming trait methods.

### 2c. `Widgets` — two uses

- `widgets.window` — transient parent for the reader window and its dialogs
- `toast(&widgets, msg)` — 56 calls, all transient confirmations and errors

### Shared helpers to move

`popover_button` (4 calls) and `popover_separator` (4 calls) are small, dependency-free
UI helpers used by the reader's context menus. They move into the crate. Everything else
the reader calls is already inside the extracted region.

### What the reader does *not* touch

No `reload_current`, `refresh_detail`, `refresh_list`, `select_key`, or `Config`. There is
no call back into the application's own view state at all. This is why the extraction is
tractable.

---

## 3. The boundary

```rust
/// Everything the reader needs from whoever embedded it.
///
/// One instance per open document, constructed by the host, which is what lets the reader
/// stay ignorant of how the document is identified — Kartoteka resolves a citation key,
/// Sputnik resolves either a library key or a local course material.
pub trait ReaderHost {
    fn load_annotations(&self) -> Result<AnnotationSidecar, String>;
    fn save_annotations(&self, sidecar: &AnnotationSidecar) -> Result<(), String>;

    /// Persist reading position. Called on close, best-effort — a failure is not surfaced.
    fn save_progress(&self, progress: Progress);

    fn page_label_override(&self) -> Option<PageLabelOverride>;
    fn set_page_label_override(&self, value: Option<PageLabelOverride>);

    /// Transient confirmation or error, shown however the host shows those.
    fn notify(&self, message: &str);
}
```

Six methods. No citation keys, no `Library`, no `Note`, no Hayagriva.

**Kartoteka's impl** closes over the open `Library` and the entry's key, and routes
`notify` to its `ToastOverlay`.

**Sputnik's impl** will route by reading kind: a `key:` reading goes to the Kartoteka
vault's `notes/<key>.md` and `annots/<key>.json`; a `material:` reading goes to its own
`annots/<blake3>.json` (`sputnik/docs/ARCHITECTURE.md` §3). Same trait, two routings.

Document identity stays where it already is — an explicit parameter. Both entry points
already take it:

```rust
show_pdf_reader(host, parent, hash, blob, title, start_page)
show_epub_reader(host, parent, hash, blob, title, start_annotation_id, start_progress)
```

(Currently `(state, widgets, key, hash, blob, title, …)`; `key` disappears into the host,
`state`/`widgets` collapse to `host` plus a parent window.)

---

## 4. Planned module layout

Done (steps 1.2–1.3):

```
kartoteka-ui-gtk/src/ui/reader/
├── mod.rs           93 lines — ReaderHost, the open-window registry, shared helpers
├── pdf.rs        2,946 lines — ReaderState and the PDF reader
├── epub.rs       1,511 lines — EpubReaderState and the EPUB reader
└── annotations.rs  304 lines — the annotations dialog
```

`app_window.rs` went from 15,825 to 10,973 lines. The relocation was verified to be
content-identical: the extracted region at the previous commit diffs to zero against the
concatenated new files, modulo blank lines and the `pub(crate)` markers added for the four
cross-module items (`show_pdf_reader`, `show_epub_reader`, `show_annotations_dialog`, and
the constants `epub.rs` shares with `pdf.rs`).

`popover_button` and `popover_separator` are deliberately *copied* into `mod.rs` rather
than imported from `app_window` — eighteen lines is not worth a dependency back on the
application from a module whose whole point is leaving it.

Step 1.4 is then `git mv` of this directory into `crates/fond-read-gtk/src/`, with
Kartoteka consuming it by path. License: **proprietary** (`LicenseRef-Proprietary`), per
Cal's decision 2026-09-09 — see the `fond-` prefix note in `docs/LICENSES.md`.

The one thing 1.4 still has to solve: these modules are `pub(crate)` and reference
`fond_bib` types directly. As a separate crate they become `pub`, and the annotation types
(`AnnotationSidecar`, `Progress`, `PageLabelOverride`) come from `fond-bib` — see §6.

---

## 5. Verification

Neither step changes behaviour, so the check is the same for both, and is the Phase 0
hand-test checklist:

Open a PDF and an EPUB; highlight, underline, strike, freestanding note; text selection
and copy; in-document search with match cycling; continuous scroll; facing pages;
undo/redo; page-label override; annotation export to Markdown; close and reopen to confirm
resume position; open the same document twice and confirm it surfaces the existing window
rather than opening a second one.

Then: confirm a sidecar written after the change is byte-compatible with one written
before it.

---

## 6. Noted, not done

`AnnotationSidecar`, `Annotation`, `AnnotationKind`, `Progress` and `PageLabelOverride`
live in `fond-bib`, so `fond-read-gtk` depends on the *bibliography* crate for types that
are really about documents. `fond-doc`'s stated remit ("PDF/EPUB rendering, text
extraction, and annotations") is where they arguably belong.

Moving them is a breaking change for every `fond-bib` consumer, Zerkalo included, for no
behavioural gain — so it is deliberately out of scope here. Worth revisiting if the shared
crates are ever versioned independently.
