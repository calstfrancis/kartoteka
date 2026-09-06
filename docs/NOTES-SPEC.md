# Kartoteka Notes & Annotations — Toward a Robust System

Status: **Tier 0 done** (Notes moved to its own right-hand reader sidebar, independent of
Contents — see `CHANGELOG.md`). **Tiers 1–6 are scoped, not started** — this is a plan for
Cal to prioritize from, not a commitment to build all of it.

> **Naming note.** This is a new, standalone spec, not part of the M1–M5 extension-track
> numbering in `docs/STATUS.md` (that numbering maps 1:1 to sections of the original
> ChatGPT brainstorm — `docs/DATA-MODEL-EXTENSIONS.md`). Notes/annotations robustness is a
> deliberate deep-dive Cal asked for directly, not a brainstorm item, so it gets its own
> doc rather than being forced into "M6."

## Where this came from

Cal asked to (1) split the reader's Notes sidebar onto its own right-hand panel — done, see
below — and (2) look deeply at how note-taking and annotation should work, and build a plan
for a more robust system. This is that plan: an audit of what Kartoteka's `fond-bib::note`/
`fond-bib::annotation` modules and the reader/detail-pane GUI currently do, tiered by value
and by how much each item fits Kartoteka's plain-file philosophy (`ARCHITECTURE.md`:
authoritative files a human can read and edit without the app).

### What Kartoteka has today

- **One note per entry** (`notes/<key>.md`): YAML frontmatter (tags, read-status, rating,
  dates, attachments, relations, progress, page-label override, cite prefs, tasks, custom
  fields) + a single Markdown body. No child notes, no standalone (item-less) notes.
- **Annotations** (`annots/<key>.json`): a flat list per entry, each anchored by PDF
  page+quadpoints (with snippet/prefix/suffix re-anchoring) or EPUB chapter+snippet. Four
  kinds: `Highlight`, `Underline`, `Strikeout`, and a freestanding `Note`. Each carries a
  hex `color` and a single-line `note` comment. No tags on individual annotations, no image/
  area capture, no ink drawing, no typed "Text" annotation (typed directly onto the page).
- **Export, not a living note**: `AnnotationSidecar::to_markdown` renders all of an entry's
  annotations to one Markdown file on demand (the "Export…" button) — a one-shot dump rather
  than a real, editable note item with clickable links back to each source annotation.
- **Search** already covers note prose, annotation snippets, and tags (`fond-index`) — no
  gap here.
- **Typed relations + knowledge-graph nodes** (M2/M3) already go well beyond a plain
  "Related items" list — no gap here either; this plan doesn't touch that system.

### The real gaps

- **Only one note per entry, and no note that stands on its own** (unattached to any
  bibliography entry — a general research note, a reading list, a stray idea). A citation
  manager's notes are naturally richer than this: several notes per source, plus notes that
  aren't about any one source at all, each individually taggable, relatable, and searchable.
- **No rich embedding story for notes** — images can't be dropped into a note the way they
  can into most note-taking tools. (See Tier 4's decision on how far to take this while
  staying plain-file.)
- **Only four annotation kinds, no per-annotation tags.** A fuller annotation model
  distinguishes highlight, underline, a freestanding sticky note, typed text placed directly
  on the page, a captured image/area, and free-hand drawing — plus a comment and tags on any
  of them. Kartoteka has four of these and no per-annotation tags.
- **"Export annotations" is one-shot, not a living, linked note** generated from a
  customizable template (e.g. `{{highlight}}`/`{{citation}}`/`{{comment}}`/`{{color}}`/
  `{{tags}}` placeholders with conditional formatting) with click-back-to-source links.

---

## Tier 0 — done

- ✅ **Notes sidebar moved to the reader's right side, independent of Contents.** Both
  `show_pdf_reader` and the EPUB reader now build two separate `gtk4::Paned`s instead of one
  Stack behind a mutually-exclusive toggle pair — Contents (when present) on the left, Notes
  on the right, either or both open at once. See `CHANGELOG.md`'s `[dev]` entry and the
  `notes_paned`/`paned` wiring in `kartoteka-ui-gtk/src/ui/app_window.rs`.

---

## Tier 1 — multi-note foundation (the prerequisite for most of what follows)

Everything below this point assumes an entry can have more than one note, and that a note
can exist with no parent entry at all. Neither is true today, and it's the single biggest
structural gap in what's here now. Proposed shape, staying inside the plain-file philosophy:

- Keep `notes/<key>.md` exactly as-is — it's the entry's "primary" note and already carries
  every workflow field the GUI/CLI depend on (tags, read-status, relations, progress, …).
  No migration needed for existing libraries.
- Add **child notes**: `notes/<key>/<note-id>.md`, one file per extra note, lighter
  frontmatter (title, tags, created/modified — no read-status/progress/etc., which only make
  sense once per entry). A key with child notes gets a `notes/<key>/` directory alongside its
  unchanged `notes/<key>.md`.
- Add **standalone notes**: a new top-level `standalone-notes/<note-id>.md`, same lightweight
  frontmatter as a child note but no parent key. **Decided: these never appear in the
  entries spreadsheet or Bookshelf grid** — no fake row with a title and blank
  Type/Author/Year/Files. Those views stay strictly citation-shaped. Standalone notes are
  reachable only through the dedicated Notes browsing view (Tier 5) and through search.
- `fsck`: orphaned child-note directories (parent key deleted), malformed note ids.
- `fond-index`: index each note file as its own indexed unit (not concatenated into the
  entry's single blob as now), so search results can point at *which* note matched.
- GUI: the entry detail pane's "Edit note…" becomes a small note list (primary note always
  first, then children) with a "+ New note" action; standalone notes are created from the
  Notes view (Tier 5), not from the main entries list.

---

## Tier 2 — annotation model upgrades

- Add `tags: Vec<String>` to `Annotation` (`fond-bib::annotation`) — lets the notes sidebar
  filter by tag or by annotation type.
- Add `AnnotationKind::Text` (a short piece of typed text placed directly on the page) and
  `AnnotationKind::Image` (a captured page-region bitmap).
  - Image capture reuses `fond-doc`'s existing PDFium bitmap rendering (already used for
    on-screen page display) to render just the selected rectangle, then stores it
    content-addressed in `attachments/` like any other blob (`Annotation` gains an
    `image_hash: Option<String>`), so the plain-file guarantee holds — the PNG sits in the
    repo like everything else, not inside the JSON sidecar as base64.
  - EPUB has no fixed page raster to capture a region from, so `Image` is PDF-only; `Text`
    works for both (EPUB "typed text" would be a DOM-inserted span, analogous to a highlight).
- Widen the notes-sidebar comment field from the current single-line `gtk4::Entry` to a
  small multi-line `gtk4::TextView`, so an annotation's own comment can be actual prose, not
  one line.
- **Explicitly deferred: ink/drawing annotations.** Free-hand Cairo drawing over a rendered
  PDF page is a much bigger UI lift (a drawing-mode gesture, undo/redo per stroke, an eraser,
  stroke-width picker) for a citation manager where marginal drawing is a rare workflow, not
  a common one the way highlighting is. Worth revisiting only if Cal specifically wants it.

---

## Tier 3 — a real "Create note from annotations"

Depends on Tier 1 (needs somewhere to put the result other than a one-shot export) and
benefits from Tier 2 (tags/color to drive template branching).

- A small template engine over the existing `to_markdown` logic: `{{highlight}}`,
  `{{citation}}`, `{{comment}}`, `{{color}}`, `{{tags}}` placeholders plus `{{#if …}}`/
  `{{else}}` blocks. One built-in default template (`{{highlight}} {{citation}} {{comment}}`),
  user-editable.
- "Create note from annotations…" in the notes sidebar: select a subset (or "all"), generates
  a new **child note** (Tier 1) from the template, not just a file — each generated block
  carries a click-to-jump-back-to-that-annotation affordance (a `GestureClick` on the
  rendered block, the same jump-to-page/chapter helper `rebuild_notes`'s own row headers
  already use — no new navigation plumbing needed, just reusing it from inside a note body).
- Filter-before-generate by color and/or tag (Tier 2) to drive per-color template branching.
- The existing one-shot "Export…" (`to_markdown` → a plain file) stays as-is for "I want this
  outside Kartoteka entirely" — this tier adds the living-note option alongside it, not in
  place of it.

---

## Tier 4 — inline linking within note prose

- `@key` autocomplete while typing in a note body, resolving against the same
  `fond-bib`/`fond-vault` lookup Zerkalo's own `@`-autocomplete already consumes against a
  Kartoteka vault (shipped as Zerkalo v0.23.0 "Open Shelf", 2026-08-14 — see
  `docs/STATUS.md`'s 4B note). This would be the first consumer of that lookup *inside*
  Kartoteka's own GUI, not just an external app — reasonable, since the API already exists
  and is already proven against real vaults.
- Clicking a resolved `@key` reference in a note jumps to that entry's detail pane.
- **Decided: notes stay plain Markdown, not rich text.** Kartoteka's whole pitch is plain
  files a human can read/edit without the app (`ARCHITECTURE.md`); a WYSIWYG/HTML note body
  would stop being comfortably hand-editable. An embedded image is a Markdown image link to
  a file in `attachments/`, typed like any other Markdown, not pasted through a formatting
  toolbar. `@key` links are the one piece of "smart" editing on top of plain text.

---

## Tier 5 — library-wide notes browsing

- A "Notes" view (third mode alongside Spreadsheet/Bookshelf, or a standalone dialog) listing
  every note in the library — primary, child, and standalone (Tier 1) — filterable by tag,
  searchable, independent of first locating the parent entry. This is the sole home for
  standalone notes (Tier 1 decided they never appear in the entries spreadsheet/Bookshelf).

---

## Tier 6 — smaller polish items

- Per-annotation tag chips shown in the notes sidebar rows, with a filter control (depends on
  Tier 2's `tags` field).
- Filter the notes sidebar by annotation kind (Highlight/Underline/Note/Text/Image).
- Optional per-library colour-name legend (e.g. "amber = key argument") shown as a tooltip on
  the colour picker — low priority.

---

## Suggested order

Tier 1 unlocks the most (multi-note + standalone notes is the real structural gap); Tier 2 is
independent and can land in parallel. Tiers 3–4 both depend on Tier 1. Tier 5 depends on
Tier 1 existing but is otherwise standalone. Tier 6 items are each small enough to slot in
wherever convenient once Tier 2 lands. None of this is scheduled — pick per Cal's actual
priorities next.
