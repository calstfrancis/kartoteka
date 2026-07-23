# Kartoteka

A plain-file reference manager and PDF library — a Zotero replacement for writing in
Typst. Part of the **Fond** suite (with Zerkalo and Skrizhal). Kartoteka (Картотека) is
the card catalogue: the inventory that lets you find anything in the fond.

**Plain files are the source of truth.** Your library is Hayagriva YAML, Markdown notes,
and JSON annotation sidecars in a git repo — human-readable, editable in `vim`, with no
opaque database. The search index and metadata cache are disposable and rebuild from the
files with one command. `library.yml` is regenerated on every write, so
`#bibliography("library.yml")` in Typst is always current.

> Status: **Milestone 1 (the vault)** — headless CLI only. No UI, network, or PDF handling
> yet. See `docs/ROADMAP.md`.

## Layout

- `crates/fond-bib` — Hayagriva model, on-disk layout, citation keys, `library.yml` (MIT)
- `crates/fond-vault` — in-process git (vendored libgit2) + filesystem watching (MIT)
- `crates/fond-doc` — PDF/annotations (MIT, stub until Milestone 4)
- `crates/fond-index` — tantivy + derived cache (MIT, stub)
- `kartoteka-cli` — the `kartoteka` binary (proprietary)

The `fond-*` crates are shared with the rest of Fond and stay UI-framework-agnostic.

## Try it

```sh
cargo build
kartoteka init my-library
printf '_:\n  type: book\n  title: The Destiny of Man\n  author: Berdyaev, Nikolai\n  date: 1937\n' \
  | kartoteka -L my-library add
kartoteka -L my-library list
kartoteka -L my-library fsck
```

## Documentation

`docs/ARCHITECTURE.md`, `docs/DATA-MODEL.md`, `docs/LICENSES.md`, `docs/ROADMAP.md`.

## Licensing

The application (`kartoteka-*`) is proprietary; the shared `crates/fond-*` libraries are
MIT. See `LICENSE` and each crate's `LICENSE`, and `docs/LICENSES.md` for the dependency
audit.
