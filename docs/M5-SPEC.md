# Kartoteka M5 — The Full Reader: PDF + EPUB Reading & Portable Annotation

Status: **Tier 1 complete (5A–5D built and tested). Tier 2–4 not started.**

> **Naming note.** "M5" here is the next step in the **knowledge-base extension track**
> (M2 = typed relations/facets/AI sidecar/projects, M3 = knowledge-graph nodes, M4 = live PDF
> annotation + Tier 2 GUI items — all built, see `docs/STATUS.md`), *not* `ROADMAP.md`'s
> "Milestone 5" (acquisition, long done). This milestone picks up the one open thread
> `M4-SPEC.md` left dangling — "underlines/strikeouts still have no drawing gesture" — and
> goes further: EPUB currently has **no reader at all**, only metadata import. Prompted by
> Cal: "look deeply at PDF and EPUB reading, make Kartoteka the go-to place to read and
> annotate either, with a Zotero-alike annotation system readable outside the file."

## Where this milestone came from

An audit of `fond-doc` (PDFium bindings, rendering, text-run selection, annotation
embed/extract), `fond-bib`'s annotation/note sidecar, `fond-index`'s indexing pipeline, and
`show_pdf_reader` in `kartoteka-ui-gtk`. Two findings drove the shape of this doc:

1. **PDF is already a strong, mostly-Zotero-grade foundation** — real text-run-anchored
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

## Tier 2 — PDF reader parity and polish

Independent of Tier 1; can land in parallel or first if Tier 1's WebKit dependency needs
more deliberation.

- Underline/Strikeout/free-note drawing gestures — a mode toggle or modifier-key variant on
  the existing `GestureDrag` in `show_pdf_reader`; reuses `select_text_in_rect` and the
  sidecar-write path Tier-1-adjacent work doesn't touch. This is the one item `M4-SPEC.md`
  already flagged as an open follow-up to 4A.
- In-reader text search (PDFium exposes a text-search API over `PdfPageText`).
- Outline/bookmarks panel (PDFium exposes the document outline tree) and page thumbnails.
- Continuous/vertical scroll as an alternative to strict page-by-page.
- Wire `fond_bib::Progress`: resume at the saved page on "Read," save current page on
  navigate/close.
- Inline annotation list/sidebar in the reader itself, plus a highlight color picker,
  instead of routing every edit through the separate `show_annotations_dialog` modal.

## Tier 3 — annotation portability ("read annotations outside the file")

The JSON sidecar is already the authoritative record and already human-readable — the gap
is a *presented* export, not the underlying data model:

- A plain-text/Markdown export of an entry's annotations (à la Zotero's "add note from
  annotations") — one command/button that renders `annots/<key>.json` into readable prose
  (quote + page/chapter + your note, grouped and ordered), so annotations are consumable
  without opening the app or reading raw JSON.
- Once 5C lands, `show_annotations_dialog` should display both PDF- and EPUB-anchored
  annotations from the same sidecar uniformly (it's already entry-keyed, not
  attachment-keyed, so this is mostly "don't assume `page`+`quadpoints` are always present").

## Tier 4 — multi-attachment correctness

5A already made the reader lookup typed (`ReaderAttachmentKind`, `present_reader_attachment`
in `app_window.rs`) — what's still open is that it's still a *first-match*, not a chooser: an
entry with both a PDF and an EPUB of the same work only ever exposes whichever one the
attachments list happens to list first. Needs a small chooser when more than one readable
attachment is present, instead of silently picking one.

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
5. **Tier 1 is now fully done.** Next: **Tier 2** — PDF polish (search, outline,
   underline/strikeout gestures, `Progress` wiring), independently schedulable, no dependency
   on Tier 1.
6. **Tier 3** — annotation export (Markdown/plain-text, à la Zotero's "add note from
   annotations"), now that 5C's schema extension exists for both formats.
7. **Tier 4** — multi-attachment chooser, small, low urgency until an entry with two
   attachments actually shows up in practice.
