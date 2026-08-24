# Kartoteka

A plain-file reference manager and PDF/EPUB library — a Zotero replacement for writing in
Typst. Part of the **Fond** suite (with Zerkalo and Skrizhal). Kartoteka (Картотека) is
the card catalogue: the inventory that lets you find anything in the fond.

**Plain files are the source of truth.** Your library is Hayagriva YAML, Markdown notes,
and JSON annotation sidecars in a git repo — human-readable, editable in `vim`, with no
opaque database. The search index and metadata cache are disposable and rebuild from the
files with one command. `library.yml` is regenerated on every write, so
`#bibliography("library.yml")` in Typst is always current.

> Status: **0.7.0 "Whole Volume"**, released 2026-08-24. A full GTK4/libadwaita desktop app
> (`kartoteka-ui-gtk`) ships alongside the headless CLI, distributed as a flatpak — see
> **Install** below. See `docs/ROADMAP.md` for what's still ahead.

## Features

- A plain-file library — Hayagriva YAML entries, Markdown notes — that lives in a git
  repository and stays readable without Kartoteka.
- A Zotero-style spreadsheet view of your entries, with optional columns for tags,
  status, and any custom field you define; drag-to-reorder columns and bulk actions
  (tag/collection/delete) across a multi-selection.
- Import from Zotero (BetterBibTeX and the Zotero SQLite store), or acquire references by
  DOI, arXiv, or ISBN. Drop a PDF or EPUB in and it identifies itself.
- Built-in PDF and EPUB readers with highlighting, underline/strikeout, freestanding
  notes, in-document and whole-book search, reading-position resume, and undo/redo.
- Typed relations and a knowledge-graph layer — link entries to people, concepts, and
  schools of thought, not just to each other — visualized as an interactive relations
  map, either centered on one entry or across the whole library, with most-connected/
  most-cited analytics.
- Exact and fuzzy duplicate detection, with one-click merging.
- Bibliography output in SBL and Chicago styles, and annotated Typst documents.
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

`docs/ARCHITECTURE.md`, `docs/DATA-MODEL.md`, `docs/LICENSES.md`, `docs/ROADMAP.md`,
`packaging/PACKAGING.md`, `CHANGELOG.md`.

## Licensing

The application (`kartoteka-*`) is proprietary; the shared `crates/fond-*` libraries are
MIT. See `LICENSE` and each crate's `LICENSE`, and `docs/LICENSES.md` for the dependency
audit.
