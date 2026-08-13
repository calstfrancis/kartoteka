# Kartoteka — STATUS / START HERE

**Read this first when picking up Kartoteka cold** (e.g. after a cleared chat). It records
where the project actually is, how the docs fit together, and what's committed vs. not.
Last updated: **2026-08-11.**

---

## ⚠️ Two different milestone numbering schemes — do not conflate

Kartoteka has **two independent tracks**, and both use the letter/word "milestone." This is
the single most confusing thing about the docs:

1. **`ROADMAP.md` "Milestone 1–5"** — the *original project brief*. **All done.** The vault
   (M1), Zotero migration (M2), bibliography output (M3), documents/PDF (M4), acquisition
   (M5), and cross-cutting search are all implemented and shipped (see dev1–dev4 history).
   Note: §4.5 there is itself stale — it says bitmap rendering "not yet needed," but the GTK
   app has had a working in-app PDF viewer for a while. Worth a cleanup pass independent of
   the M4 work below.

2. **The knowledge-base extension track — "M1 / M2 / M3 / M4 / M5"** — a *newer* track that
   grew out of a large brainstorm (a ChatGPT feature dump, 23 sections), now extended past
   the brainstorm's own scope. This is what `M2-SPEC.md`, `M3-SPEC.md`, `M4-SPEC.md`,
   `M5-SPEC.md`, `DATA-MODEL-EXTENSIONS.md`, and `M2-GUI-PLAN.md` describe.
   - **Extension-M5** = the full PDF+EPUB reader. **Tier 1 and Tier 2 are both fully
     complete**: Tier 1 — 5A (typed attachment routing, fixing the
     EPUB-attachment-opens-the-PDF-viewer bug), 5B (EPUB rendering + chapter/TOC navigation
     via an embedded WebKitGTK 6 `WebView`), 5C (EPUB highlighting — chapter+snippet anchor,
     WebKit-native selection + DOM search-and-wrap), 5D (EPUB body text in the search
     index). Tier 2 — PDF `Progress` resume/save, underline/strikeout/note drawing gestures,
     an outline/bookmarks panel, an in-reader text search bar, a colour picker + inline
     per-page annotation list, continuous/vertical scroll (a toggle alongside the
     page-by-page view, eagerly rendered on first use rather than virtualized), and
     Zotero-style document page numbering (shows/navigates by a PDF's own `/PageLabels`
     printed page number, not just the raw file position — added beyond the original Tier 2
     list, on request). Everything above verified end-to-end headless, not just compiled.
     A further round of PDF reader UX polish also shipped on request, beyond both tiers'
     original lists: the Contents outline became a persistent sidebar instead of a popover, a
     live drag preview shows the highlight/underline/strikeout region while dragging (not
     just after release), snapshot-based undo/redo covers every annotation mutation
     (Undo/Redo buttons + Ctrl+Z/Ctrl+Shift+Z), and "This page"'s inline annotation list
     gained editable notes (parity with the whole-document "Annotations…" dialog). Still open:
     Tier 3's portable annotation export and Tier 4's multi-attachment chooser (`M5-SPEC.md`).
   - **Extension-M4** = the map toward a "full-fledged" citation manager beyond the original
     brief and the brainstorm both — live PDF annotation, Zerkalo vault adoption, and the
     smaller GUI/platform gaps. **Tier 1's PDF-annotation work (4A, all three phases) and
     all of Tier 2 are done** (`M4-SPEC.md`); 4B (Zerkalo vault adoption) is the only Tier 1
     item still open, and is Zerkalo-repo work.
   - **Extension-M1** = the original vault (`DATA-MODEL.md`) — the baseline these extend.
   - **Extension-M2** = typed relations, facets, small note fields, AI sidecar, projects/usage
     — **built and tested** (see below).
   - **Extension-M3** = knowledge-graph nodes (people/concepts/schools) + author IDs —
     **complete (all 8 PRs built & tested)**: library layer (node files, predicates,
     polymorphic targets + relation hosts, fsck, indexing) + GUI (Nodes manager, nodes as
     relation targets, node detail/neighbours, author→node linkage) (`M3-SPEC.md`).

When a doc says "M2/M3" it means the **extension track**. When `ROADMAP.md` says
"Milestone 2/3" it means the **original brief**. They are unrelated.

---

## Where things stand right now

### Committed + tagged
- **`v0.1.0-dev9`** (commit + tag `v0.1.0-dev9`, **unpushed**) — three library-management
  features, each in CLI + GUI:
  - **Delete entries** — `Library::delete_entry` (removes the entry + sidecars, strips inbound
    relation edges, drops collection membership, GCs unshared attachment blobs); CLI
    `kartoteka delete <key> [-y]`; a destructive "Delete…" button in the detail panel.
  - **Edit citation info** — a structured field editor (type/title/author/year/publisher/
    DOI/ISBN) via `entry::EntryFields` + `Library::edit_fields`, which rewrites only changed
    fields (exotic fields preserved) and validates by a parse round-trip; GUI "Edit citation…".
  - **EPUB import** — new `fond-doc::epub` (zip + quick-xml) reads OPF Dublin Core; ISBN
    enrichment via OpenLibrary with an OPF-only offline fallback; CLI `add-epub`, GUI
    "Add EPUB…". Added deps → `cargo-sources.json` regenerated (only additions).
  **Not pushed.** Ready for Cal to run `./dev-build.sh` in `kartoteka/`.
- **`v0.1.0-dev8`** (commit + tag `v0.1.0-dev8`, **unpushed**) — bugfix on top of dev7: the
  dev7 flatpak crashed at startup (`FieldNotFound("kind")` abort) on any library carrying a
  search index built before the node work, because PR5 added a `kind` schema field and
  `SearchIndex::open` did `get_field("kind").expect(...)` against the persisted old schema.
  `open` now returns a `SchemaMismatch` error instead of panicking; the GUI rebuilds the index
  once from the authoritative files, the CLI prints a "run `kartoteka reindex`" message. New
  `fond-index` regression tests cover the legacy- and current-schema open paths. `Cargo.lock`
  only moved the two local crate versions (no dependency change) so `cargo-sources.json` stays
  byte-identical. **Not pushed.** Ready for Cal to re-run `./dev-build.sh` in `kartoteka/`.
- **`v0.1.0-dev7`** (commit + tag `v0.1.0-dev7`, pushed via `dev-build.sh`) — **the complete M3
  knowledge-graph track**, folding in everything since dev5: the extension-M2 index/GUI
  surfacing plus M3 PRs 1–8.
  - **PRs 1–3** (`11b8ad9`/`c733bdc`/`63c36a4`, first shipped in the dev6 tag) — node file type
    (`NodeType`/`NodeFrontmatter`/`Node`, `node_slug`, `nodes/` in `Library`); node-oriented
    `Predicate` vocabulary + `TargetKind`/`forward_choices_for`; `resolve_target` + `RelationHost`
    so relations span notes ∪ nodes.
  - **PR 4** (`faf030a`) — `fsck` node checks (`unparseable_nodes`, `malformed_node_slugs`);
    `reconcile_relations` made corruption-tolerant.
  - **PR 5** (`1dc5437`) — `fond-index` node indexing with a `kind` discriminator (`kind:node`,
    `type:person`), reusing title/type/note + alias/identifier; `SearchHit.kind`.
  - **PR 6** (`1501d0a`) — GUI Nodes manager (menu → "Nodes…"): filterable list + create/edit
    editor. No delete yet.
  - **PR 7** (`bf9973f`) — GUI: `relations_dialog` offers nodes as targets with predicate
    dropdowns curated by `forward_choices_for(TargetKind)`.
  - **PR 8** (`c8745b7`) — GUI: node editor "Relations" neighbours section + node-label
    resolution in entry relation lists; "Link author…" creates/links a person node per author.
  - `cargo-sources.json` unchanged (only the two local crate versions moved; no dependency set
    change, so the generator emits byte-identical output).
  **Not pushed.** Ready for Cal to run `./dev-build.sh` in `kartoteka/`.

### Working tree
Clean once the dev7 bump is committed.

### Test / quality state
- Whole workspace: **127 tests passing, 0 clippy warnings** at last run (up from 117 after
  M3; dev9 adds delete_entry, edit_fields, EPUB-OPF, and book_yaml tests across fond-bib and
  fond-doc).
- `cargo test --workspace` and `cargo clippy --workspace` are the gates.

---

## Document map (what each file is for)

| Doc | Purpose |
|---|---|
| **`STATUS.md`** (this) | Start-here: current state, track disambiguation, doc map. |
| `ARCHITECTURE.md` | Foundational invariants (authoritative files vs. disposable `.kartoteka/`). |
| `ROADMAP.md` | The **original** Milestone 1–5 brief (all done). |
| `DATA-MODEL.md` | The approved M1 on-disk vault format (entries/notes/annots/collections). |
| `DATA-MODEL-EXTENSIONS.md` | The 23-section brainstorm mapped to disk + the scope **decisions**. The master "where does each idea land" table lives here. |
| `M2-SPEC.md` | Extension-M2 implementation spec (relations/facets/fields/AI/projects). Has an "Implementation status" section. |
| `M2-GUI-PLAN.md` | Extension-M2 GUI + index surfacing plan, with ✅/🔨/⏳ status per item. |
| `M3-SPEC.md` | Extension-M3 spec (knowledge-graph nodes). **Complete** — all 8 PRs built and tested. |
| `M4-SPEC.md` | Extension-M4: the map toward a "full-fledged" citation manager (live PDF annotation, Zerkalo vault adoption, remaining GUI/platform gaps), tiered by how much each blocks the core workflow. **In progress.** |
| `LICENSES.md` | Licensing (`fond-*` crates MIT; app proprietary). |

**Reading order for a cold start:** this file → `DATA-MODEL-EXTENSIONS.md` (the map) →
whichever of `M2-SPEC.md` / `M2-GUI-PLAN.md` / `M3-SPEC.md` / `M4-SPEC.md` matches the task.

---

## What's done vs. next (extension track)

Done and tested (extension-M2), in `crates/fond-bib` + `crates/fond-index` + GUI:
- Typed relations: `relation.rs` (closed `Predicate` vocab, `inverse()`, `label()`,
  `forward_choices()`), `set_relations`/`add_relation`/`remove_relation`,
  `reconcile_relations` (in `fsck` + `--fix`), `migrate_related_to_relations`.
- Note fields: `progress`, `cite`, `tasks`; `split_facet`.
- AI sidecar: `ai.rs` + `load_ai`/`write_ai` (never overwrites curated fields — structural).
- Projects/usage: `project.rs`, `scan_usage`/`write_usage` → `.kartoteka/usage.json`.
- Index: `facet` + `ai` fields. GUI: relations display + editing dialog.

Next candidates (pick per Cal) — **superseded by `M4-SPEC.md`'s tiering**, kept here as a
one-line pointer rather than duplicated:
- **Extension-M3** (`M3-SPEC.md`): knowledge-graph nodes — **complete (all 8 PRs)**.
- **Extension-M4** (`M4-SPEC.md`): live PDF annotation — **Tier 1's 4A (all 3 phases) and
  all of Tier 2 done**; 4B (Zerkalo vault adoption, a Zerkalo-repo task) is the only open
  Tier 1 item; Tier 3 (Windows UI) and Tier 4 (browser capture, bulk ops, library
  quick-switcher) not started.
- **Extension-M5** (`M5-SPEC.md`): the full PDF+EPUB reader. **Tier 1 and Tier 2 both fully
  complete, verified end-to-end headless (real xdotool-driven interaction, not just
  compiled).** Tier 1 — 5A (typed attachment routing), 5B (EPUB rendering + chapter/TOC nav
  via WebKitGTK 6), 5C (EPUB highlighting — chapter+snippet anchor, WebKit-native selection +
  DOM search-and-wrap), 5D (EPUB text into the search index). Tier 2 — PDF `Progress`
  resume/save, underline/strikeout/note drawing gestures, an outline/bookmarks panel,
  in-reader text search, a colour picker + inline per-page annotation list,
  continuous/vertical scroll (a toggle alongside the page-by-page view), and Zotero-style
  document page numbering (`/PageLabels`-aware display and jump-by-typed-label). A further
  round of PDF reader UX polish also shipped: Contents as a persistent sidebar, a live drag
  preview while highlighting, snapshot-based undo/redo (buttons + Ctrl+Z/Ctrl+Shift+Z), and
  editable notes in "This page"'s inline annotation list. Still open:
  Tier 3 (a portable annotation export), Tier 4 (a multi-attachment chooser) — see that doc
  for the full tiered list.

---

## Working rules that bite if forgotten (from root `CLAUDE.md`)

- **Kartoteka version lives in TWO files:** `kartoteka-cli/Cargo.toml` **and**
  `kartoteka-ui-gtk/Cargo.toml` — keep in sync. `fond-*` crates keep their own version.
- **Cargo workspace:** when `Cargo.lock` changes, regenerate `packaging/cargo-sources.json`
  (offline flatpak build fails without it). Cal runs `dev-build.sh` / `publish-flatpak.sh`.
- **Never** `git add -A`; stage explicitly. Commit/push/tag only when Cal asks (the dev-build
  and release workflows are the exception that mandate commit+tag).
- **No metainfo `<release>` entry for dev builds.** No release-name constant for Kartoteka.
- **Headless GUI smoke test recipe** (verified working): Xvfb + `GDK_BACKEND=x11` +
  unset `WAYLAND_DISPLAY` + full XDG isolation (`XDG_CONFIG_HOME`/`DATA`/`CACHE`/`STATE`) +
  `dbus-run-session`. Preseed `$XDG_CONFIG_HOME/kartoteka/gui.json` with `library_path` to
  auto-open a library. `xdotool` drives clicks. Seed a throwaway library via the
  `kartoteka` CLI (`init` + `add`). **Never point it at Cal's real data.**
