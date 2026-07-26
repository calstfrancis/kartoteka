# Kartoteka — STATUS / START HERE

**Read this first when picking up Kartoteka cold** (e.g. after a cleared chat). It records
where the project actually is, how the docs fit together, and what's committed vs. not.
Last updated: **2026-07-25.**

---

## ⚠️ Two different milestone numbering schemes — do not conflate

Kartoteka has **two independent tracks**, and both use the letter/word "milestone." This is
the single most confusing thing about the docs:

1. **`ROADMAP.md` "Milestone 1–5"** — the *original project brief*. **All done.** The vault
   (M1), Zotero migration (M2), bibliography output (M3), documents/PDF (M4), acquisition
   (M5), and cross-cutting search are all implemented and shipped (see dev1–dev4 history).

2. **The knowledge-base extension track — "M1 / M2 / M3"** — a *newer* track that grew out of
   a large brainstorm (a ChatGPT feature dump, 23 sections). This is what the `M2-SPEC.md`,
   `M3-SPEC.md`, `DATA-MODEL-EXTENSIONS.md`, and `M2-GUI-PLAN.md` docs describe.
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

### Committed + tagged (unpushed)
- **`v0.1.0-dev7`** (commit + tag `v0.1.0-dev7`) — **the complete M3 knowledge-graph track**,
  folding in everything since dev5: the extension-M2 index/GUI surfacing plus M3 PRs 1–8.
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
- Whole workspace: **117 tests passing, 0 clippy warnings** at last run (up from 94 pre-M3;
  fond-bib carries the node/predicate/relation-host/fsck additions, fond-index the node
  indexing).
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
| `M3-SPEC.md` | Extension-M3 spec (knowledge-graph nodes). Not built yet. |
| `LICENSES.md` | Licensing (`fond-*` crates MIT; app proprietary). |

**Reading order for a cold start:** this file → `DATA-MODEL-EXTENSIONS.md` (the map) →
whichever of `M2-SPEC.md` / `M2-GUI-PLAN.md` / `M3-SPEC.md` matches the task.

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

Next candidates (pick per Cal):
- **Extension-M2 leftovers** (`M2-GUI-PLAN.md` §4): "Used in" panel (needs GUI scan trigger +
  `usage.json` loader), facet chip grouping in the editor, "promote AI keyword → tag", global
  task view, in-GUI editing of progress/cite/tasks.
- **Extension-M3** (`M3-SPEC.md`): knowledge-graph nodes — **complete (all 8 PRs)**. Possible
  future extensions (all deferred): a visual graph/neighbours canvas (spec open item 3); a
  node-side relation editor (relations are currently authored from the entry side only); node
  deletion with relation cleanup (the Nodes manager has no delete yet — vim/git for now).

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
