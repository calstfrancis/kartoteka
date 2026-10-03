# Kartoteka

A plain-file reference manager and PDF/EPUB library for writing in Typst. Part of the
**Fond** suite (with Zerkalo, the Typst writing app, and Skrizhal). Documents open in
[Pereplyot](https://github.com/calstfrancis/pereplyot), the PDF/EPUB reader. Kartoteka
(Картотека) is the card catalogue: the inventory that lets you find anything in the fond.

**Plain files are the source of truth.** Your library is Hayagriva YAML, Markdown notes,
and JSON annotation sidecars in a git repo — human-readable, editable in `vim`, with no
opaque database. The search index and metadata cache are disposable and rebuild from the
files with one command. `library.yml` is regenerated on every write, so
`#bibliography("library.yml")` in Typst is always current.

> Status: **0.18.2 "Lending Desk"**. A full GTK4/libadwaita desktop app (`kartoteka-gtk`)
> ships alongside the headless CLI (`kartoteka`), distributed as a flatpak — see **Install**
> below. `CHANGELOG.md` is the record of what has shipped; `docs/UX-REVIEW.md` is the current
> list of what to improve next.

## Features

- A plain-file library — Hayagriva YAML entries, Markdown notes — that lives in a git
  repository and stays readable without Kartoteka.
- Multiple notes per entry — a primary note plus any number of lighter child notes, each
  listed by a title derived from its own first line and indexed for search individually —
  plus standalone notes not attached to any entry (hamburger menu → "Standalone notes…").
  All note editors autosave as you type; nothing is lost by closing the dialog.
- A sortable spreadsheet view of your entries, with optional columns for tags,
  status, and any custom field you define; drag-to-reorder columns and bulk actions
  (tag/collection/delete) across a multi-selection.
- A Bookshelf view — a cover-grid alternative to the spreadsheet, showing just your book
  entries with cover art (fetched by ISBN from OpenLibrary and cached locally) and a
  reading-status badge, for when Kartoteka is doing double duty as a reading tracker.
- Import from Zotero (BetterBibTeX and the Zotero SQLite store), or acquire references by
  DOI, arXiv, or ISBN. Drop a PDF or EPUB in and it identifies itself.
- Attachments are automatically renamed to a human-readable "Author Year - Title.ext" from
  the entry's citation info, instead of keeping a download's original filename; a
  "Rename attachments to citations…" action (hamburger menu, or `kartoteka
  rename-attachments`) backfills attachments added before this existed.
- Reads PDFs and EPUBs in [Pereplyot](https://github.com/calstfrancis/pereplyot), a
  separate reader app (install it from the same flatpak repo), with highlights, notes,
  reading position, and page numbering saved straight into this library. "Annotations…"
  opens the entry's highlights and notes there too. Without Pereplyot, "Read" falls back
  to your default document viewer.
- Nestable collections you can rename, reparent, and delete from the sidebar, with
  drag-and-drop to file entries into them from either the spreadsheet or Bookshelf view.
- A guided "Set up backup…" wizard walks through GitHub sign-in, repository creation,
  and enabling automatic backups in one flow.
- Consistent printed-page numbering (with a manual override for scans with no embedded
  page labels) across the reader, the Annotations dialog, and the CLI.
- Typed relations and a knowledge-graph layer — link entries to people, concepts, and
  schools of thought, not just to each other — visualized as an interactive relations
  map, either centered on one entry or across the whole library, with most-connected/
  most-cited analytics.
- Exact and fuzzy duplicate detection, with one-click merging.
- Bibliography output in SBL, Chicago, Turabian, APA, MLA and every other bundled CSL style
  (`kartoteka styles` lists them), for a collection or the whole library, and annotated Typst
  documents.
- Full-text search over metadata, notes, annotations, and PDF/EPUB text.
- Sync via git, GitHub, or WebDAV.

## Layout

- `crates/fond-bib` — Hayagriva model, on-disk layout, citation keys, `library.yml` (MIT)
- `crates/fond-vault` — in-process git (vendored libgit2) + filesystem watching (MIT)
- `crates/fond-doc` — PDF/EPUB rendering, text extraction, and annotations (MIT)
- `crates/fond-index` — tantivy full-text search + derived cache (MIT)
- `kartoteka-cli` — the headless `kartoteka` binary (proprietary)
- `kartoteka-ui-gtk` — the GTK4/libadwaita desktop app, `kartoteka-gtk` (proprietary)

The `fond-*` crates are shared with the rest of Fond and stay UI-framework-agnostic.

## Install

The desktop app is distributed as a flatpak from Cal's self-hosted repo:

```sh
flatpak remote-add --user calstfrancis \
  https://calstfrancis.github.io/flatpak/calstfrancis.flatpakrepo
flatpak install calstfrancis io.github.calstfrancis.Kartoteka
```

See `packaging/PACKAGING.md` for building the flatpak yourself.

## Try the CLI

```sh
cargo build
kartoteka init my-library
printf '_:\n  type: book\n  title: The Destiny of Man\n  author: Berdyaev, Nikolai\n  date: 1937\n' \
  | kartoteka -L my-library add
kartoteka -L my-library list
kartoteka -L my-library fsck
```

## Documentation

`docs/ARCHITECTURE.md`, `docs/DATA-MODEL.md`, `docs/LICENSES.md`, `docs/UX-REVIEW.md`,
`packaging/PACKAGING.md`, `CHANGELOG.md`. `docs/STATUS.md` records how the milestone docs
(`M2`–`M5-SPEC.md`, `NOTES-SPEC.md`) fit together; the original brief (`ROADMAP.md`) is no longer
in the tree.

## Licensing

The application (`kartoteka-*`) is proprietary; the shared `crates/fond-*` libraries are
MIT. See `LICENSE` and each crate's `LICENSE`, and `docs/LICENSES.md` for the dependency
audit.
