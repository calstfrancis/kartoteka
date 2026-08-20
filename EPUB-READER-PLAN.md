# EPUB reader/annotator — plan to reach PDF-reader parity

Status: **Phase 1 and 2 done** (2026-08-20, "go forward with the epub plan"). Phase 3
onward (zoom, in-book search, undo/redo, reading position) not started — see below.

## Done: Phase 1 + 2

- **Notes/highlights sidebar**, same pattern as the PDF reader's: every annotation in
  reading order, click to jump (loads the chapter if needed, scrolls the mark into
  view), inline note-comment editing, delete. Built via the same self-referential
  rebuild-cell idiom (`RebuildCell`) `show_pdf_reader` uses.
- **Contents sidebar** converted from a popover menu to the same persistent
  toggle-in-header / Paned-sidebar pattern as PDF's, mutually exclusive with Notes —
  matches CLAUDE.md's house sidebar style exactly now.
- **Three mark kinds** (Highlight/Underline/Strikeout) via a mode `DropDown` + a
  colour `DropDown` (reusing `COLOR_PRESETS`) + one "Apply" button, replacing the old
  single hardcoded "Highlight" button. Underline/strikeout use `currentColor` (no
  hardcoded palette, reads correctly in light and dark).
- Freestanding (unanchored) "Note" kind — deliberately **not** included; `drawn_epub`
  requires a text snippet to anchor on, and EPUB has no page-level note concept the way
  PDF does. Still an open question (see below), not a bug.

**Verified live** (headless Xvfb): chapter rendering, Contents-sidebar TOC navigation
(chapter label/prev-next update correctly), Notes-sidebar open/close and its empty
state, text selection. **Not independently verified live**: the actual
selection→annotation→disk round trip (`Apply` → `evaluate_javascript` →
`write_annotations`). In this sandboxed test environment, WebKit's
`evaluate_javascript` callback never fires at all — confirmed with a trivial `1+1`
probe outside any of this feature's own code — so no JS round trip could be observed
end-to-end here, including the pre-existing single-highlight flow this replaced.
Chapter loading and prev/next/TOC navigation (which don't depend on that callback) all
worked correctly live, and the JS itself is a small, direct extension of the
already-shipped `EPUB_CAPTURE_SELECTION_JS`/`EPUB_APPLY_HIGHLIGHTS_FN` (only the
per-kind styling and payload fields are new) — but this is worth Cal double-checking
in a real session before relying on it.

## Original plan (for context)

## Where things actually stand today

Kartoteka already has *two* working, separate readers, both reachable from an entry's
"Read" button (`ReaderAttachmentKind` picks by attachment type, or offers a chooser if
both exist):

- **PDF** (`show_pdf_reader`) — renders pages via PDFium into Cairo `Picture`s, page by
  page or in a "Continuous" scroll mode. Full toolset: Contents (outline) sidebar,
  persistent Notes/highlights sidebar, drag-to-select in four modes (select text /
  highlight / underline / strikeout / note), a highlight colour picker, in-document
  search with prev/next, zoom, undo/redo, page-label-aware navigation.
- **EPUB** (`show_epub_reader`) — renders one chapter at a time via WebKitGTK
  (`webkit6`), with a TOC sidebar (if the EPUB has one), prev/next chapter buttons, and
  exactly one interaction: select text → click **Highlight**. That's it — one
  annotation kind, one colour, no search, no zoom, no undo/redo, and no in-reader way to
  see/jump-to/delete highlights already made (that only exists via the separate,
  general "Annotations…" dialog elsewhere in the app).

So "works like the PDF one" is a real, specific gap, not a from-scratch build — the
EPUB reader's bones (chapter loading, TOC, one working highlight round-trip through
`fond_bib::AnnotationSidecar`) are there. What's missing is everything PDF has *around*
that core loop.

## Why this isn't a straight port: PDF and EPUB are different shapes

The single biggest reason this needs its own plan rather than "copy `show_pdf_reader`":
**PDF has pages; EPUB doesn't.** PDF's whole interaction model — page-by-page nav,
continuous-scroll-of-pages, drag-a-rectangle-on-a-raster-image, quad-geometry highlight
storage (`fond_bib::Annotation` stores page + quad points) — is built on "a page is a
fixed image with fixed coordinates." An EPUB chapter is reflowable HTML: no fixed
coordinates, no pages (until WebKit lays it out at a given window size, and that layout
changes with window resize, font size, zoom). Consequences:

- **Selection/highlighting must be DOM-based, not coordinate-based.** This already
  works today (`EPUB_CAPTURE_SELECTION_JS`, JS `Range`/`Selection` capture, applied back
  via an injected "apply highlights" function) — that mechanism is the right foundation
  to extend, not replace.
- **"Position" is a chapter + an anchor within it, not a page number.** Today's
  `Progress { page, of }` (`fond_bib::note::Progress`) is PDF-shaped. An EPUB reading
  position needs something like `(chapter index, character offset or CFI-ish DOM path)`
  — a real, if small, data-model addition.
- **"Continuous scroll" already basically exists** — WebKit renders a chapter as one
  scrollable flow. What PDF's "Continuous" toggle adds (stitching *pages* into one
  scroll) has no EPUB equivalent to build; the open question is whether "continuous
  across chapters" (auto-advance to the next chapter at the bottom of this one, so the
  whole book reads as one scroll) is wanted — see open questions below.
- **Search needs WebKit's own find API**, not PDFium's text-extraction search. WebKit
  has `WebView::find_controller()` with basic find-in-page support — different API
  surface than the PDF searcher (`fond_doc::search_document`), but conceptually the
  same feature.
- **Zoom** is CSS font-size/zoom-factor on the WebView, not a raster scale factor — an
  easier, more native fit for WebKit than for the Cairo `Picture` path PDF uses.

## Feature-by-feature plan

Each item: what PDF has, what it'd take for EPUB, rough size.

| Feature | PDF today | EPUB plan | Size |
|---|---|---|---|
| Multiple annotation kinds (highlight/underline/strikeout/note) | Mode picker, 4 kinds | Extend `EPUB_CAPTURE_SELECTION_JS`'s payload + the "apply" JS to render 3 more CSS treatments (underline, strikeout, and a margin/inline note marker) over the same `Range`-based anchor; add the same mode-picker UI PDF has | Medium |
| Highlight colour picker | `color_drop`, stored per-annotation | Same dropdown; colour becomes a CSS custom property on the injected `<mark>`/span wrapper | Small |
| Notes/highlights sidebar (browse, jump, delete) | Persistent sidebar, reuses `fond_bib::Annotation` list | Same sidebar widget/pattern, jump = load chapter (if different) then scroll-to-anchor via the existing "apply" JS's `scroll_to_id` path (already half-built) | Medium |
| In-book search | PDFium text search, prev/next, match count | WebKit `find_controller()` — search current chapter live; searching "the whole book" needs either pre-extracting all chapters' text (possible — `fond_doc` already extracts EPUB text for indexing) or an explicit "search searches this chapter only" scope decision (see open questions) | Medium |
| Zoom | Raster scale factor | WebView `zoom-level` (native GTK/WebKit property) — likely *simpler* than PDF's | Small |
| Undo/redo | Generic edit-stack over annotation ops | Same edit-stack pattern, EPUB ops (add/remove highlight) instead of PDF ops | Small–Medium (mostly reusing the existing undo/redo plumbing) |
| Reading position / progress | `Progress { page, of }` | New shape needed: chapter index + in-chapter anchor (percent-scrolled is the simplest robust option; a DOM-path/CFI is more precise but more fragile across re-renders). **Data-model decision needed** — see below | Medium (touches `fond_bib::note::Progress`) |
| Contents (TOC) sidebar | Persistent, toggle in header (only shown if the doc has an outline) | EPUB already renders TOC, just not as the same persistent-sidebar-with-toggle pattern PDF uses — bring it into the same `Adw` sidebar-toggle convention as PDF's `sidebar_toggle`/`notes_toggle` pair, per CLAUDE.md's own house sidebar style | Small |
| Continuous mode | Stitches pages into one scroll | No direct equivalent; closest analogue is "auto-advance to next chapter at scroll-bottom" — **needs a product decision**, not just an engineering one (see open questions) | Medium, *if wanted* |

## Suggested phases

1. ✅ **Notes sidebar + jump/delete for existing highlights.** Done 2026-08-20.
2. ✅ **More annotation kinds + colour picker.** Done 2026-08-20.
3. **Zoom + in-chapter search.** Both fairly self-contained, native WebKit features;
   good next slice once the sidebar exists to show search context in.
4. **Reading position / progress tracking**, once the chapter+anchor shape is decided.
5. **Undo/redo.** Do this after (2), once there's more than one kind of edit worth
   undoing — undoing "add highlight" alone is low value.
6. **Whole-book search + continuous-across-chapters scroll** (if wanted at all) — last,
   since both are the most speculative/highest-effort items and depend on product
   decisions below.

Each phase is independently shippable; nothing here blocks a dev build in between.

## Open questions for Cal (product decisions, not engineering ones)

1. **Reading position precision.** Percent-scrolled-through-current-chapter (simple,
   robust, slightly coarse) vs. a DOM-anchor/CFI-style position (precise, but more
   fragile if a chapter's HTML changes — e.g. re-extracting a re-downloaded EPUB).
   Recommendation: start with percent + chapter index; it's what most e-readers show
   anyway ("12% through Chapter 4").
2. **Does "continuous" mean anything for EPUB**, or is chapter-by-chapter (as today)
   the right mental model, with just a nicer TOC sidebar? Recommendation: skip it
   (phase 6, low priority) unless it turns out to bother you in daily use — chapter
   navigation is already a completely normal e-reader pattern, unlike PDF where
   page-by-page really is the awkward default.
3. **Search scope**: current chapter only (cheap, matches "one chapter loaded at a
   time" reality) vs. whole book (needs pre-extracting every chapter's text up front,
   more like the PDF searcher). Recommendation: start chapter-scoped, revisit if it
   feels wrong in use.
4. **Note-kind rendering**: PDF's "note" annotation kind is a small margin/inline
   marker tied to a typed comment. What should that look like injected into arbitrary
   EPUB HTML without breaking the source layout? Worth a quick visual sketch before
   building it, since unlike highlight/underline/strikeout (all just `<mark>`/CSS on
   existing text) a note marker adds new visible content to the page.
