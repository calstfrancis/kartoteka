# Kartoteka M4 — Map to a Full-Fledged Citation Manager

Status: **in progress.** 4A Phase 1 (live PDF highlighting) is **built and tested** this
session. Everything else below is scoped, not started.

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
- Full workspace gate: **135 tests passing** (up from 127 at session start), 0 clippy
  warnings.

**Phase 2 — real text-run selection (not started).** Phase 1 highlights an axis-aligned
*rectangle*, not the actual selected text run — good enough for "mark this region" but not
"highlight this sentence" the way Zotero/a PDF reader does. Needs PDFium's per-character
bounding boxes (`PdfPageText::chars()`, not yet used anywhere in `fond-doc`) to hit-test a
drag against the text layer and produce one quad per line the selection spans, plus capture
the actual selected string into `Annotation.snippet` (currently `None` for drawn highlights —
Phase 1 has no re-anchoring key if the PDF is later replaced by a differently-produced copy,
unlike imported annotations). Phase 1's `blend_highlights`/`page_size`/drag-to-quad plumbing
all carry over unchanged; only the *shape* of what gets captured changes.

**Phase 3 — annotation management UI (not started).** There is still no way to see, edit, or
delete individual annotations from the GUI — the detail panel shows only a count
("Annotations: 3"). Needed: a list view (reuse the pattern from the Nodes manager or duplicate
groups dialog), click to jump to that page/highlight in the reader, edit the `note` field,
delete. Underlines/strikeouts (already representable in `AnnotationKind`, already
import/export-capable) have no drawing gesture yet either — Phase 1 only draws highlights.

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
2. **4A Phase 3 (annotation list/edit/delete)** next — Phase 1 without any way to review or
   remove what you drew is an unfinished loop; this is small and self-contained.
3. **4A Phase 2 (real text-run selection)** — bigger (PDFium char-geometry hit-testing), worth
   doing once Phase 3 proves the sidecar-round-trip UX is right.
4. **4B (Zerkalo integration)** — highest leverage on the *combined* workflow, but lives in
   another repo; scope it there when picked up.
5. **Tier 2 items** — pick per Cal, same as `M2-GUI-PLAN.md` always said; none block anything
   else.
6. **Tier 3 (Windows)** — after Tier 1 is actually done on Linux.
