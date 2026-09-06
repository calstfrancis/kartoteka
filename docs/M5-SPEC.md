# Kartoteka M5 — The Full Reader: PDF + EPUB Reading & Portable Annotation

Status: **All four tiers complete.** Tier 1 and Tier 2 (including continuous scroll and
printed-page-number-aware page numbers), a further round of PDF reader UX polish (Contents sidebar, live
drag preview, undo/redo, editable "This page" notes), Tier 3 (Markdown annotation export), and
Tier 4 (multi-attachment chooser) — all built and tested end-to-end.

> **Naming note.** "M5" here is the next step in the **knowledge-base extension track**
> (M2 = typed relations/facets/AI sidecar/projects, M3 = knowledge-graph nodes, M4 = live PDF
> annotation + Tier 2 GUI items — all built, see `docs/STATUS.md`), *not* `ROADMAP.md`'s
> "Milestone 5" (acquisition, long done). This milestone picks up the one open thread
> `M4-SPEC.md` left dangling — "underlines/strikeouts still have no drawing gesture" — and
> goes further: EPUB currently has **no reader at all**, only metadata import. Prompted by
> Cal: "look deeply at PDF and EPUB reading, make Kartoteka the go-to place to read and
> annotate either, with an annotation system readable outside the file."

## Where this milestone came from

An audit of `fond-doc` (PDFium bindings, rendering, text-run selection, annotation
embed/extract), `fond-bib`'s annotation/note sidecar, `fond-index`'s indexing pipeline, and
`show_pdf_reader` in `kartoteka-ui-gtk`. Two findings drove the shape of this doc:

1. **PDF is already a strong foundation** — real text-run-anchored
   highlighting, a portable JSON sidecar as the *authoritative* annotation record (the PDF
   itself is explicitly documented as "the disposable copy," see `fond-bib/src/annotation.rs`
   header comment). What's missing is reader ergonomics (search, outline, continuous scroll)
   and the annotation *kinds* the schema already supports but the UI can't draw.
2. **EPUB has zero reading capability, and one live bug.** `fond-doc::epub.rs` parses only
   OPF Dublin Core (title/author/date/publisher/ISBN) — it never touches chapter content.
   There is no EPUB rendering anywhere in the app. Worse: the "Read" button's attachment
   lookup (`present_pdf` in `app_window.rs:3893`) does an untyped `find_map` over an entry's
   attachments — the first attachment found, of *any* file type, is treated as "the PDF."
   Attach an EPUB and "Read" opens `show_pdf_reader`, which hands EPUB bytes to PDFium, fails
   to parse, and silently shows a blank "Page 1 of 1" window — no error, no fallback. There is
   currently no working path, built-in or external, to reliably view an EPUB's content from
   inside Kartoteka.

Given that, "the go-to place to read and annotate either format" is two different sizes of
work: PDF needs polish, EPUB needs to be built from nothing.

---

## Current state, grounded

### PDF — real reader, real gaps

`show_pdf_reader` (`kartoteka-ui-gtk/src/ui/app_window.rs:4724`): PDFium raster render,
page nav, zoom, click-drag highlighting via `fond_doc::select_text_in_rect`
(`fond-doc/src/pdf.rs:158`), which correctly hugs multi-line text selections rather than
using PDFium's `chars_inside_rect` (documented as deliberately wrong for multi-line drags).
Highlights persist to `annots/<key>.json` (`fond_bib::AnnotationSidecar`) with page +
quadpoints + snippet + prefix/suffix context, so a highlight re-anchors even against a
differently-produced copy of the same PDF.

Missing:
- Only `AnnotationKind::Highlight` can be *drawn*; `Underline`/`Strikeout`/`Note` exist in
  the schema and in PDF import/export (`fond-doc/src/annotation.rs`) but have no UI gesture.
- No in-reader text search, no outline/bookmarks panel, no page thumbnails, no continuous
  scroll (strictly page-by-page), no keyboard navigation, no numeric "go to page" entry.
- Highlight color is hardcoded (`HIGHLIGHT_RGBA`, amber only) — no picker.
- Annotations can only be edited/deleted via a separate modal (`show_annotations_dialog`,
  `app_window.rs:4508`), not inline while reading.
- `fond_bib::Progress` (page/of, `note.rs:38`) exists in the note schema and is editable by
  hand in the note editor, but `show_pdf_reader` never reads or writes it — "Read" always
  opens at page 1 (except when launched from the annotations dialog's "Go to page", which
  passes an explicit `start_page`), and closing the reader never saves where you left off.

### EPUB — metadata only

`fond-doc::epub.rs` reads `META-INF/container.xml` → OPF `<metadata>` Dublin Core fields.
That's the entire surface. No spine/manifest parsing, no chapter extraction, no rendering.
`kartoteka-ui-gtk` has no EPUB-specific reader UI at all.

Confirmed bug: `present_pdf` (`app_window.rs:3893`) has no attachment-type filter —

```rust
let present_pdf = note.as_ref().and_then(|n| {
    n.frontmatter.attachments.iter().find_map(|att| {
        // ...
        path.exists().then(|| (path, att.filename.clone(), att.hash.clone()))
    })
});
```

— so an EPUB attachment is treated identically to a PDF one everywhere `present_pdf` gates a
feature: "Read" (`app_window.rs:3924`), "Annotations…" (`app_window.rs:4041`), and "Open
externally" (`app_window.rs:4089`). "Open externally" happens to work by accident (any file
type launches fine via `gtk4::FileLauncher`), but "Read" opens the PDF viewer against EPUB
bytes and fails silently.

This same untyped lookup is also why **multi-attachment entries are broken today for both
formats**: `Attachment` is already stored as a `Vec` (`note.rs:26`), but only the
first-found attachment is ever reachable through Read/Annotations/Open externally — an
entry with both a PDF and an EPUB of the same work would only ever expose one of them.

Not indexed either: `SearchIndex::rebuild` (`fond-index/src/index.rs:139`) takes a
`pdf_text` callback only — EPUB chapter body text is never extracted or indexed, so
full-text search never reaches a book's actual content, only its bibliographic metadata.

---

## Tier 1 — EPUB reading, from nothing

### 5A. Stop the silent EPUB-reader failure — **done.**

`app_window.rs`'s attachment lookup is now typed (`ReaderAttachmentKind::{Pdf,Epub}`,
by filename extension) instead of the old untyped "first attachment of any kind." An EPUB
attachment now routes "Read" to the new EPUB reader (5B) rather than silently failing in
`show_pdf_reader`; "Annotations…" stays gated to `Pdf` (annotation *creation* is still
PDF-only, 5C isn't built); "Open externally" stays untyped, as it should — it works for any
file type.

### 5B. EPUB rendering — **done, verified end-to-end.**

Built as recommended below: **embedded WebKitGTK 6** (`webkit6 = "0.2"` — the release that
happens to pin to this app's existing `gtk4 0.7`/`glib 0.18`/`libadwaita 0.5`, so no
ecosystem version bump was needed; confirmed by resolving newer `webkit6` releases first,
which pull `gtk4 0.11+`, a much bigger jump not worth taking for this). Added only to
`kartoteka-ui-gtk/Cargo.toml` — `fond-doc` stays GTK-free per `ARCHITECTURE.md` §4.

- `fond-doc::epub` gained `open_book()` (OPF manifest + spine parsing, reading order,
  `linear="no"` exclusion), an EPUB3 nav / EPUB2 NCX table-of-contents parser (flattened,
  not tree-shaped — a jump list, not an outline widget), and `extract_all()` (unzips to a
  destination dir preserving structure, via `zip`'s own `.extract()`). Href resolution
  handles `../` segments, percent-decoded filenames, and `#fragment` anchors. 14 new unit
  tests, including two full synthetic-EPUB `open_book()` round-trips (one EPUB3 nav, one
  EPUB2 NCX fallback) and a `linear="no"` exclusion case.
- `kartoteka-ui-gtk::show_epub_reader`: extracts the EPUB to a content-hash-addressed cache
  dir (`~/.cache/kartoteka/epub/<hash>/`, extracted once, reused on repeat opens), then
  serves chapters over `file://` via `WebView::load_uri` — deliberately *not* a custom
  WebKit URI-scheme handler, so a chapter's own relative links to sibling images/CSS resolve
  the ordinary browser way with zero extra plumbing. Prev/next chapter buttons and a
  "Contents" popover (from the parsed TOC) both funnel through one `epub_go_to()` that
  updates the current-chapter index, the "Chapter i of N" label, and button sensitivity —
  the same single-navigation-path pattern `show_pdf_reader`'s `render()` closure already
  uses for PDF pages.
- **Verified for real, not just compiled**, with a standalone headless harness (Xvfb +
  `GDK_BACKEND=x11` + unset `WAYLAND_DISPLAY` + `LIBGL_ALWAYS_SOFTWARE=1` + `dbus-run-session`
  — the same recipe `CLAUDE.md`'s "Common gotchas" already documents for this machine) driving
  a real `webkit6::WebView` against a synthetic multi-chapter EPUB with a subdirectory image
  and a stylesheet: confirmed `load-changed` reaches `Finished`, confirmed via
  `evaluate_javascript` that the chapter's actual text content is present in the rendered DOM
  (not just that the request didn't error), and confirmed the sibling `images/cover.png`
  resolved and decoded (`naturalWidth: 1, complete: true`) through a real subdirectory
  relative reference — the exact case a hand-rolled resource loader would be most likely to
  get wrong. One real gotcha surfaced and is worth remembering: WebKit serves a `.xhtml` file
  as `application/xhtml+xml` (correct), which uses the strict XML parser — elements only get
  `HTMLImageElement`/etc. DOM interfaces (not a bare `Element`) if the document's root `<html>`
  declares `xmlns="http://www.w3.org/1999/xhtml"`. Every real EPUB does (it's spec-mandated),
  so this isn't a real-world risk, but it's the thing to check first if a chapter ever renders
  text fine but images/links behave oddly.
- Read-only: no highlighting yet (that's 5C, a separate anchor scheme since there's no PDF-
  style page grid to hang quadpoints on).

Full workspace gate after 5A+5B: **all tests passing** (`cargo test --workspace`), **0
clippy warnings** (`cargo clippy --workspace --all-targets`).

What 5B's recommendation looked like before landing, kept for context on the choice made:
embed WebKitGTK 6 rather than hand-build an XHTML layout/reflow engine — gets images,
embedded fonts, and footnotes correct for free, and WebKit's DOM Range/Selection API is the
hook 5C's highlight creation will eventually need (analogous to how `pdfium-render` already
gives PDF text-run selection).

### 5C. EPUB annotation anchor + sidecar extension — **done, verified end-to-end.**

Built as recommended below: `fond_bib::Annotation` extended in place rather than forked —
`page` became `Option<u32>` (backward-compatible: `Option<T>` deserializes a bare integer
from every existing sidecar file fine, and a `Some` value serializes identically to the old
bare field) and a new `chapter: Option<String>` (a zip-internal path, same shape as
`fond_doc::EpubBook::spine`) carries the EPUB anchor. `page`/`quadpoints` stay `None`/empty
for an EPUB annotation; `chapter` stays `None` for a PDF one — one shared type, one sidecar
file, one "Annotations…" dialog for both formats, as scoped. A new
`Annotation::drawn_epub()` constructor parallels `drawn()`/`imported()`.

No EPUB CFI — the plain chapter + snippet + prefix/suffix scheme was enough; not revisited.

The reader side reuses WebKit's own native text selection (the user drags the ordinary
browser way) rather than a custom selection UI:
- **Capture** (`EPUB_CAPTURE_SELECTION_JS`, `app_window.rs`): reads `window.getSelection()`
  and computes the selection's absolute character offset by walking `document.body`'s text
  nodes, then slices 40 characters of prefix/suffix context around it.
- **Apply** (`EPUB_APPLY_HIGHLIGHTS_FN`): the reverse walk — finds each saved annotation's
  snippet (preferring the prefix+snippet+suffix combined string when present, to disambiguate
  a snippet that appears more than once in the chapter) and wraps it in a
  `<mark class="kartoteka-hl">` via `Range.surroundContents`, clearing previously-applied
  marks first so re-applying (after adding a highlight, or on every chapter (re)load) is
  idempotent rather than double-wrapping. This is the DOM-search-and-wrap equivalent of the
  PDF reader's `blend_highlights` — no bitmap to blend into here, WebKit owns the render.
- A "Highlight" button in the EPUB reader's header runs capture → `Annotation::drawn_epub` →
  `library.write_annotations` → re-apply, the same save-then-redraw shape
  `show_pdf_reader`'s drag-end handler already uses.
- `show_annotations_dialog` is now format-aware (`ReaderAttachmentKind` threaded through):
  location label shows "Page N" or the chapter filename, "Go to page"/"Go to chapter" opens
  the right reader — for EPUB, `show_epub_reader(..., Some(&annotation.id))` opens on that
  annotation's chapter and scrolls its highlight into view.
- The `has_annotations`/"Annotations…" gate in the detail pane no longer restricts to PDF —
  EPUB entries can have annotations too.

**Verified for real** with a standalone headless WebKit harness (same Xvfb recipe as 5B),
running the actual `EPUB_APPLY_HIGHLIGHTS_FN`/`EPUB_CAPTURE_SELECTION_JS` source against a
two-paragraph test document where the *same phrase* appears in both paragraphs — the hardest
case for snippet-based anchoring. Confirmed: selection capture correctly disambiguated which
occurrence was selected (prefix/suffix matched the right paragraph); applying the highlight
placed the mark in the correct paragraph only; re-applying the same annotation left exactly
one mark (idempotent, no double-wrap); and applying two annotations together correctly marked
both paragraphs without either interfering with the other.

### 5D. EPUB text extraction for the search index — **done, verified end-to-end.**

`fond_doc::epub::extract_text()` (aliased `fond_doc::extract_epub_text`) reads each spine
chapter's XHTML and strips tags via a small `quick-xml` walk (skipping `<script>`/`<style>`
contents), joining chapters the same way `PdfText::full_text()` joins PDF pages. Chose a
dependency-free tag-stripper over asking WebKit for `innerText`, since extraction needs to
run from the CLI's `reindex` command too, which has no `WebView` to ask.

`fond_index::SearchIndex::rebuild()` gained a second callback parameter, `epub_text`,
parallel to the existing `pdf_text` — both `FnMut(&str) -> Option<String>`, both optional
(an entry with neither, or an extractor that errors, just gets an empty field, same as
always). A new `epubtext` schema field joins `pdftext` in both the index schema and the
default query fields, so free-text search reaches it without any new query syntax. The CLI's
`reindex` command now extracts both PDF and EPUB text per entry (EPUB extraction needs no
availability gate, unlike PDF's PDFium-presence check — pure `zip`+`quick-xml`, always
available). All `rebuild()` call sites (CLI, GUI, tests) updated for the new parameter.

---

## Tier 2 — PDF reader parity and polish — **done, verified end-to-end.**

Independent of Tier 1; landed after it. Every item built and confirmed working in a real
running instance (headless Xvfb, xdotool-driven — screenshots of each interaction), not just
compiled:

- **Progress wiring — done.** "Read" now resumes at `fond_bib::Progress.page` (looked up
  from the entry's note at the detail-pane call site) instead of always page 1; the reader
  saves `page`/`of` back on `connect_close_request`. Confirmed on disk: closing on page 2
  wrote `progress: {page: 2, of: 2}` to `notes/<key>.md`.
- **Underline/Strikeout/free-note drawing gestures — done.** A mode `DropDown`
  (Highlight/Underline/Strikeout) in the reader header drives `ReaderState.draw_kind`, read
  by the existing `GestureDrag` handler instead of a hardcoded `Highlight`. Rendering gained
  `fond_doc::{MarkupKind, blend_annotations}` — Underline/Strikeout narrow each quad to a
  thin band (near the bottom, or through the middle) before blending, rather than filling
  the whole quad; verified both visually (a screenshot shows a real thin green line struck
  through rendered text, not a filled block) and with two new `fond-doc` pixel-level tests.
  `Note` has no on-page quad (it's marginal) — a separate "Note…" button opens a small
  text-entry dialog, saving via the same `Annotation::drawn` path with empty quadpoints.
- **PDF outline/bookmarks panel — done.** `fond_doc::pdf::outline()` reads the bookmark tree
  via `pdfium-render`'s `PdfBookmark` API (`PdfBookmarks::iter()` already gives document
  order; depth is the ancestor-count via `.parent()` chain walking, since the iterator gives
  order but not depth directly), flattened like `fond_doc::epub::TocEntry`. A "Contents"
  popover (only shown when the PDF actually has one — most don't) jumps to a bookmark's
  page. Verified with a real hand-built PDF containing a nested `/Outlines` tree (two
  PDFium-backed tests: document order + depth + page resolution; empty-outline case) and
  confirmed live — the popover showed "Chapter One" / indented "Section 1.1" and jumped
  correctly.
- **In-reader PDF text search — done.** `fond_doc::pdf::search_document()` walks every page's
  `PdfPageText::search()`, returning match page + quads in document order (verified by two
  PDFium-backed tests). A search bar (`SearchEntry` + prev/next + count label) runs on Enter,
  jumps to the first match's page, and blends the current match in a colour
  (`SEARCH_MATCH_RGBA`, blue) distinct from saved highlights. Confirmed live: searching
  "treasure" jumped to the right page, highlighted the exact word, and showed "1 of 1".
- **Inline annotation sidebar + colour picker — done.** A colour `DropDown` (five presets:
  Amber/Green/Blue/Pink/Red) sets `ReaderState.draw_color`, threaded into
  `Annotation::drawn`'s new `color` parameter (`None` falls back to the original amber, so
  existing callers/sidecars are unaffected); `render()` now blends each annotation in *its
  own* stored colour (parsed via a small hex-to-RGBA helper) rather than one shared constant
  for every highlight on the page. A "This page" popover (rebuilt fresh on every open, since
  content is page-dependent) lists the current page's annotations with inline delete —
  addresses the Tier 2 ask without replacing the existing whole-document
  `show_annotations_dialog`, which is still the only whole-document view. Confirmed live: a
  green strikeout and a note both appeared in the popover with working delete, and the
  on-disk sidecar recorded `"color": "#8bc34a"` correctly.
- **Continuous/vertical scroll — done, as a toggle alongside the page-by-page view.** A
  "Continuous" button switches a `gtk4::Stack` between the original single-`Picture`
  `ScrolledWindow` and a new vertical `Box` of per-page `Picture` widgets in its own
  `ScrolledWindow`. Deliberately **eager, not virtualized**: on first toggle-on, every page
  is rendered up front (a one-time, sub-second cost for the page counts this app actually
  sees — academic papers, book chapters) rather than lazily as pages scroll into view. This
  trade was made on purpose — it avoids the correctness risk a virtualized `ListView` would
  carry: each page gets one *permanent* widget, so its drag-to-annotate gesture captures that
  page's index by value and can never end up stale after a recycled row gets reassigned to a
  different page. `ReaderState` gained `continuous_pictures`/`continuous_offsets` (each
  page's cumulative top position, for scroll-to-page and for tracking which page is
  "current" from scroll position via `vadjustment.connect_value_changed`)/
  `continuous_rendered`. `render_pdf_page_texture()` was extracted as the one shared
  rendering path both views call, so they can never visually disagree; `save_drag_annotation()`
  was similarly extracted so per-page drag handlers in both views share the same coordinate
  math and sidecar write. Every navigation surface (prev/next, Contents, search jump/prev/
  next) is mode-aware, branching on `continuous_toggle.is_active()`. Zoom changes tear down
  and rebuild the continuous view (simpler than resizing in place; zoom changes are
  infrequent). **Verified end-to-end headless** against a real 6-page PDF: toggled on,
  scrolled with the mouse wheel and watched the page label track correctly across the
  page-1/page-2 boundary; drew a highlight directly on page 3 *while the tracked "current"
  page was 2*, confirming the sidecar recorded `"page": 3` — the exact case a
  recycled-widget bug would get wrong; ran a search that jumped to page 5 with the match
  blended in its own colour; toggled back to paged mode and confirmed both the search
  highlight and the earlier page-3 drawn highlight were still there, correctly rendered by
  the same shared code path.
- **Document page numbering — done, verified end-to-end (added beyond the original Tier 2
  list, prompted by "show the document's own printed page number, not the raw file
  position — can Kartoteka?").** PDFium already wraps this directly: a PDF's `/PageLabels`
  catalog entry (a number tree mapping page ranges to a numbering style — decimal, upper/
  lower-case roman, upper/lower-case letters — plus an optional prefix and start value) is
  exposed as `PdfPage::label()`. `fond_doc::pdf::page_labels()` reads every page's label
  once at reader-open (`None` where the PDF defines none — most PDFs). The static "Page X of
  Y" label became an editable `Entry` showing the *document's* printed number (falling back
  to the raw file position when there's no `/PageLabels`, identical to the old behaviour) —
  the entry's tooltip always gives the raw number too, since `Annotation.page`/search
  matches/Contents targets all mean the raw position internally, never the printed one.
  Typing a value and pressing Enter jumps to it: an exact label match first
  (case-insensitive, so a roman numeral typed in the wrong case still resolves), falling
  back to parsing it as a raw 1-based page number — so a PDF with no custom numbering
  navigates exactly as before. Shared by both the paged view's `render()` and continuous
  mode's scroll-position tracker via one `update_page_display()` helper, so the two can't
  disagree. Two new `fond-doc` tests against a hand-built PDF with a real `/PageLabels` tree
  (mixed roman front matter + arabic body restarting at 1, and the no-labels case). Verified
  live: a 4-page test PDF showed "i", "ii", "1", "2" correctly across its pages (not "1",
  "2", "3", "4"); typing "ii" jumped straight to the Table of Contents page; typing "3" (no
  page is labelled "3" in this document) correctly fell back to the raw file position,
  landing on the page labelled "1".

## PDF reader UX round 2 — sidebar, live preview, undo/redo, editable notes — **done, verified end-to-end.**

Prompted by Cal after Tier 2 shipped: "contents nav should be in a sidebar. there needs to be
a better indication of what you're highlighting when highlighting — right now the highlight
appears after you finish dragging and let go. make sure there are undo/redo functions, and
the ability to edit and remove highlights and notes/annotations." Four PDF-reader-scoped
changes (EPUB's own separate reader/Contents popover was left as-is — this request was about
the PDF reader specifically, matching the whole conversation's focus):

- **Contents sidebar.** The header `MenuButton`+`Popover` outline list became a persistent
  `gtk4::Paned`, toggled by a new headerbar button (`sidebar-show-symbolic`) at the *start* of
  the headerbar, per the house sidebar style — collapsed by default, stays open across page
  jumps instead of closing after every click. Implementation note: `content` (the search
  bar + hint + page/continuous view stack) must *not* be parented into the `ToolbarView` before
  being reparented into the `Paned`'s end-child — `Paned::set_end_child` asserts the child has
  no existing parent, so `view.set_content(&content)` is skipped whenever a sidebar exists,
  deferred until the `Paned` itself becomes the view's content.
- **Live drag preview.** Both the paged view's `Picture` and each continuous-mode page's
  `Picture` are now wrapped in a `gtk4::Overlay` with a semi-transparent, non-targetable
  `DrawingArea` on top (`build_drag_preview_overlay`), fed by a shared `Rc<Cell<Option<(f64,
  f64, f64, f64)>>>` rectangle that `connect_drag_begin`/`connect_drag_update` update and
  `queue_draw()` on — so the drag rectangle is visible in the current draw colour throughout
  the gesture, not just after `drag_end`. The preview draws the raw drag rectangle (not the
  final narrowed underline/strikeout band `blend_annotations` produces) — close enough to show
  intent without duplicating that geometry.
- **Undo/redo.** Snapshot-based: `ReaderState` gained `undo_stack`/`redo_stack: Vec<
  AnnotationSidecar>` (capped at `UNDO_HISTORY_LIMIT = 50`). `push_undo_snapshot()` is called
  at the top of every mutation site — drag-created highlight/underline/strikeout
  (`save_drag_annotation`), a note added via the "Note…" dialog, an annotation deleted from
  "This page", and a note edited from "This page" — and clears the redo stack (standard editor
  convention: a fresh edit invalidates pending redo). Undo/Redo header buttons
  (`edit-undo-symbolic`/`edit-redo-symbolic`) plus Ctrl+Z / Ctrl+Shift+Z via an
  `EventControllerKey` on the reader window; `sync_undo_redo_buttons()` keeps their
  sensitivity current after every mutation. A snapshot doesn't record which page(s) it
  touched, so undo/redo re-renders every page (`rerender_all_pages`) — cheap, since each
  page's blend is just a texture re-render, not a re-parse of the PDF.
- **Editable "This page" notes.** The inline per-page annotation list gained the same
  save-on-Enter/blur note `Entry` the whole-document "Annotations…" dialog already had,
  closing the parity gap between the two — previously "This page" could only show a note
  preview and delete, not edit one.

Verified live under Xvfb: sidebar toggle shows/hides and its row clicks jump pages correctly
while staying open; a slow multi-step drag shows the live amber rectangle mid-gesture and
resolves into the normal saved highlight on release; creating a highlight enables Undo (Redo
stays disabled), Undo removes it and enables Redo, Redo restores it, and Ctrl+Z reproduces the
button's effect; editing a "This page" note via Enter persists across popover reopens and is
itself undoable via Ctrl+Z.

## Tier 3 — annotation portability ("read annotations outside the file") — **done, verified end-to-end.**

The JSON sidecar was already the authoritative record and already human-readable — the gap
was a *presented* export, not the underlying data model:

- **A portable Markdown export**: `AnnotationSidecar::
  to_markdown(entry_title)` in `fond-bib` (UI-agnostic, per the crate-boundary rule — no GTK
  types) renders an entry's whole sidecar into readable prose, one `##` heading per PDF page
  or EPUB chapter, the highlighted text as a `>` blockquote (omitted for a plain
  drag-rectangle highlight with no text under it — `snippet` is `None` there), then the note
  below it. PDF annotations (page-anchored) are ordered by page; EPUB annotations
  (chapter-anchored, `page` always `None`) are ordered by chapter path then creation time,
  kept in a separate block after the PDF ones rather than interleaved by a shared sort key,
  since "page 3" and "chapter 3" have no meaningful relative order. An empty sidecar still
  renders a titled, `_No annotations._` file rather than nothing. Five `fond-bib` unit tests
  cover the empty case, quote+note rendering, quote omission with no snippet, and PDF-before-
  EPUB ordering.
- An **"Export…" button** in the "Annotations…" dialog's header (`show_annotations_dialog`,
  `app_window.rs`) reloads the sidecar fresh from disk at click time (not the dialog's own
  possibly-stale capture, in case a note was edited or an annotation deleted earlier in the
  same dialog session), renders it via `to_markdown`, and saves through the same
  `gtk4::FileDialog` save flow "Export bibliography…" already uses, defaulting the filename
  to `<key>-annotations.md`.
- `show_annotations_dialog` already displayed both PDF- and EPUB-anchored annotations from
  the same sidecar uniformly once 5C landed (its `location` match on `page`/`chapter` already
  handled both) — nothing further needed there.

## Tier 4 — multi-attachment correctness — **done, verified end-to-end.**

5A already made the reader lookup typed (`ReaderAttachmentKind`), but it was still a single
*first-match* over all attachments (`present_reader_attachment`, singular) — an entry with
both a PDF and an EPUB of the same work only ever exposed whichever one the attachments list
happened to list first, silently.

Fixed by looking up each readable kind independently: `readable_attachment_of(wanted:
ReaderAttachmentKind)` in `show_detail` (`app_window.rs`) is called once for `Pdf` and once
for `Epub`, giving `pdf_attachment`/`epub_attachment: Option<(PathBuf, filename, hash)>`
instead of one combined option. Two attachments of the *same* kind (two PDFs) isn't a
supported case — the sidecar's single `pdf_hash` field can't disambiguate between them — so
this still takes the first match per kind, not a full list.

- **"Read" button.** Exactly one of the two present → the same plain `Button` as before,
  unchanged behaviour for the common single-attachment case. Both present → a `MenuButton`
  with a small hand-built popover ("PDF — `<filename>`" / "EPUB — `<filename>`"), so the user
  picks instead of Kartoteka silently guessing. Neither present → "Find PDF" (DOI/Unpaywall),
  unchanged.
- **`show_annotations_dialog`** now takes `pdf_attachment`/`epub_attachment: Option<(hash,
  blob)>` independently instead of one `(kind, hash, blob)` for the whole dialog. This also
  fixed a latent bug the single-attachment case never exposed: every row's "Go to" button used
  to match on that one dialog-wide `kind`, so on a mixed PDF+EPUB entry *every* row — including
  the EPUB-anchored ones — would have routed through `show_pdf_reader` with the PDF's
  hash/blob. Each row now decides its own format from the annotation itself
  (`annotation.page.is_some()` → PDF, else EPUB) and looks up the matching attachment
  independently; if that annotation's format's attachment isn't currently present, its "Go
  to" button is shown disabled with a tooltip explaining why, rather than hidden or (worse)
  silently wired to the wrong reader.
- **`has_annotations`** (gating whether the "Annotations…" row appears at all) now checks
  `pdf_attachment.is_some() || epub_attachment.is_some()` instead of the old single option.

Verified live under Xvfb with a synthetic entry carrying both a real PDF (with an existing
highlight) and a hand-built minimal EPUB: "Read" rendered as a `MenuButton`, its popover
listed both filenames by format, clicking "PDF" opened the PDF reader and clicking "EPUB"
opened the EPUB reader (confirmed by each window's distinct chrome — page-number entry +
Highlight/Underline/Strikeout mode picker for PDF vs. chapter nav for EPUB). After adding an
EPUB highlight alongside the existing PDF one, "Annotations…" showed both rows correctly
labelled ("Page 1 · Highlight" → "Go to page", "chap1.xhtml · Highlight" → "Go to chapter"),
and clicking "Go to chapter" opened the EPUB reader with that highlight visible — confirming
per-row routing, not the dialog-wide `kind` the old code used.

---

## Suggested order

1. **5A — done.** Stopped the silent EPUB failure.
2. **5B — done.** EPUB rendering + chapter/TOC navigation via WebKitGTK 6, verified
   end-to-end headless (real chapter text + a subdirectory image resource resolved
   correctly). Read-only — no highlighting.
3. **5C — done.** EPUB-shaped annotation anchor (chapter + text snippet, no quadpoints) and
   highlighting in `show_epub_reader` via WebKit's own native selection + DOM search-and-wrap,
   verified end-to-end headless including the ambiguous-snippet disambiguation case.
4. **5D — done.** EPUB body text into the search index, verified via a new `fond-index` test.
5. **Tier 1 — done.**
6. **Tier 2 — fully done.** Progress wiring, underline/strikeout/note gestures, outline
   panel, in-reader search, colour picker + inline per-page annotation list, and
   continuous/vertical scroll (a toggle alongside the page-by-page view, eagerly rendered
   rather than virtualized) — all built and verified end-to-end headless.
7. **Tier 3** — next up. A portable Markdown/plain-text annotation export, straightforward
   now that 5C's schema extension exists for both formats.
8. **Tier 4** — multi-attachment chooser, small, low urgency until an entry with two
   attachments actually shows up in practice.
