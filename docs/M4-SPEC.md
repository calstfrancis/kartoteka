# Kartoteka M4 — Map to a Full-Fledged Citation Manager

Status: **Tier 1 done.** All of 4A (live PDF highlighting: rectangle highlights, real
text-run selection, and annotation list/edit/delete), all of Tier 2, and 4B (Zerkalo
integration) are **built and tested**. Tiers 3–4 are scoped, not started.

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

### 4B. Zerkalo doesn't consume the vault yet — done

**Done, Zerkalo-side, shipped 2026-08-14 as Zerkalo v0.23.0 "Open Shelf" (before this
doc's or `STATUS.md`'s last update — this file just hadn't been reconciled with it until
2026-09-04).** Zerkalo's Settings → Bibliography and the citation sidebar both gained a
"choose a Kartoteka vault folder" option alongside the existing `.bib`/`.yaml` picker
(`src/ui/app_window/citations.rs`, `src/vault_watch.rs`, `src/bibliography.rs`). Entries
load through `fond-bib::Library::open`/`keys_sorted`/`load_entry` — the same crate
Kartoteka itself is built on — and refresh live via `fond_vault::watch`'s `notify` wrapper
the moment anything changes in the vault: add or edit an entry in Kartoteka and it shows
up in Zerkalo's `@`-autocomplete popup, the citation sidebar, and the reference manager
within about a second, no restart needed. A "K" button next to the vault picker launches
Kartoteka directly (or focuses it if already running). Plain `.bib`/`.yaml` bibliographies
keep working unchanged.

Zerkalo currently pins `fond-bib`/`fond-vault` at `tag = "v0.7.0"` (its `Cargo.toml`)
against Kartoteka's current `v0.9.0` — checked 2026-09-04: the only changes to those two
crates between the two tags are `acquire::fetch_isbn_cover`/`Library::store_cover`
(Bookshelf-view cover caching, v0.8.0), which Zerkalo's citation autocomplete doesn't use,
so there is no functional gap from the stale pin. Bumping it is still worth doing
opportunistically (next time Zerkalo's dependencies get a pass, per its own
`WINDOWS-HARDENING-PLAN.md` precedent of bumping this same pin) — just not urgent.

---

## Tier 2 — done

Carried forward from `docs/M2-GUI-PLAN.md` §4 and `docs/STATUS.md`'s "next candidates" —
nothing new decided here, just consolidated. All six items are now built.

- **Progress/cite/tasks in-GUI editing — done.** The note editor gained Progress (page/of),
  Cite (short form + preferred style), and a small editable Tasks list (add/check/set a due
  date/delete), all previously round-tripped on disk only. Verified end-to-end headless:
  filled every field, saved, confirmed `notes/<key>.md`'s frontmatter matched exactly,
  reopened and confirmed it reloaded into the form, deleted a task and confirmed it left the
  frontmatter entirely (not just marked done).
- **Node deletion — done.** `Library::delete_node()` mirrors `delete_entry`'s approach —
  strips every relation edge naming the node (reusing `strip_all_edges_to`, which already
  spans notes ∪ nodes) then removes `nodes/<slug>.md` — and the node editor gained a
  "Delete…" button (only when editing an existing node) behind the same confirmation-dialog
  pattern `confirm_delete_entry` already established. A `fond-bib` test covers the file
  removal, the relation cleanup, and delete-again-is-a-no-op idempotency. Verified end-to-end
  headless: deleted a node with an incoming relation from an entry, confirmed the node file,
  the confirmation dialog, and the Nodes-manager list refresh all worked.
- **Facet chip grouping — done.** The detail panel's Tags row now groups faceted tags
  (`discipline:theology`) under a small caption per facet, each value rendered as a pill
  chip (a new `.tag-chip` class — the one rule in `GLOBAL_CSS` Kartoteka owns outright, since
  no other Fond app has a tagging UI); unfaceted tags collect into their own trailing,
  caption-less group. Replaces the old plain comma-joined string, which stopped scanning as
  soon as facets and plain topical tags were mixed together. Verified visually, light.
- **"Promote AI keyword → tag" — done.** An "AI keywords…" button on the detail panel (shown
  only when `ai/<key>.yml` has keywords) opens a checklist — keywords already present as a
  tag are shown disabled ("already a tag"), the rest pre-checked — and "Add as tags" appends
  the chosen ones to the note. One-directional and user-triggered only: nothing here ever
  writes back into the AI sidecar, matching `docs/M2-SPEC.md` §4's boundary rule. Verified
  end-to-end headless: seeded an AI sidecar with one keyword already tagged and two not,
  confirmed the dialog showed that split correctly, and confirmed both new keywords landed
  in `notes/<key>.md` after saving.
- **"Used in" panel — done.** The detail panel shows a "Used in: Project (doc.typ)" row
  whenever `scan_usage()`'s reverse map has this key. Scanned live on each render rather
  than reading a cached `usage.json` — a declared project is typically a handful of Typst
  files, so this is cheap, and live means it can never go stale from a forgotten manual
  rescan. Nothing shows until a project is declared (`projects/<slug>.yml` is still
  vim/git-only; there's no project-creation GUI). Verified end-to-end headless: declared a
  project pointing at a real `.typ` file citing the entry's key, confirmed the row appeared
  with the right project name and filename.
- **Global task view — done.** A "Tasks…" menu item aggregates every note's `tasks:` into
  one list — undone first (nearest due date first, no-due-date last), then done — each row
  showing the task, which entry it belongs to, a due date if set, a checkbox that saves
  straight back to that task's own note (a lens over per-entry data, not a copy of it), and
  a "Go to entry" button that selects the entry in the main window and closes the dialog.
  Verified end-to-end headless across two entries: confirmed the aggregation, the sort
  order, and that toggling a checkbox persisted `done: true` to the correct note on disk.
- **Node-side relation editor — done.** The node editor gained a "Relations…" button next to
  "Delete…", calling the *same* `relations_dialog` the entry detail panel already uses —
  it turned out to already be host-agnostic (`Library::forward_relations`/`set_relations`
  resolve through `RelationHost`, spanning notes ∪ nodes uniformly since M3, and the dialog
  already excluded the subject from both the entries *and* nodes candidate lists), so the
  gap was purely a missing entry point, not missing plumbing. This closes the one path that
  had no GUI story at all before now: a node-to-node edge (e.g. "this person influenced that
  person") couldn't be created any other way than hand-editing YAML. The node editor's own
  read-only neighbours section won't reflect a save made through this button until the node
  is reopened — the same boundary every other dialog in the app already has with its
  siblings, not a new one. Verified end-to-end headless: created two person nodes, related
  one to the other, confirmed the forward edge landed on the subject's file *and* the
  auto-maintained inverse edge landed on the target's file.

## Tier 3 — platform completion

- **`kartoteka-ui-tauri` (Windows)** isn't scaffolded at all. `ARCHITECTURE.md` §8 treats
  Windows as sequential-after-Linux; Linux isn't "done" by Tier 1's bar yet either, so this
  stays last.

## Tier 4 — not asked for anywhere in the existing docs, flagging in case it matters

- A real browser extension (one-click capture), vs. today's paste-a-URL scrape. **Not
  started** — a different tech stack (WebExtension + a local capture endpoint) rather than
  a GTK feature; scope as its own mini-project when picked up.
- ✅ Bulk/batch operations — multi-select tag, add-to-collection, or delete. **Done as of
  0.6.0** "Deep Stacks" (the header's "Select multiple" toggle). No dedicated
  move-to-collection (only add) — collections behave as non-exclusive tags, so "move" may
  not be a meaningful operation; revisit only if that assumption turns out wrong.
- ✅ A recent-libraries quick-switcher. **Done 2026-09-04** — the hamburger menu's "Switch
  to…" row(s), most-recent-first, current library excluded, persisted in `gui.json`
  (`recent_libraries`, capped at 8). See `CHANGELOG.md`.

---

## Suggested order

1. **4A Phase 1 — done.**
2. **4A Phase 3 — done.**
3. **4A Phase 2 — done.** All of Tier 1's Kartoteka-side work (4A) is now complete.
4. **Tier 2 — done.** All six items built.
5. **4B (Zerkalo integration) — done.** Shipped as Zerkalo v0.23.0 "Open Shelf"
   (2026-08-14). **Tier 1 is now fully complete.**
6. **Tier 3 (Windows)** — next up, now that Tier 1 is actually done on Linux.
