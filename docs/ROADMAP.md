# Kartoteka — Roadmap

Status: **planning draft, awaiting approval.**

Milestones from the brief, broken into concrete, individually shippable tasks. Each task
is small enough to land, test, and commit on its own. Milestone 1 is detailed because it
is what gets implemented this session (after approval); later milestones are scoped
coarsely and will be broken down when reached.

Testing philosophy (from the brief): **test the round-trips, not the tautologies.** YAML
in → model → YAML out byte-identical modulo formatting; git commit → clone → read; nuke
`.kartoteka/` → reindex → identical results. No schema-migration system until there is a
schema worth migrating.

---

## Milestone 0 — workspace scaffold (this session, before M1 tasks)

- **0.1** Create the cargo workspace and the six member crates per `ARCHITECTURE.md` §2
  (`crates/fond-{bib,vault,doc,index}`, `kartoteka-cli`, and empty placeholder manifests
  for the two UI crates — no code). `fond-doc`/`fond-index` are stubs at M1.
- **0.2** Relicense: proprietary `LICENSE` for app crates, MIT retained on `fond-*`
  (pending Cal's confirmation — `LICENSES.md` flag 1).
- **0.3** Workspace-level `Cargo.toml` with shared dependency versions; add only the M1
  dependencies (`hayagriva`, `serde`, `serde_yaml_ng`, `git2`, `blake3`, `walkdir`,
  `directories`, `clap`, `thiserror`, `notify`). No PDF/tantivy/network deps yet.
- **0.4** CI (Linux + Windows) building the workspace and running tests headless. Fmt +
  clippy gates.
- **0.5** A fixture library under `tests/fixtures/` (hand-written `entries/`, `notes/`,
  `collections/`) used by round-trip tests across the milestone.

---

## Milestone 1 — the vault (this session's implementation target)

Read/write the on-disk layout, generate keys, regenerate `library.yml`, git
init/commit/status, `fsck`. No network, no PDFs. Delivered as `kartoteka` CLI subcommands.

### fond-bib

- **1.1** `Entry` load/save: parse `entries/<key>.yml` via `hayagriva`, one key per file;
  enforce filename == inner key. Round-trip test: load → save → byte-identical mod
  formatting, over the fixture library.
- **1.2** Note model: parse/emit `notes/<key>.md` (YAML frontmatter + Markdown body) with
  the `tags`/`read-status`/`rating`/`date-added`/`date-read`/`attachments` schema
  (`DATA-MODEL.md`). Round-trip test.
- **1.3** Collection model: parse/emit `collections/<slug>.yml` (ordered keys +
  name/description). Round-trip test.
- **1.4** Citation-key generation: `<lastname><year><titleword>` with ASCII folding,
  article stripping, and the `b`/`c`/… collision suffix from `DATA-MODEL.md`. Unit tests
  over tricky names (diacritics, corporate authors, missing author, missing date,
  colliding trio). Key assignment is stable and checked against the on-disk key set.
- **1.5** `library.yml` regeneration: deterministic concatenation of `entries/` in
  ascending-key order, canonical field order. Test: regenerating twice is byte-identical;
  editing one entry changes only that block.
- **1.6** `LibraryError` enum (`thiserror`); no `Box<dyn Error>` in the public API
  (`ARCHITECTURE.md` §4).

### fond-vault

- **1.7** Repo open/init via `git2` (vendored libgit2, in-process — never shell out).
- **1.8** `status`: porcelain-equivalent structured status (staged/unstaged/untracked/
  conflicted paths) as plain Rust types.
- **1.9** `stage` + `commit` with a supplied identity and message. Test: init → write
  entry → stage → commit → the commit exists and contains the file.
- **1.10** Clone-and-read round-trip test: commit a fixture library, clone to a temp dir,
  read it back with `fond-bib` — identical model. (This is the "survives git" guarantee.)
- **1.11** `notify` file-watcher wrapper delivering change events over a channel (the
  Zerkalo live seam; no consumer yet, but the API lands here).
- **1.12** `VaultError` enum; conflict paths surfaced, never auto-resolved silently
  (except regenerable `library.yml`, per `DATA-MODEL.md` §conflicts — deferred to when a
  real conflict path exists; M1 just surfaces).

### kartoteka-cli

- **1.13** `kartoteka init [path]` — create/initialise a library repo (layout +
  `.gitignore` + `git init`).
- **1.14** `kartoteka add` — add an entry from a Hayagriva snippet / minimal fields;
  generate its key; write `entries/<key>.yml`; regenerate `library.yml`. (Network-free —
  metadata *lookup* is M5; this is manual/paste entry.)
- **1.15** `kartoteka list` / `kartoteka show <key>` — read-side surface over the vault.
- **1.16** `kartoteka reindex` — regenerate `library.yml` (and, later, the index/cache)
  fully from files. At M1 this proves the rebuild-from-scratch guarantee even before there
  is an index to rebuild.
- **1.17** `kartoteka fsck` — report dangling collection references, filename/key
  mismatches, and (schema present now, blobs later) attachment record/blob discrepancies:
  missing / orphaned / hash-mismatch. Attachment hashing via `blake3` lands here even
  though PDFs are M4, because the *records* exist from M1.
- **1.18** `kartoteka status` / `kartoteka commit -m` — thin passthroughs to `fond-vault`.
- **1.19** Human output by default; `--json` for scripting/UI consumption on the
  read commands.

### Milestone 1 exit criteria

- A library can be created, populated by hand, committed, cloned, and read back with a
  faithful round-trip.
- `rm -rf library/library.yml && kartoteka reindex` reproduces it byte-for-byte.
- `kartoteka fsck` on a deliberately broken fixture reports every seeded problem and
  exits non-zero.
- Everything works headless on Linux **and** Windows in CI.

---

## Milestone 2 — migration (import from Zotero)

Lossless-enough import that Cal can actually switch. Import BetterBibTeX `.bib` (via
`biblatex`) and/or the Zotero SQLite store: entries, attachment paths, tags, collections,
and extractable existing PDF annotations. **Write a report of anything that did not map
cleanly** rather than dropping it silently.

Slice A (`.bib` importer) is **done**; slice B (Zotero SQLite) is next.

- 2.1 ✅ `.bib` → `Entry` via `hayagriva::io::from_biblatex_str`; keys preserved.
- 2.2 ⬜ Zotero SQLite reader (read-only) for tags, collections, attachment paths, item
  relations.
- 2.3 ◐ Tags → `notes/` frontmatter (done). Collections + read-status await the SQLite
  reader (`.bib` has no clean collection representation).
- 2.4 ✅ Attachment path capture → records (hash + copy blobs into `attachments/`);
  missing files reported.
- 2.5 ⬜ Existing PDF annotation extraction → `annots/` sidecars where extractable
  (depends on `fond-doc`; may partly land with M4).
- 2.6 ✅ **Migration report**: unmapped fields, key collisions, unsafe keys, missing
  attachments.

---

## Milestone 3 — bibliography output

- 3.1 `kartoteka bib <collection> --style <sbl|chicago-notes|…>` — formatted CSL output
  via `hayagriva`. SBL (`society-of-biblical-literature-fullnote-bibliography`) and
  Chicago-notes (`chicago-notes-bibliography`) must both work.
- 3.2 CSL style bundling per `LICENSES.md` (CC-BY-SA assets + attribution).
- 3.3 `kartoteka annotated-bib <collection>` — emit a `.typ` file pairing each rendered
  CSL entry with its `notes/` prose.
- 3.4 Style/collection round-trip and golden-output tests.

---

## Milestone 4 — documents

- 4.1 `fond-doc`: PDFium render + text-layer extraction + outline via `pdfium-render`.
- 4.2 Extracted PDF text feeds the search index (`fond-index`).
- 4.3 Annotation sidecar read/write (`annots/<key>.json`, `DATA-MODEL.md` schema).
- 4.4 Import annotations from, and export them to, **embedded** PDF annotations, with
  snippet-based re-anchoring so highlights survive a re-downloaded PDF.
- 4.5 `directories`-based PDF cache; present/missing attachment state wired end-to-end.

---

## Milestone 5 — acquisition

- 5.1 DOI / ISBN / arXiv metadata lookup (`reqwest` **blocking + rustls**, one request at
  a time).
- 5.2 PDF metadata sniffing (embedded metadata → candidate fields).
- 5.3 "Drop a PDF in → populated entry" end-to-end: hash, store blob, sniff, look up,
  generate key, write entry + attachment record.

---

## Search — cross-cutting from Milestone 1

Search is not a milestone; it grows across them, all in one query with field-scoped
syntax:

- **M1**: metadata (`fond-bib` fields) + note text + tags/read-status, backed by
  `fond-index` (tantivy + SQLite cache). Even before PDFs, `reindex` builds this from
  files.
- **M4**: PDF full text joins the same index.
- Query syntax supports field scoping (`author:cone tag:christology status:read
  "black theology"`), all against the disposable index that any `reindex` rebuilds.

`fond-index` may only ever *cache* what the authoritative files already say
(`ARCHITECTURE.md` §1). Deleting `.kartoteka/` and reindexing must yield identical search
results.
