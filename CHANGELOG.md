# Changelog

All notable changes to Kartoteka are recorded here. Kartoteka is part of the Fond suite.

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
