# Changelog

All notable changes to Kartoteka are recorded here. Kartoteka is part of the Fond suite.

## [dev]

**Relations map (prototype), fleshed out.** An entry's "More" menu has "Relations
map…" — an entry-centered, force-directed visualization of its typed relations (cites,
influenced by, authored by, …), including the automatically-maintained inverse edges
(so it doubles as a backlinks view: who cites *this*, not just what this cites).
- Click a node to pull its own connections in too, expanding outward from the entry
  you started on.
- **Double-click a node to open it** — closes the map and selects that entry (or opens
  the node editor, for a person/school/concept/event/place).
- **Right-click a node to remove it** from the map (client-side only; doesn't touch
  your data) — for decluttering after expanding a few hops.
- **Reset button** in the header returns to just the starting entry's direct connections.
- **Edges are now directional** (arrowheads, matching the predicate label's own
  phrasing) and there's a **legend** for the node-kind colours.
- A soft cap (80 nodes) with an on-canvas notice, so an entry with a lot of connections
  can't turn the map into an unreadable, unresponsive tangle.

Still read-only otherwise — no editing relations or dragging a line to create one from
the map itself. Rendered in a `WebView` (Canvas 2D, a small hand-written force
simulation, no external resources) rather than hand-built in Cairo — the same
`WebView`-embedding pattern the EPUB reader uses, extended with a JS↔Rust message
channel (`UserContentManager`) for node clicks. Marked "(prototype)" in the UI; not
yet a finished feature.

**Re-arrangeable columns, remembered window sizing, more keyboard access.**
- The entries spreadsheet's columns can now be dragged by their headers to reorder them
  (native GTK behaviour — order resets to Key/Title/Author/Year/Files each launch, same
  as column widths already did).
- Window size/maximized state and both pane positions (collections sidebar, detail pane)
  are now remembered across sessions instead of resetting to the same defaults every launch.
- New keyboard shortcuts: Ctrl+N (New item), Ctrl+O (Open library), Ctrl+Shift+N (New
  library), Ctrl+F (focus search), Ctrl+? / F1 (Keyboard shortcuts — a new help dialog,
  also in the menu). Escape in the search field now clears it and returns focus to the list.
- The EPUB reader gets font-size controls (A-/A+ in the header) — text-only zoom, so
  images and layout width stay put while the text scales.


**EPUB reader: notes sidebar, more mark kinds.** The EPUB reader gets a persistent
Notes/highlights sidebar — every annotation in reading order, click to jump to it,
edit its comment inline, delete it — matching the PDF reader's own sidebar (Contents
moved into the same sidebar-toggle pattern alongside it). Marking text is no longer
highlight-only: a mode picker adds Underline and Strikeout, plus a colour picker for
highlights. See `EPUB-READER-PLAN.md` for the full parity plan and what's still ahead
(zoom, in-book search, undo/redo, reading position tracking).

**Spreadsheet: read-only cells, zebra striping, more room by default.** Cells in the
entries spreadsheet are no longer editable in place — editing lives entirely in the
detail pane on the right now, matching how attachments/tags/notes already work. Rows
alternate with a subtle background tint to make wide rows easier to track across
columns, and the window opens with substantially more space given to the spreadsheet
(the detail pane starts narrower — drag the divider if you want it back).

**Custom fields.** Menu → "Custom fields…" lets you define your own fields — a name and
a type (Text, Number, or Tag/comma-separated, styled like the built-in Tags field) —
that then appear on every entry's detail pane. Values are per-entry as usual; the
definitions are library-wide and travel with the library (`custom-fields.yml` at the
library root, git-tracked like everything else).

**Move library…** (Menu) relocates the current library's folder — its git history and
search index included — to a new location, e.g. a different drive; updates the saved
path so it reopens there next time. A plain rename where possible, falling back to a
full copy for a cross-filesystem move.

**Create book part…** (an entry's "More" menu, on a Book/Anthology entry) starts a new
chapter/section entry — for one contributor's chapter in an edited anthology — that
credits the source book as its `parent:` (title, editor/author, publisher, date, ISBN…)
without duplicating those fields onto the new entry. Choose whether the book's listed
author(s) become the part's editor (the common case) or stay as author. Unlike a
copy-and-retype workflow, the part remembers where it came from: **"Refresh from source
book…"** on the resulting entry re-pulls the book's current fields on demand, so editing
the book later doesn't leave every chapter's citation quietly out of date.

**Friendlier for first-time and non-technical users.** A round of accessibility/onboarding
work:
- **First-run welcome page** instead of a blank window: "New library…" and "Open existing
  library…" front and centre, with a plain-language explanation of what a library is.
- **New library…** creates the folder and its layout for you (name + location), so you no
  longer need to go create an empty folder yourself before Kartoteka will let you in.
- **Empty-library and no-results states**: an empty library now shows quick "Acquire…" /
  "New item…" buttons instead of a blank pane; a search or collection with no matches says
  so instead of just going blank.
- **Save a copy…** (Menu) — a one-click, no-git-required backup: copies the whole library to
  a folder you choose. "Back up (git commit)…" is still there for versioned history and
  GitHub push, for anyone who wants it.
- **Plain-language error messages** in place of raw internal errors for the most common
  actions — opening a library, adding a reference (Acquire/New item/PDF/EPUB/URL), editing
  fields, saving notes/tags/collections/nodes — including a specific, actionable message
  when a first backup fails because git has no name/email configured yet.
- **Jargon explained in place**: hover text on the DOI/arXiv/ISBN picker, the citation-key
  column, and the search bar's `author:`/`title:`/`tag:`/`type:`/`year:` syntax; the Nodes
  feature now says up front that it's for tracking people, places, and other things
  connected to your references.

**Entries as a spreadsheet.** The entry list is now a sortable, Zotero-style spreadsheet
(`Key` / `Title` / `Author` / `Year` / a compact PDF/EPUB availability column) instead of a
card list — click a column header to sort by it, and double-click (or Enter on a focused
cell) any Title/Author/Year cell to edit it in place, no dialog needed. The citation key is
its own read-only column (it's also the on-disk filename); dragging it onto a collection in
the sidebar still adds the entry to that collection, same as before. The sidebar and detail
pane are unchanged.

**Automatic backups.** Menu → "Automatic backups…" turns on a periodic backup while a
library is open: commits any changes on a chosen interval (15 min / 30 min / hour / 4
hours), pushing to GitHub if already signed in with a repo configured, and mirroring to
WebDAV if that's set up too. Off by default; reuses whatever GitHub/WebDAV credentials are
already configured via the existing manual backup flows, so there's nothing new to set up.
Ticks are skipped when nothing has changed, and auto-backup never creates a new GitHub repo
on its own — that first push still happens through the explicit "Back up (git commit)…"
action.

## [0.5.1] "Sound Index" — 2026-08-19 — Dependency security updates and a search-index race fix

### Fixed

- **Fixed a rare, intermittent failure rebuilding the search index** — a background
  file-watcher thread tantivy sets up for search queries could still be touching the index
  directory at the moment something tried to delete and rebuild it, occasionally failing the
  rebuild outright. More likely to surface under load (e.g. background reindexing while other
  work is happening) than in everyday single-library use, but a real bug either way.

### Internal

- Updated `rusqlite` (0.32→0.40), `hayagriva` (0.9→0.10), and `quick-xml` (0.37→0.41) —
  closes two real CVSS-7.5 denial-of-service advisories in `quick-xml`
  (RUSTSEC-2026-0194, RUSTSEC-2026-0195) that a `cargo audit` pass turned up, reachable via
  EPUB text extraction. Two more `quick-xml` advisories remain, pulled in transitively via
  `citationberg`'s own pin — not fixable from here until `citationberg` bumps its own
  `quick-xml` dependency.

## [0.5.0] "Cross Reference" — 2026-08-14 — Nested collections and an editable-in-place detail pane

"Select text" mode (and highlighting/underlining/strikeout too) now works the way a real
text editor's click-drag selection does: dragging in a straight line down through a
paragraph selects each whole in-between line, not just the narrow column directly under the
pointer — only the first and last lines are trimmed to the exact start/end position. The
cursor switches to an I-beam in Select mode, and the live drag preview now shows the actual
line-aware selection (in a distinct blue tint for Select mode) instead of a plain rectangle.

Adding a note right after copying a text selection on the same page now pre-fills the note
with the selected text, quoted and tagged with its page number, instead of starting blank.

**Collections can now nest.** A collection can have a parent (set at creation, or by hand in
its `collections/<slug>.yml`), and the sidebar shows the resulting tree, indented — the
"Collections…" membership dialog reflects the same nesting. Drag an entry from the list
straight onto a collection in the sidebar to add it there, instead of only through the
membership dialog.

Each entry in the list now shows a small icon when a PDF and/or EPUB is available to open,
at a glance, without opening the entry first.

**The detail pane is directly editable — no dialog required.** Type, Title, Author(s), Year,
Publisher, DOI, ISBN, Tags, Status, and Rating are now live fields: click into one, edit it,
and it saves on Enter or when you click away. The "Edit citation info…" dialog is gone (its
job is done inline now); "Edit note…" remains for the fields that aren't inline yet —
reading progress, citation preferences, tasks, and the free-text note body.

## [0.4.0] "Quiet Margin" — 2026-08-14 — PDF reader usability pass

The Contents/table-of-contents sidebar is now user-resizable via its Paned handle instead of
being pinned to the width of the longest chapter title, and can be dragged down to a slim
strip. Continuous scrolling is now the reader's default mode (was single-page); switching
between continuous and single-page already preserved your position and still does.

The old per-page "This page" dropdown for editing/deleting annotations is gone — **right-click
a highlight, underline, strikeout, or note** to edit its note text or delete it inline, or
right-click blank page space to add a new note there. A new **Notes sidebar toggle** (next to
Contents) lists every note and highlight in the whole document, with its highlighted snippet
and note text readable at a glance rather than just an on-page icon — click an entry to jump
to its page, or delete it from there too. Highlight and search-match colours are also more
visible (raised from ~35%/50% to ~55%/65% opacity).

Page navigation and zoom controls moved from the headerbar down to a new bottom status bar,
freeing the headerbar's title slot to show the document's own name again.

The mode picker has a new **"Select text"** option (now the drag mode next to Highlight/
Underline/Strikeout) — drag over text to copy it to the clipboard instead of creating an
annotation.

**Fixed:** opening a PDF (or switching into continuous-scroll mode) could hang the whole
window for the document's full render time, sometimes appearing to freeze permanently —
continuous mode's page layout used to rasterize every page synchronously before showing
anything. Page sizing is now computed from cheap PDF metadata up front (instant), and each
page's actual image renders incrementally in idle time afterward, starting from whichever
page you opened on.

Dragging a PDF onto the window to add it now tells you when a drop couldn't be read (an
unrecognized drag source, or a remote file with no local path) instead of silently doing
nothing.

**Fixed:** launching Kartoteka while it was already running opened a second window instead
of raising the existing one — relevant now that other Fond-suite apps (starting with Zerkalo)
can launch Kartoteka via `flatpak run`, which activates the existing instance over D-Bus
rather than starting a new process.

## [0.3.0] "Loose Leaf" — 2026-08-13 — Markdown annotation export, multi-attachment reading

Annotations are now readable without opening Kartoteka at all, à la Zotero's "add note from
annotations": an **"Export…" button** in the "Annotations…" dialog renders an entry's whole
annotation sidecar as a portable Markdown file — one heading per PDF page or EPUB chapter,
the highlighted text quoted, then your note below it — and saves it wherever you choose.

An entry with **both a PDF and an EPUB attached** (the same work in two formats) now gets a
**"Read" chooser** letting you pick which to open, instead of silently opening whichever the
attachments list happened to list first. The "Annotations…" dialog's "Go to" buttons are also
now correctly per-annotation format-aware on such an entry — a PDF-anchored row opens the PDF
reader, an EPUB-anchored row opens the EPUB reader, instead of every row routing through
whichever format the dialog happened to open with.

## [0.2.0] "Marked Folio" — 2026-08-13 — Built-in EPUB reader and PDF reader polish

A built-in EPUB reader, alongside the existing PDF one: chapter navigation, a table-of-
contents jump list (from the EPUB3 nav document or the EPUB2 NCX), and proper rendering of
each chapter's text, images, and stylesheet via an embedded WebKitGTK view — not just the
metadata-only import EPUB had before.

EPUB pages can now be highlighted, same as PDF: select text and click "Highlight" to save it
to the entry's annotation sidecar (chapter + snippet, since an EPUB has no fixed page grid to
anchor a PDF-style quadpoint to), with saved highlights reapplied automatically on every
chapter load. The "Annotations…" list and "Go to" now work for both formats, jumping into
the right chapter and scrolling straight to the highlight for an EPUB entry.

EPUB chapter text is also now indexed for full-text search, alongside PDF text (previously
only an EPUB's bibliographic metadata was searchable, never its actual content).

Also fixed: an EPUB attachment no longer routes into the PDF reader and fails silently on
"Read" — attachments are now identified by type before deciding which reader to open.

The PDF reader itself also gained a round of polish:
- **Underline and strikeout**, alongside the existing highlight — a mode picker in the
  reader's header switches what a drag creates, drawn as a thin line rather than a filled
  block. A **"Note…" button** adds a freestanding marginal note not tied to any drawn region.
- **A colour picker** (five presets) for highlights/underlines/strikeouts — each annotation
  keeps its own colour, shown correctly on re-render, not just a single shared tint for every
  highlight on the page.
- **"Contents"**, an outline/bookmarks panel — jumps straight to a chapter or section, when
  the PDF has one.
- **A search bar** — find text across the whole document, jump between matches, with the
  current match highlighted in its own colour.
- **"This page"**, an inline list of the current page's annotations with one-click delete —
  no need to open the separate "Annotations…" dialog just to remove a stray highlight.
- **Reading position is remembered**: "Read" now resumes on the page you left off on, saved
  automatically when the reader closes.
- **Continuous scroll**: a "Continuous" toggle switches from page-by-page to scrolling
  smoothly through every page in one view — highlighting, search, and navigation all work
  the same way in either mode, and switching between them keeps your place.
- **Document page numbers, Zotero-style**: for a PDF that defines its own printed page
  numbering (roman-numeral front matter, an index restarting at 1, etc.), the reader now
  shows and lets you type that printed number instead of always the raw position in the
  file — type "iv" or "1" and it jumps to the actual page the document itself calls that.

A further round of PDF reader polish:
- **Contents is now a persistent sidebar**, not a popover — toggle it from the headerbar and
  it stays open while you navigate, instead of closing after every jump.
- **Live drag preview**: dragging to highlight/underline/strikeout now shows the region in
  your current colour *while* you're dragging, instead of only appearing once you let go.
- **Undo/redo** for every annotation change — a new highlight, a note, a deletion, or a note
  edit. Undo/Redo buttons in the header, or Ctrl+Z / Ctrl+Shift+Z.
- **"This page" annotations are now editable, not just deletable** — each row has an inline
  note field (save on Enter or on losing focus), matching the whole-document "Annotations…"
  dialog instead of requiring you to open that separate dialog just to edit a note.

## [0.1.1] "Tidy Shelf" — 2026-08-13 — Internal: CI format-check fix

No user-facing changes. The codebase had no `rustfmt.toml` and had never been kept `cargo
fmt`-clean, so CI's format check had been failing since the earliest run visible in history —
skipping Clippy and Test on every single push, silently. Reformatted the whole workspace with
`cargo fmt --all` (pure whitespace/line-wrapping; build, clippy, and both test invocations CI
runs were re-verified clean before and after) so CI can actually catch something again.

## [0.1.0] "Open Stacks" — 2026-08-13 — First release

Kartoteka's first stable release, after seventeen development builds covering the original
project brief (vault, Zotero migration, bibliography output, documents, acquisition,
cross-cutting search) and two extension tracks on top of it: typed relations, facets, an AI
sidecar, and projects/usage (extension-M2); and a knowledge-graph layer of person/concept/
school nodes with typed edges to entries (extension-M3). The name is a library-science term
for shelving patrons can browse directly rather than request from a closed stack — apt for a
plain-file library that stays readable and editable without Kartoteka at all.

### Highlights

- **A plain-file library.** Entries are Hayagriva YAML, notes are Markdown, everything lives
  in a git repository — nothing locked in an opaque database.
- **Getting references in.** Import from Zotero (BetterBibTeX or its SQLite store); acquire
  by DOI, arXiv, or ISBN (the "Acquire…" dialog, Zotero's "Add by identifier" equivalent); or
  drop a PDF or EPUB in and it identifies itself, enriching via DOI/ISBN lookups where it can.
- **Working with PDFs.** A built-in viewer with highlighting, annotation management, and
  live rectangle highlighting.
- **Getting references out.** Bibliography output in SBL and Chicago styles, and annotated
  Typst documents that pair each reference with its note.
- **Finding things again.** Full-text search over metadata, notes, annotations, and PDF text,
  with field scoping (`author:`, `title:`, `tag:`, `type:`, `year:`).
  Typed relations, facets, and free-text notes, plus a knowledge-graph layer — link an entry
  to a person, concept, or school of thought, not just to another entry.
- **Sync.** Git, GitHub (device-flow sign-in), or WebDAV.

Full development history, including everything above in its original increments, follows
below.

## [0.1.0-dev17] — 2026-08-13 — Fix: hamburger menu not opening

dev16's hand-built hamburger popover (~20 rows) had no height cap, unlike the `gio::Menu` it
replaced, which scrolls automatically once its content doesn't fit. Reproduced locally: on a
screen without enough room below the button for the popover's full natural height, it didn't
reposition or shrink — it failed to show at all, which is what "click it and nothing happens"
was.

### Fixed

- **The hand-built popover helper (`popover_menu`, shared by the hamburger and the detail
  panel's "Edit"/"More" popovers) now wraps its rows in a `ScrolledWindow` capped at 420px**,
  scrolling past that instead of growing unbounded. Short popovers (Edit, More) are
  unaffected — the cap only engages once content actually exceeds it.

## [0.1.0-dev16] — 2026-08-13 — Detail panel UX pass

The detail panel's action row had grown to as many as eleven buttons (Edit note, Edit
citation…, Cite, Read, Annotations…, Open externally, Collections…, Relations…, AI
keywords…, Link author…, Locate, Delete…) in a single non-wrapping row, which forced the
whole panel to scroll horizontally to reach the later ones at any normal window width. Fixing
that properly — rather than just letting the buttons wrap — was also a chance to address a
broader clarity gap: the panel dumped every field (including Typst-internal ones like the
citation key) at equal weight, and the hamburger menu's 20-odd actions were one flat
`gio::Menu` list.

### Changed — action row capped at four items, everything else in a "More" popover

- **The action row is now `[Read/Find PDF] [Edit ▾] [Cite] [More ▾]`**, never more, regardless
  of how many actions an entry has. "Edit" is a small popover offering both edit surfaces
  (citation fields vs. personal note); "More" holds Collections…, Relations…, Annotations…,
  Link author…, AI keywords…, Open externally, Open DOI, Google Scholar, and — set off by its
  own separator — Delete…, which no longer sits in the always-visible row where a stray click
  was one motion away.
- **The hamburger menu is now a hand-built grouped popover** instead of a flat `gio::Menu`
  model — the same house-style pattern (Zerkalo's hamburger) CLAUDE.md's UI standard calls for
  once a menu has "more than a handful" of actions. Theme (System/Light/Dark) is now three
  rows with the active one bold, replacing a nested submenu — the same "name-as-label" toggle
  idiom the status bar convention already uses elsewhere in the suite.
- **The detail panel's "Key" and "Used in" rows moved into a collapsed "Details" disclosure**,
  renamed "Citation key" with a tooltip explaining what it's for — both are Typst-internal
  concepts a reader of the entry wouldn't recognize, now one click away instead of dumped
  in the main field list. DOI/ISBN stay visible as-is; they're recognizable identifiers, not
  jargon.



Dropping a book PDF onto Kartoteka scraped noticeably less than Zotero — usually just a
title and author, missing date and publisher entirely — because the fallback path only ever
read the PDF's own embedded metadata (which rarely carries more than that), with no attempt
at the ISBN-based enrichment already used for EPUB. A Zotero-style "Add by identifier" search
already existed (the "Acquire…" dialog/`kartoteka acquire`, DOI/arXiv/ISBN), so this closes
the other half: better *automatic* enrichment for dropped PDFs.

### Added — ISBN sniffing for dropped PDFs

- **`fond_doc::find_isbn`** scans a PDF's text layer for a checksum-validated ISBN-10 or
  ISBN-13 — near an "ISBN" label first (the copyright-page case), falling back to any
  checksum-valid `978`/`979`-prefixed ISBN-13 found unlabeled. Mirrors `find_doi`'s existing
  scan-and-checksum approach.
- **PDF import now tries DOI, then ISBN, then embedded metadata, in order** (CLI `add-pdf`
  and the GUI's "Add PDF…"), each network step falling through to the next on failure instead
  of hard-erroring — so a DOI lookup that fails to resolve no longer kills an import that
  could still have succeeded via ISBN or embedded title/author. An ISBN hit is enriched via
  the same OpenLibrary lookup EPUB import already uses, pulling in date, publisher, location,
  and page count that embedded PDF metadata never carries. If the ISBN was found but the
  network lookup itself failed, it's still kept and written into the fallback entry's
  `serial-number` rather than silently dropped.



The last Tier 2 item from `docs/M4-SPEC.md`: the node-side relation editor. Tier 2 is now
fully built.

### Added — node-side relation editor

- **A node can now relate to another node, or to an entry, without leaving its own editor.**
  The node editor gained a "Relations…" button that opens the same dialog the entry detail
  panel already uses — it turned out the library layer (`set_relations`/
  `forward_relations`) was already host-agnostic since the M3 knowledge-graph work, so this
  was a missing entry point, not missing plumbing. Closes the one real gap left in the
  knowledge graph: a node-to-node edge (e.g. "this person influenced that person") had no
  path to creation other than hand-editing YAML. This was the last item on
  `docs/M4-SPEC.md` Tier 2 — Tier 2 is now fully built.

### Added — "Used in" panel and a global task view

- **The detail panel shows "Used in: Project (doc.typ)"** whenever a declared project's
  Typst documents cite the entry's key — the reverse map `scan_usage()` already computed,
  now actually surfaced.
- **A "Tasks…" menu item aggregates every note's tasks into one list** — undone first
  (nearest due date first), then done — with the owning entry, a due date if set, a
  checkbox that saves straight back to that task's note, and "Go to entry". See
  `docs/M4-SPEC.md` Tier 2, which is now down to one item (the node-side relation editor).

## [0.1.0-dev13] — 2026-08-11 — Development build

Two more Tier 2 items from `docs/M4-SPEC.md`: facet chip grouping and promote-AI-keyword-to-tag.

### Added — facet chip grouping and promote-AI-keyword-to-tag

- **The detail panel's Tags row now groups faceted tags** (`discipline:theology`) under a
  small caption per facet, rendered as pill chips, instead of one flat comma-joined string
  that stopped scanning as soon as facets and plain topical tags mixed together.
- **An "AI keywords…" button** (shown only when an entry has an `ai/<key>.yml` sidecar with
  keywords) offers them as tags to add — already-tagged keywords shown disabled, the rest
  pre-checked. One-directional and user-triggered only; nothing writes back into the AI
  sidecar. See `docs/M4-SPEC.md` Tier 2.

## [0.1.0-dev12] — 2026-08-11 — Development build

Two Tier 2 items from `docs/M4-SPEC.md`: progress/cite/task editing in the note editor, and
node deletion.

### Added — progress, cite, and task editing in the note editor

- **The note editor gained three sections that were previously disk-only round-trips:**
  reading progress (page X of Y), per-entry citation preferences (a short form and a
  preferred style), and a small editable task list — add a task, check it done, give it a
  due date, delete it. See `docs/M4-SPEC.md` Tier 2.

### Added — node deletion

- **The Nodes manager can delete a node.** A "Delete…" button in the node editor (only when
  editing an existing node, matching the entry detail panel's pattern) removes the node file
  and strips every relation edge naming it from every other entry or node, behind the same
  confirmation dialog the entry delete flow already uses. `Library::delete_node()` reuses
  `delete_entry`'s `strip_all_edges_to` cleanup, which already spans notes ∪ nodes.

## [0.1.0-dev11] — 2026-08-11 — Development build

Finishes live PDF annotation (real text-run selection and an annotation management UI, on
top of `dev10`'s rectangle highlighting) and adopts the Fond suite's shared theming
throughout the app.

### Added — real text-run selection when highlighting

- **Dragging over text now selects the actual text, not just a rectangle.** The highlight
  hugs each line's real glyph extent — one quad per line — instead of the drag's own
  bounding box, and the selected text is captured into the annotation (`Annotation.snippet`),
  which Phase 1's rectangle highlights couldn't do. Dragging over an area with no text (a
  figure, a blank margin) still falls back to a plain rectangle, so nothing regressed.
  `fond_doc::select_text_in_rect()` walks every character on the page and groups the ones
  under the drag into lines — deliberately not PDFium's `chars_inside_rect()`, which only
  resolves the two characters nearest the drag's left/right edges and is wrong the moment a
  selection spans more than one line. Three new `fond-doc` integration tests exercise this
  against a real two-line PDF (spanning both lines, one line only, and a blank-area miss).
  See `docs/M4-SPEC.md` §4A Phase 2.

### Added — annotation management

- **The detail panel gains an "Annotations…" button** (shown whenever an entry has any)
  listing every highlight: page and kind, a "Go to page" button that opens the built-in
  reader jumped straight to it, an editable note (saves on Enter or on losing focus), and a
  delete button. This closes the loop opened by `0.1.0-dev10`'s drag-to-highlight — until
  now there was no way to review, annotate, or remove a highlight once drawn, only a bare
  count on the detail panel. See `docs/M4-SPEC.md` §4A Phase 3.

### Added — adopted fond-style theming, throughout the app

- **The GTK app now loads the Fond suite's shared stylesheet** (`fond-style`, vendored at
  `kartoteka-ui-gtk/style/fond.css`) — until this, Kartoteka predated the suite-wide theming
  work entirely and every window used bare libadwaita defaults. Added
  `kartoteka/kartoteka-ui-gtk` to `fond-style`'s `sync.sh`/`check.sh` app list so future
  stylesheet changes reach it the same way they reach
  Rubric/Zerkalo/Skrizhal/Iskra/Gost/Kopilka/Retseptura/Chered.
- **Every header bar in the app** (24 of them, across the main window and every dialog) now
  carries the shared chrome tint instead of Adwaita's plain white.
- **The main window's three panes** use the suite's surface classes: `.fond-sidebar` on the
  collections pane, `.fond-ground` on the entries-list pane, `.fond-view` on the detail
  pane.
- **Every real row-list in the app** — the entries list, the collections list, the Cite
  picker, the Relations dialog, the Tags manager, the Nodes manager, and the Annotations
  dialog — now uses the shared `.fond-list`/`row.fond-row`/`.fond-row-title`/
  `.fond-row-meta` conventions in place of Adwaita's `navigation-sidebar` (an accent-colour
  fill) or plain unstyled labels, and the entries list, Tags manager, and Annotations
  dialog additionally group into `.fond-card` cards with rounded first/last corners.
  Checklist-style forms (collection membership, link-authors) were deliberately left as
  plain forms — they're short confirmation lists, not scannable content, so the row/card
  treatment doesn't fit them the way it fits an actual list.
- The bottom status bar now carries `.fond-chrome`/`.fond-statusbar` to match.
- Verified light and dark headless across the main window, the Tags manager, and the Nodes
  manager.

## [0.1.0-dev10] — 2026-08-11 — Development build

Live PDF highlighting in the built-in viewer, and richer metadata from the ISBN/URL/manual
lookups (fuller dates, plus fields that were being scraped but not captured).

### Added — live PDF highlighting

- **The built-in PDF viewer can now create highlights, not just display embedded ones.**
  Click-drag over a rendered page draws a highlight, saved immediately to the entry's
  `annots/<key>.json` sidecar and blended back into the page render so it's visible right
  away. This is the sidecar format's first *writer* — until now, annotations only entered it
  via importing highlights already embedded in a PDF by another reader.
  `fond-doc` gains `page_size()` and `blend_highlights()` (pure pixel math, unit-tested);
  `fond-bib::Annotation` gains a `drawn()` constructor alongside the existing `imported()`.
  See `docs/M4-SPEC.md` §4A for the phased plan (this is Phase 1 — rectangle highlights only;
  real text-run selection and an annotation list/edit/delete UI are the follow-ups).

### Added — richer metadata from lookups

- **The ISBN lookup (OpenLibrary) no longer truncates dates to a bare year.** `publish_date`
  values like "October 2007" or "Oct 01, 2007" now become `2007-10` / `2007-10-01` —
  whatever precision OpenLibrary actually gave — instead of always dropping to `2007`.
  It also now captures the place of publication (`location`), the edition statement (as
  `note`), and OCLC/LCCN identifiers alongside the ISBN, when OpenLibrary has them.
- **"Add from URL" and the manual "New item" form gain the same date fix**, plus capture
  volume, issue, page range, and language from Highwire/Dublin-Core meta tags
  (`citation_volume`, `citation_issue`, `citation_firstpage`/`citation_lastpage`,
  `citation_language`) — previously discarded entirely.
- Verified against the real parser, not just string checks: a new `fond-bib` integration
  test builds an entry from a full OpenLibrary-shaped JSON fixture through
  `Library::add_from_yaml` and confirms Hayagriva accepts every new field.

## [0.1.0-dev9] — 2026-07-27 — Development build

Three library-management features: deleting entries, editing citation fields, and importing
EPUBs — each available from both the CLI and the GTK app.

### Added — delete entries

- **Delete an entry and everything tied to it.** `Library::delete_entry` removes the entry
  file and its sidecars (note, annotations, AI), strips every relation edge naming the key
  from all other notes/nodes, drops the key from every collection, and garbage-collects
  attachment blobs no other entry still references (a shared blob is kept). Best-effort and
  idempotent.
- **CLI:** `kartoteka delete <key>` (confirmation prompt, `-y` to skip), then a best-effort
  reindex.
- **GUI:** a destructive "Delete…" button in the entry detail panel behind a confirmation
  dialog; the list and search index refresh afterward.

### Added — edit citation info

- **A structured citation editor** for the common bibliographic fields (type, title,
  author(s), year, publisher, DOI, ISBN), reached from an "Edit citation…" button in the
  detail panel. Only the edited fields are rewritten — every other field on the entry
  (edition, language, a publisher's location, a full ISO date, …) is preserved, and the
  citation key never changes. Edits are validated by a parse round-trip before they are saved.

### Added — EPUB import

- **Drop in an EPUB and get an entry.** `kartoteka add-epub <file>` and the GUI's "Add EPUB…"
  read the book's embedded (OPF) metadata — title, author, date, publisher, ISBN — and, when
  an ISBN is present, enrich the record via an OpenLibrary lookup (falling back to the EPUB's
  own metadata offline). The `.epub` is attached to the resulting entry.

## [0.1.0-dev8] — 2026-07-27 — Development build

Completes the M3 knowledge-graph track: `fsck` node checks and node search indexing at the
library layer, and the full node GUI — a Nodes manager (list + editor), nodes as relation
targets with kind-curated predicates, node neighbours views, and author→node linkage. Fixes a
launch crash on libraries carrying a search index from a pre-nodes build.

### Fixed — startup crash on an out-of-date search index

- **Opening a library whose `.kartoteka/index/` was built before the node work no longer
  aborts.** The node indexing added a `kind` field to the search schema; opening an older
  on-disk index panicked (`FieldNotFound("kind")`) at startup, taking the whole app down before
  the window appeared. `SearchIndex::open` now detects a schema built by an older version and
  reports it instead of crashing; the GUI rebuilds the index once from the authoritative files,
  and the CLI's `search` prints a clear "run `kartoteka reindex`" message. New regression tests
  in `fond-index` cover both the legacy-schema and current-schema open paths.

### Added — M3 node detail & author→node linkage (`kartoteka-ui-gtk`, PR 8 of the sequence)

- **Author → node linkage** — an entry's detail panel gains a **Link author…** action: it
  offers to create a person node for each author (or link an existing one whose family-name
  slug already exists) and relate it to the entry with `authored`. This is how §1 author
  identifiers (ORCID/VIAF/Wikidata) get captured — link the author, then add identifiers in
  the node editor. A new person node's slug is the author's family name.
- **Node neighbours view** — the node editor now shows a read-only **Relations** section: the
  node's edges grouped by predicate, targets resolved to display names (e.g. "Authored: Black
  Theology and Black Power").
- **Node labels in entry relations** — an entry's relation list now resolves a node target to
  its label instead of showing the raw slug (e.g. "Authored by: James H. Cone").

This completes the M3 knowledge-graph track. Verified end-to-end headless.

### Added — M3 nodes as relation targets (`kartoteka-ui-gtk`, PR 7 of the sequence)

- **Relate entries to nodes** — the entry Relations dialog now lists every knowledge-graph
  node (label + type · slug) alongside other entries as a checkable target. Saving writes the
  typed forward edge via `set_relations`, which maintains the inverse edge on the node file
  automatically (polymorphic targets, PR 3).
- **Node-appropriate predicates** — each target row's predicate dropdown is curated to the
  target's kind via `Predicate::forward_choices_for` (a person node offers Related/Influenced,
  a school adds Member of, a work keeps cites/critiques/…). Advisory only: a hand-authored
  edge whose predicate falls outside the curated set is appended so Save round-trips it. See
  `docs/M3-SPEC.md` §6. Verified end-to-end headless.

### Added — M3 node manager GUI (`kartoteka-ui-gtk`, PR 6 of the sequence)

- **Nodes manager** — a new "Nodes…" menu item opens a manager window listing the library's
  knowledge-graph nodes (label + type · slug), with a live filter and a **+** to create one.
  Activating a row opens a node editor for `node-type`, `label`, `aliases`, `identifiers`
  (one `scheme: value` per line), and prose. A new node gets a collision-free slug derived
  from its label; an existing node keeps its stable slug; relations are preserved untouched
  (they're edited elsewhere). Saving quietly rebuilds the search index so the node is findable.
  Deletion is intentionally left to vim/git for now (removing a node needs relation cleanup —
  a later PR). See `docs/M3-SPEC.md` §6. Verified end-to-end headless.

### Added — M3 node indexing (`fond-index`, PR 5 of the sequence)

- **Nodes are searchable** — `fond-index` now indexes `nodes/` alongside `entries/` in one
  schema, discriminated by a `kind` field (`entry` / `node`). A node reuses `title` for its
  `label`, `type` for its `node-type`, and `note` for its body, and adds searchable `alias`
  and `identifier` text (each identifier's scheme *and* value). So a bare query matches a node
  by label/alias/identifier/body, `kind:node augustine` scopes to nodes, and `type:person`
  finds person nodes. `SearchHit` gains a `kind` field; for a node hit `title` is the label
  and `author`/`year` are empty. `kartoteka search --json` now includes `kind`. See
  `docs/M3-SPEC.md` §5.

### Added — M3 fsck node checks (`fond-bib`, PR 4 of the sequence)

- **Node files are fsck-checked** — `fsck` now parses every `nodes/<slug>.md` and flags any
  that don't (`unparseable_nodes`), and flags filenames that aren't a well-formed slug —
  lowercase alphanumerics joined by single hyphens — so a hand-created node with spaces or
  uppercase, which couldn't serve as a reachable relation target, is caught
  (`malformed_node_slugs`). The inverse-edge reconciliation across notes ∪ nodes already
  arrived with PR 3. See `docs/M3-SPEC.md` §4.
- **Reconciliation is corruption-tolerant** — `reconcile_relations` (and thus `fsck`) now
  skips a relation host whose backing file doesn't parse instead of aborting the whole pass;
  the parse failure is reported by the dedicated fsck check rather than surfacing as an error.

## [0.1.0-dev6] — 2026-07-25 — Development build

The M2 GUI/index surfacing plus the first three M3 knowledge-graph slices at the `fond-bib`
library layer: node files (`nodes/<slug>.md`), the node-oriented predicate vocabulary, and
polymorphic relation targets that span notes ∪ nodes. No GUI for nodes yet (that's later in
the M3 sequence).

### Added — M3 polymorphic relation targets (`fond-bib`, PR 3 of the sequence)

- **`resolve_target`** — `Library::resolve_target(target)` classifies a relation target as an
  existing entry key (`Target::Entry`), an existing node slug (`Target::Node`), or neither
  (`Target::Dangling`); entries win if a slug ever collides with a key. This is the mechanism
  that makes a relation `target` polymorphic across the note and node file types.
- **Relations span notes ∪ nodes** — `set_relations`/`add_relation`/`remove_relation` and
  `reconcile_relations` now operate over a *relation host* (the note behind an entry key, or a
  node file behind a slug) instead of assuming target == entry key. A forward edge may live on
  a node and point at an entry (or vice-versa), and its maintained inverse lands on whichever
  file type the target resolves to. `reconcile_relations` gathers forward edges from every
  note **and** every node, validates targets against entry keys ∪ node slugs, and repairs
  inverse edges across both — so `fsck` (which calls it) now covers node relations too. See
  `docs/M3-SPEC.md` §3. Purely a widening: with no nodes present, behaviour is identical to M2.

### Added — M3 node-oriented predicates (`fond-bib`, PR 2 of the sequence)

- **Node-oriented relationship predicates** — the closed `Predicate` vocabulary gains five
  new pairs for graph edges among nodes and to works: `influenced`/`influenced-by`,
  `authored`/`authored-by`, `member-of`/`has-member`, `about`/`discussed-in`, and
  `part-of`/`has-part`. Each has its inverse, label, and stable sort key, so the existing
  inverse-maintenance and diff-stable ordering carry over unchanged. See `docs/M3-SPEC.md` §2.
- **Domain-appropriate predicate offering** — `Predicate::forward_choices_for(TargetKind)`
  curates which forward predicates the relations dialog should surface first for a given
  target kind (a work offers `cites`/`authored`/…; a school offers `member-of`; a concept
  offers `about`; an event/place offers `part-of`). Advisory only — the vocabulary stays
  fully open. `TargetKind` maps from `NodeType` (`From`), with an entry key counting as a
  work. `forward_choices()` now also includes the new authorable forwards.

### Added — M3 knowledge-graph nodes (`fond-bib`, PR 1 of the sequence)

- **Node files (`nodes/<slug>.md`)** — first-class non-work entities (person / concept /
  school / event / place / uncatalogued work) with a `label`, `aliases`, free-form external
  `identifiers` (wikidata/viaf/orcid/…), and typed `relations`. Structurally a note (YAML
  frontmatter + Markdown prose), so the graph is homogeneous: a relation target may be a
  citation key or a node slug. `fond_bib::Node` (`parse`/`to_text`) round-trips; an omitted
  `node-type` defaults to `concept`, an unknown one is a parse error, and a missing/blank
  `label` is rejected. See `docs/M3-SPEC.md` §1.
- **Node slugs** — `fond_bib::key::node_slug(label)` produces an ASCII-folded kebab-case base
  slug; collision suffixing reuses `assign_key` as citation keys do.
- **Library wiring** — `Library::init` now creates `nodes/` alongside the other dirs, plus
  `node_path` / `node_slugs` / `load_node` / `write_node`. Purely additive: an empty or
  absent `nodes/` is valid and existing vaults are untouched.
- Internal: `split_frontmatter` is now a shared helper (`util`) reused by both the note and
  node parsers rather than copied.

### Added — M2 search & GUI surfacing

- **Facet indexing** — `fond-index` now indexes faceted tags (`discipline:theology`) into a
  `facet` field, so `facet:discipline` scopes search to items carrying that facet.
  `fond_bib::split_facet` parses the `facet:value` convention.
- **AI text search** — AI-sidecar text (summary/keywords/concepts/claims) is indexed into a
  dedicated `ai` field: searchable in free text and scopable via `ai:` so it can be filtered
  apart from curated text.
- **Typed relations in the detail panel** — the GUI now displays an entry's relations
  grouped by predicate ("Cites", "Critiqued by", …) as navigable links, folding legacy
  untyped `related` into the "Related" group. `Predicate::label()` provides the display text.
- **Typed relations editing** — the entry's "Related…" action is now "Relations…": a
  searchable list where each checked target carries a predicate dropdown
  (`Predicate::forward_choices()`), writing typed forward edges via `set_relations` (which
  maintains each target's inverse edge automatically). One predicate per target; a predicate
  already on disk but outside the authorable set is preserved rather than dropped on save.

## [0.1.0-dev5] — 2026-07-25 — Development build

The first M2 knowledge-base slice, at the `fond-bib` library layer: typed relationships,
reading/workflow note fields, an AI-metadata sidecar, and project-based usage tracking.
GUI/index surfacing of these is deferred (see `docs/M2-SPEC.md`).

### Added — Typed relationships (M2, `fond-bib`)

The first slice of the M2 knowledge-base layer (`docs/M2-SPEC.md` §1). Item relationships
gain typed predicates, upgrading the previous untyped `related` links:

- **`relations:` note frontmatter** with a closed predicate vocabulary (`cites`/`cited-by`,
  `critiques`/`critiqued-by`, `reviews`/`reviewed-by`, `commentary-on`/`has-commentary`,
  `translation-of`/`has-translation`, `edition-of`/`has-edition`, `supersedes`/
  `superseded-by`, `replies-to`/`replied-to-by`, `expands`/`expanded-by`, and the symmetric
  `related`). An unknown predicate is a hard parse error, never a silent broken edge.
- **Automatic inverse maintenance** — asserting `a cites b` writes the forward edge on `a`
  and the `cited-by` inverse on `b`; the symmetric `related` is stored as a plain forward
  edge on both. `Library::{set_relations, add_relation, remove_relation}` keep both sides in
  sync, preserving each note's inbound inverse edges when its own forward edges are edited.
- **`fsck` reconciliation** — `reconcile_relations` (wired into `fsck` report-only; repair
  via `reconcile_relations(true)`) rebuilds derived inverse edges from the forward edges that
  are the source of truth: adds missing, removes orphaned, flags dangling targets. Idempotent.
- **`related` → `relations` migration** — `migrate_related_to_relations` lifts legacy
  untyped links into typed `related` edges and clears the old field; idempotent.

### Added — Reading/workflow fields & AI sidecar (M2, `fond-bib`)

- **Note frontmatter fields** — `progress` (page/of reading position), `cite` (per-entry
  short form + preferred style), and `tasks` (per-item to-dos). All optional and elided when
  empty, so existing notes round-trip byte-identically.
- **`ai/<key>.yml` sidecar** — AI-generated summary/keywords/concepts/claims/counterarguments
  with a provenance header (model id, timestamp, source hash). Committed but never
  authoritative: `write_ai` writes solely to `ai/`, structurally guaranteeing AI output can
  never overwrite curated note/entry fields. `AiMetadata`, `Library::{load_ai, write_ai}`.

### Added — Projects & usage tracking (M2, `fond-bib`)

- **`projects/<slug>.yml`** — a named project declaring the Typst documents it comprises.
  `Project`, `Library::{load_project, save_project, project_slugs}`.
- **Usage scan** — `scan_usage`/`write_usage` parse each project's Typst documents for
  `@key` citations and build the reverse "used in" map, persisted to `.kartoteka/usage.json`
  (derived, gitignored — never written into notes). `fsck` flags project documents that
  don't exist.

## [0.1.0-dev4] — 2026-07-24 — Development build

The full headless roadmap (Milestones 1–5 plus cross-cutting search) is implemented — the
plain-file vault, Zotero migration (BibTeX + SQLite), CSL bibliography output (SBL/Chicago)
and annotated Typst, tantivy search over metadata/notes/annotations/PDF text, PDF text
extraction and embedded annotation import/export via PDFium, and acquisition
(DOI/arXiv/ISBN + drop-a-PDF-in). A GTK4/libadwaita **GUI** (`kartoteka-gtk`) now sits on
top of it, with a flatpak packaging scaffold. Detailed per-milestone notes are below.

### Added — Zotero-parity pass (GUI)

A batch of features bringing the GUI toward Zotero's everyday workflow:

- **Collections sidebar** — a leftmost pane with "All entries" and one row per collection;
  filter the list by collection, create collections, and set an entry's membership.
- **In-app PDF reader** — a **Read** button opens a built-in reader that rasterizes pages
  with PDFium (`fond-doc::render_page` → `gdk::MemoryTexture`), with page navigation and
  zoom. No Poppler (GPL) — pure PDFium (BSD). The system viewer remains as *Open externally*.
- **Add from URL** — scrape a web page's citation `<meta>` tags (Highwire `citation_*`,
  Dublin Core, Open Graph) into an entry and attach a linked PDF when the page offers one.
- **Duplicate detection & merge** — *Find duplicates…* groups entries by DOI / ISBN /
  folded title+year and merges a group into one, folding tags, attachments, annotations,
  and note prose and rewriting collection membership.
- **Bulk PDF import** — *Add folder of PDFs…* recursively identifies and attaches every PDF
  under a folder, and **Find PDF** locates an open-access PDF for a DOI via Unpaywall.
- **Tag manager** — *Manage tags…* renames or deletes any tag library-wide.
- **Sortable list** — sort by Title / Author / Year with an ascending/descending toggle.
- **Manual New item** — a form covering 13 Hayagriva item types with the common fields.
- **Cite-while-you-write** — *Cite…* (Ctrl+K) searches the library and copies a Typst `@key`
  citation to the clipboard; the detail pane also has a **Cite** button.
- **Related items** — link entries to each other (symmetric links stored in note
  frontmatter), navigable from clickable links in the detail pane; plus a **Locate** menu
  (open DOI / Google Scholar).

### Added since dev1 — GTK GUI

- **`kartoteka-ui-gtk`** — the Linux GTK4/libadwaita frontend crate (binary `kartoteka-gtk`),
  a thin shell over `fond-bib`/`fond-index`. Open a library (remembered across launches),
  a sidebar list of entries with a live author/title/key filter, and a detail pane showing
  the selected entry's YAML and note. Follows the Fond house style (adw `ToolbarView`,
  `HeaderBar`, `ToastOverlay`, `navigation-sidebar`). App id
  `io.github.calstfrancis.Kartoteka`.
- **Hamburger menu** with the house-standard **System / Light / Dark** theme toggle
  (persisted), **Reindex search**, and **About**.
- **Acquire dialog** (headerbar `+`) — add a reference by **DOI / arXiv / ISBN**; the
  network lookup runs on a worker thread (UI stays responsive) and the entry is added and
  the list refreshed on completion.
- **Field-scoped search** — the sidebar search now queries the `fond-index` tantivy index
  (`author:`/`title:`/`tag:`/`type:`/`year:`, relevance-ranked), with a substring fallback.
- **Structured detail pane** — title, byline, Type/Key/DOI/ISBN/Tags/Status/Rating rows, PDF
  attachment status (present/missing + size/pages), annotation count, and note prose, with
  **Edit note** and **Open PDF** buttons.
- **Edit note dialog** — edit tags, read status, rating, and prose, written to
  `notes/<key>.md`.
- **Open PDF** — open an entry's attachment in the system viewer.
- **Export bibliography…** (menu) — pick a collection + CSL style + format (reference list
  or annotated `.typ`) and save to a file.
- The search index now opens an existing index on library open rather than rebuilding it
  every launch (rebuild on demand via **Reindex search**).
- **Add PDF…** (menu) — drop a PDF in: identify it (DOI-sniff or embedded metadata) on a
  worker thread, create the entry, and attach the PDF. Needs PDFium at runtime.
- **Drag-and-drop** a PDF onto the window to add it (same identify-and-attach flow).
- **Import…** (menu) — import a BetterBibTeX `.bib` and, optionally, a Zotero
  `zotero.sqlite` (collections + notes), with an overwrite toggle; runs on a worker thread.
- **Back up (git commit)…** (menu) — commit the library to git (local snapshot; init if
  needed). Attachments and `.kartoteka/` are gitignored, so only the plain records commit.
- **GitHub remote push** — **Sign in to GitHub…** via the OAuth device flow (token in the
  system keyring); the backup dialog can push to GitHub after commit, creating the repo and
  remote on first push. In-process via libgit2 with the token as HTTPS credentials (no
  shelling out to `git`). Requires a Kartoteka GitHub OAuth App client id (placeholder in
  `github.rs`).
- **Back up to WebDAV…** (menu) — one-way mirror of the whole library (records + attachment
  blobs, skipping `.kartoteka/`) to a WebDAV server (Nextcloud, pCloud, …). URL/username in
  config, password in the keyring; uploads on a worker thread.
- **Status bar** (house style) — the library path on the left and a `v{version}` button on
  the right that opens the embedded **changelog**.
- **Flatpak packaging** — manifest, desktop entry, AppStream metainfo, placeholder icon,
  and `dev-build.sh`/`publish-flatpak.sh` under `packaging/` (bundles PDFium; see
  `packaging/PACKAGING.md`). Not yet validated against a real flatpak-builder run.

## Milestone detail (headless core)

## [Unreleased] — Milestone 5 (acquisition) — complete

### Added

- **`kartoteka acquire`** — populate an entry from an identifier, generating a fresh key:
  - `--doi <doi>` — doi.org content negotiation (BibTeX).
  - `--arxiv <id>` — routed through the arXiv DataCite DOI (`10.48550/arXiv.<id>`).
  - `--isbn <isbn>` — OpenLibrary lookup, mapped to a Hayagriva entry.
  - `--bibtex-file <path>` — from a local BibTeX file (offline).
- **`kartoteka add-pdf <path>`** — the "drop a PDF in → populated entry" flow: identify the
  paper (`--doi`/`--arxiv`/`--isbn`, else **sniff a DOI from the PDF text**, else build a
  minimal entry from **embedded PDF metadata**), then hash + copy the PDF into
  `attachments/` and record it on the entry. `--key` attaches to an existing entry instead.
- **`fond-bib::acquire`** — blocking HTTP (no async runtime), **feature-gated** behind
  `acquire`, built with **rustls** (no OpenSSL). Includes a brace-aware BibTeX sanitizer
  that repairs the unquoted field values (`month=July`) doi.org sometimes returns.
- **`fond-doc`** — `extract_metadata` (embedded title/author + page count) and a
  dependency-free `find_doi` text scanner.
- `Library::add_bibtex` / `add_entries` (generated keys) and `store_attachment` (hash +
  copy a blob and record it on an entry's note).

## [Unreleased] — Search (cross-cutting) + Milestone 4.2

### Added

- **`fond-index`** — tantivy full-text search over the library: metadata (`author`,
  `title`, `year`, `type`), note prose, annotation snippets, and extracted PDF text, all in
  one index under `.kartoteka/index/` (derived, rebuildable from the files).
- **`kartoteka search <query>`** — free-text plus field scoping (`author:cone
  tag:christology type:book year:1970`), `--limit`, `--json`. Ranked by relevance.
- **`kartoteka reindex`** now also rebuilds the search index; when PDFium is available it
  indexes present PDF attachments' text too (so search reaches inside PDFs).
- Rebuild-from-scratch guarantee tested: deleting `.kartoteka/index/` and reindexing yields
  identical results.

## [Unreleased] — Milestone 4 (documents), slice B: PDF text extraction

### Added

- **`fond-doc`** — PDF text extraction via `pdfium-render`, binding to a native PDFium
  library at runtime (resolved from `PDFIUM_LIB_PATH`, else the system path). `extract_text`
  returns per-page text; `full_text()` is the form fed to search. Text-only build (no image
  feature). PDFium is BSD-3 and never vendored into the repo.
- **`kartoteka pdf-text <key>`** — extract and print the text layer of an entry's present
  PDF attachment (found via its blake3 blob).
- CI now downloads a PDFium prebuilt binary (Linux + Windows) so the extraction test runs
  in CI rather than skipping.

## [Unreleased] — Milestone 4.4: embedded PDF annotations

### Added

- **`kartoteka import-annots <key>`** — read highlights/underlines/strikeouts embedded in
  an entry's PDF attachment into its sidecar (`annots/<key>.json`), capturing the
  highlighted text as the re-anchoring snippet. Idempotent (stable content-derived ids).
- **`kartoteka export-annots <key> -o <pdf>`** — write the sidecar's highlights into a copy
  of the entry's PDF.
- **`fond-doc`** — `extract_annotations` (markup annotations + snippet via the text layer)
  and `embed_highlights`; `fond-bib::Annotation::imported` (stable id) + `AnnotationSidecar::upsert`.
  Round-trip tested (embed → save → extract). With this, **the roadmap is complete**.

## [Unreleased] — Milestone 4 (documents), slice A: annotation sidecar

### Added

- **Annotation sidecar** (`annots/<key>.json`) — the `fond-bib::annotation` model from
  `DATA-MODEL.md`: highlights/underlines/strikeouts/notes anchored on page + quadpoints +
  a text snippet (with surrounding context) for re-anchoring against a differently-produced
  copy of the same PDF. Pretty-printed JSON with stable field order, round-trip tested.
- `Library::load_annotations` / `write_annotations`, and **fsck** validation of sidecars:
  unparseable JSON, filename/inner-key mismatch, and a `pdf_hash` that matches no
  attachment recorded on the entry.
- **`kartoteka annots <key>`** — list an entry's annotations (`--json` for the raw sidecar).

The PDF-touching parts of Milestone 4 (text extraction into the search index, and
importing/exporting embedded PDF annotations) are the next slice; they need `pdfium-render`
and a PDFium native library at runtime.

## [Unreleased] — Milestone 3 (bibliography output)

### Added

- **`kartoteka bib <collection> --style <name>`** — render a collection as a formatted CSL
  reference list. **SBL** (`sbl`) and **Chicago notes** (`chicago-notes`) both work, plus
  `chicago-author-date`, any style in Hayagriva's archive by name or CSL id (e.g. `apa`,
  `ieee`), or an arbitrary `--style-file <path.csl>`. `--html` for HTML output.
- **`kartoteka annotated-bib <collection> --style <name>`** — emit a Typst document pairing
  each rendered reference with its note prose, in the collection's own order. `-o <file>`
  or stdout.
- New `fond-bib::render` module wrapping Hayagriva's CSL `BibliographyDriver`.
- **Bundled SBL style** (`assets/styles/sbl-fullnote-bibliography.csl`, CC BY-SA 3.0) —
  Hayagriva's archive does not carry it. Redistributed verbatim with attribution; see
  `assets/styles/LICENSE-STYLES.md` and `docs/LICENSES.md`.

## [Unreleased] — Milestone 2 (migration)

### Added — slice B: Zotero SQLite augmentation

- **`kartoteka import --from-bibtex <bib> --zotero-db <zotero.sqlite>`** — augment a
  `.bib` import with data only Zotero's store holds. Read-only; never writes to Zotero.
  - **Collections** → `collections/<slug>.yml`, with items linked to citation keys by
    **DOI, then title+year** (the citation key isn't in Zotero's core tables). Empty
    collections (no matched items) are skipped; subcollection parentage noted.
  - **Notes** — Zotero child notes are converted from HTML to Markdown and merged into
    the parent entry's note prose; standalone notes (no parent) are counted and reported.
  - **Migration report** extended: collections created, notes merged, standalone notes
    skipped, and Zotero items referenced by a collection/note that matched no entry.
- New `fond-bib::zotero` module (read-only Zotero reader) and a dependency-free
  HTML→Markdown converter for note bodies. Adds `rusqlite` (bundled SQLite).

### Added — slice A: BibTeX import

- **`kartoteka import --from-bibtex <file>`** — import a BetterBibTeX / BibLaTeX `.bib`
  library. **Citation keys are preserved** so existing `@key` references in Typst
  documents keep working (keys unsafe as a cross-platform filename are skipped and
  reported, never silently remapped).
  - Bibliographic data via Hayagriva's BibLaTeX converter; `keywords` → note `tags`;
    entries stay pure Hayagriva.
  - Attachment `file` references are resolved, hashed (blake3), and copied into
    `attachments/`, with a record written to the note. Handles Zotero's
    `title:path:mimetype` form and `;`-separated lists.
  - **Migration report** of everything that did not map cleanly: key collisions,
    unsafe keys, missing attachment files, and source fields Kartoteka does not track.
  - `--overwrite`, `--no-attachments`, `--attachment-base` flags.
- `today_iso()` date helper (dependency-free) stamps `date-added` on notes created by
  import.

Still to come in Milestone 2: the Zotero SQLite reader (collections, item relations,
proper attachment resolution) and PDF annotation extraction (partly Milestone 4).

## [0.1.0] — unreleased (Milestone 1: the vault)

The headless vault. No UI, no network, no PDFs yet (see `docs/ROADMAP.md`).

### Added

- **Cargo workspace** with the crate split from `docs/ARCHITECTURE.md`: `fond-bib`,
  `fond-vault`, `fond-doc` (stub), `fond-index` (stub), and `kartoteka-cli`. Shared
  `fond-*` crates are MIT; the app crate is proprietary.
- **`fond-bib`** — the on-disk library model:
  - One-key-per-file `entries/<key>.yml`, kept as pure Hayagriva.
  - `notes/<key>.md` with YAML frontmatter (tags, read-status, rating, dates,
    attachment records) and Markdown body; bare-body notes round-trip byte-identically.
  - `collections/<slug>.yml` ordered key lists.
  - Citation-key generation `<lastname><year><titleword>` with ASCII diacritic folding,
    leading-article stripping, and stable `b`/`c`/… collision suffixes.
  - Deterministic `library.yml` regeneration (ascending-key order), committed and always
    reproducible from `entries/`.
- **`fond-vault`** — in-process git via vendored libgit2 (never shells out): init,
  structured status, stage, commit (handles the first commit), a git-config identity
  helper, and a `notify`-based recursive watcher for the future Zerkalo live seam.
- **`kartoteka` CLI** — `init`, `add` (from file or stdin, auto-keys entries), `list`,
  `show`, `reindex`, `fsck`, `status`, `commit`; `--json` on the read commands.
- **`kartoteka fsck`** reports filename/key mismatches, unparseable entries, dangling
  collection references, and missing / orphaned / hash-mismatched attachment blobs
  (blake3), exiting non-zero when anything is found.
- **Tests** — round-trip focused: YAML in → model → YAML out stable; `library.yml`
  regeneration byte-stable and reproducible after deletion; git init → commit → local
  clone → read back; fsck detecting every seeded problem.
- **CI** on Linux and Windows (fmt, clippy `-D warnings`, tests).
- Planning docs under `docs/`: `ARCHITECTURE.md`, `DATA-MODEL.md`, `LICENSES.md`,
  `ROADMAP.md`.
