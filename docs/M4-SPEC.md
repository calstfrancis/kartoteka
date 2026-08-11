# Kartoteka M4 — Map to a Full-Fledged Citation Manager

Status: **in progress.** All of 4A (live PDF highlighting: rectangle highlights, real
text-run selection, and annotation list/edit/delete) is **built and tested**. Everything
else below is scoped, not started.

> **Naming note.** "M4" here is the next step in the **knowledge-base extension track**
> (M1 = the original vault, M2 = typed relations/facets/AI sidecar/projects, M3 = knowledge-
> graph nodes — all built, see `docs/STATUS.md`), *not* `ROADMAP.md`'s "Milestone 4"
> (documents/PDF handling, long done — text extraction, annotation import/export). This
> milestone is about closing the gap between "the original brief, fully implemented" (which
> Kartoteka already is) and "actually full-fledged" — the handful of things a working citation
> manager still needs that neither track above covers.

## Where this milestone came from

An audit against Zotero-level expectations, prompted by "analyze kartoteka, build a map
toward a full-fledged citation manager." Two findings drove the shape of this doc:

1. **The project is much further along than `~/Projects/CLAUDE.md` (the suite-wide root
   doc) described it** — that doc still said "CLI-stage, no GUI yet." In reality:
   `kartoteka-ui-gtk` is a mature ~5,800-line GTK4/libadwaita app; M1–M3 are complete (vault,
   Zotero migration, CSL bibliography output, PDF text extraction + annotation import/export,
   metadata acquisition, EPUB import, full-text search, duplicate detection, typed relations,
   the knowledge graph); and there's real sync/capture infrastructure not documented in any
   roadmap file — GitHub OAuth push (in-process libgit2), a WebDAV attachment-blob mirror,
   and an "Add from URL" citation-metadata scraper (Highwire/Dublin-Core/Open-Graph).
2. **`docs/ROADMAP.md` §4.5 is itself stale** — it says "bitmap render/outline not yet
   needed," but `kartoteka-ui-gtk` already has a working in-app PDF viewer (page nav + zoom,
   PDFium bitmap rendering) in `show_pdf_reader`. Worth a cleanup pass on `ROADMAP.md`
   independent of this milestone.

Given that, the real gaps toward "full-fledged" are narrower than they first looked. They're
tiered below by how much they block the actual end-to-end workflow (find a source → read and
annotate it → cite it while writing).

---

## Tier 1 — blocks the core workflow

### 4A. Live PDF annotation

**The gap:** the built-in PDF viewer was read-only (page nav + zoom). Annotations only
entered the sidecar via import of *externally*-created highlights (`import-annots`, needs
another PDF reader in the loop). Zotero's single most-used feature — read a PDF, highlight
as you go — didn't exist in Kartoteka itself.

**Phase 1 — rectangle highlighting (done this session).** Click-drag on the rendered page
creates a highlight, written straight to the existing `annots/<key>.json` sidecar format
(`fond-bib::Annotation`/`AnnotationSidecar` — this is that format's first *writer*; until now
only PDF import/export touched it) and immediately visible, blended into the page render.

- `fond-doc::pdf::page_size()` — a page's PDF-point dimensions, the scale factor between
  screen pixels and the quadpoint coordinate space annotations are anchored in.
- `fond-doc::pdf::blend_highlights()` — pure pixel math (no PDFium involved beyond the
  already-rendered bitmap), alpha-blends quadpoint rectangles into a `RenderedPage`'s RGBA
  buffer. Unit-tested without a real PDF.
- `fond-bib::Annotation::drawn()` — a second constructor alongside the existing `imported()`,
  time-seeded rather than content-seeded (a hand-drawn highlight has no snippet to derive
  stability from, and — being newly created, not re-imported — has no idempotency
  requirement).
- `kartoteka-ui-gtk`: `show_pdf_reader` gained a `GestureDrag` on the page `Picture`, a
  `ReaderState.annotations` sidecar loaded once at open and rewritten on every highlight, and
  re-renders each page with that page's saved highlights blended in.
- **Verified end-to-end headless**, with a real PDFium binary (`bblanchon/pdfium-binaries`,
  the same release the flatpak manifest pins) rather than the skip-when-absent path the crate
  tests fall back to: opened a throwaway library's PDF, dragged a highlight, confirmed the
  correct quadpoints landed in `annots/<key>.json` on disk and the tint rendered in the
  correct place on re-render.
**Phase 2 — real text-run selection (done).** A drag now hugs the actual text it covers
instead of highlighting an axis-aligned rectangle: `fond_doc::select_text_in_rect()` walks
every character on the page (`PdfPageText::chars()`), filters to the ones whose own bounds
overlap the drag, and groups consecutive hits into one quad per line (a new line starts
whenever a hit's vertical range stops overlapping the current line's). This is deliberately
*not* `chars_inside_rect()` — that method resolves only the two characters nearest the
rectangle's left/right edges and returns everything between them by document index, correct
for a single line but wrong the moment a drag spans more than one. The selected text is
captured into `Annotation.snippet` (Phase 1 always left this `None`, so a Phase-1 highlight
had no re-anchoring key if the PDF were ever replaced by a differently-produced copy — Phase
2 highlights do). A drag over an area with no text (a figure, a blank margin) falls back to
Phase 1's plain rectangle, so nothing regressed. Phase 1's `blend_highlights`/`page_size`
plumbing carries over unchanged — only the quad-building step changed.

Verified two ways: three new `fond-doc` integration tests against a real two-line synthetic
PDF (a two-line drag → two quads and both lines' text; a same-line drag → one quad and just
that line's text; a drag over blank space → `None`, confirming the Phase-1 fallback path is
reachable) — all real PDFium calls, not string-matching. Then end-to-end headless in the
live app: dragged over two lines of real rendered text, confirmed `annots/<key>.json` got
two tightly-fitted quads (not the drag rectangle's bounding box) and the correct
`"snippet"` text, and confirmed the highlight visually hugs each line on re-render.

Full workspace gate after both phases: **135 tests passing** (up from 127 at session
start), 0 clippy warnings.

**Phase 3 — annotation management UI (done).** An "Annotations…" button on the detail panel
(shown whenever the sidecar has entries) opens a list: each row shows page + kind, a "Go to
page" button that opens the reader jumped to that page (`show_pdf_reader` gained a
`start_page` parameter), an editable note field (saves on Enter or on losing focus, matching
the app's other "save as you go" dialogs), and a delete button that removes the annotation
from both the sidecar on disk and the row on screen immediately. Verified end-to-end
headless: edited a note and confirmed the new text landed in `annots/<key>.json`; used "Go
to page" and confirmed the reader opened on the right page with the existing highlight
already rendered; deleted the annotation and confirmed both the on-disk sidecar and the
list row were gone.

Underlines/strikeouts (already representable in `AnnotationKind`, already import/export-
capable) still have no *drawing* gesture — Phase 1's drag gesture only creates highlights.
That's a small follow-up (a mode toggle or modifier-key variant on the same drag), not
tracked as its own phase since it reuses everything Phase 1 already built.

### 4B. Zerkalo doesn't consume the vault yet

**The gap:** `ARCHITECTURE.md` §5 calls Zerkalo/Skrizhal adoption of `fond-bib`/`fond-vault`
"deferred." Today, Zerkalo can only point at `library.yml` as a flat bibliography file — there
is no live citation-key autocomplete while writing, because Zerkalo never watches the vault
directory or reads structured `Entry` objects. For a citation manager whose reason for being
is "for writing in Typst," this is the actual missing link end-to-end: Kartoteka manages
sources, but the writing tool doesn't yet feel that.

**This is Zerkalo-side work, not Kartoteka-side** — the crates (`fond-bib`, `fond-vault`) are
already designed UI-agnostic and consumer-neutral for exactly this (`ARCHITECTURE.md` §4), so
adoption is additive on Zerkalo's end: depend on the two crates as a git dependency on this
repo, watch the vault with `fond-vault`'s `notify` wrapper, feed a citation-key autocomplete
from `fond-bib::Library::keys_sorted()`/`load_entry`. Scoping the actual autocomplete UI is a
Zerkalo-repo task; flagged here because it's the highest-leverage single change to the
*combined* Kartoteka+Zerkalo workflow, and because Kartoteka's crates already made the
adoption path a non-redesign on purpose.

---

## Tier 2 — already spec'd, not built

Carried forward from `docs/M2-GUI-PLAN.md` §4 and `docs/STATUS.md`'s "next candidates" —
nothing new decided here, just consolidated:

- **"Used in" panel** — `scan_usage`/`write_usage` exist at the `fond-bib` layer; needs a GUI
  scan trigger (on open/reindex) and a `usage.json` loader on the detail panel.
- **Facet chip grouping** in the entry editor (the search side, `facet:` scoping, is done).
- **"Promote AI keyword → tag"** — one-directional, user-triggered only (never automatic, per
  the `ai/<key>.yml` boundary rule).
- **Global task view** — aggregate note `tasks:` across the library.
- **Progress/cite/tasks in-GUI editing** — currently round-tripped on disk only.
- **Node deletion** — the Nodes manager (M3) has no delete yet; vim/git for now.
- **Node-side relation editor** — relations are currently authored from the entry side only.

## Tier 3 — platform completion

- **`kartoteka-ui-tauri` (Windows)** isn't scaffolded at all. `ARCHITECTURE.md` §8 treats
  Windows as sequential-after-Linux; Linux isn't "done" by Tier 1's bar yet either, so this
  stays last.

## Tier 4 — not asked for anywhere in the existing docs, flagging in case it matters

- A real browser extension (one-click capture), vs. today's paste-a-URL scrape.
- Bulk/batch operations — multi-select tag, delete, or move-to-collection.
- A recent-libraries quick-switcher (today: one `library_path` in config, reopened via a
  folder picker each time you switch).

---

## Suggested order

1. **4A Phase 1 — done.**
2. **4A Phase 3 — done.**
3. **4A Phase 2 — done.** All of Tier 1's Kartoteka-side work (4A) is now complete.
4. **4B (Zerkalo integration)** next — highest leverage on the *combined* workflow, but lives
   in another repo; scope it there when picked up.
5. **Tier 2 items** — pick per Cal, same as `M2-GUI-PLAN.md` always said; none block anything
   else.
6. **Tier 3 (Windows)** — after Tier 1 is actually done on Linux.
