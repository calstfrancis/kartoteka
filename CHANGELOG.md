# Changelog

All notable changes to Kartoteka are recorded here. Kartoteka is part of the Fond suite.

## [Unreleased]

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
