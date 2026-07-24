# Changelog

All notable changes to Kartoteka are recorded here. Kartoteka is part of the Fond suite.

## [0.1.0-dev2] — 2026-07-24 — Development build

The full headless roadmap (Milestones 1–5 plus cross-cutting search) is implemented — the
plain-file vault, Zotero migration (BibTeX + SQLite), CSL bibliography output (SBL/Chicago)
and annotated Typst, tantivy search over metadata/notes/annotations/PDF text, PDF text
extraction and embedded annotation import/export via PDFium, and acquisition
(DOI/arXiv/ISBN + drop-a-PDF-in). A GTK4/libadwaita **GUI** (`kartoteka-gtk`) now sits on
top of it, with a flatpak packaging scaffold. Detailed per-milestone notes are below.

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
- **Structured detail pane** — title, byline, Type/Key/DOI/ISBN/Tags rows, PDF attachment
  status (present/missing + size/pages), annotation count, and note prose.
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
