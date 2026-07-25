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
     **PRs 1–6 of 8 built** — the whole non-GUI library layer (node file type; predicate
     vocabulary; polymorphic targets + relation hosts; fsck node checks; index node indexing)
     **plus PR 6 GUI** (Nodes manager window: list + create/edit editor). PRs 7–8 (remaining
     GUI) still to do (`M3-SPEC.md`).

When a doc says "M2/M3" it means the **extension track**. When `ROADMAP.md` says
"Milestone 2/3" it means the **original brief**. They are unrelated.

---

## Where things stand right now

### Committed + tagged (unpushed)
- **`v0.1.0-dev6`** (commit `4e31c2e`, tag `v0.1.0-dev6`) — folds in the extension-M2 index +
  GUI surfacing (`5614503`) **and** M3 PRs 1–3:
  - **PR 1** (`11b8ad9`) — `crates/fond-bib/src/node.rs` (node file type
    `NodeType`/`NodeFrontmatter`/`Node`), `node_slug` in `key.rs`, `nodes/` wired into
    `Library` (`init` + `node_path`/`node_slugs`/`load_node`/`write_node`), `split_frontmatter`
    factored into `util`.
  - **PR 2** (`c733bdc`) — node-oriented `Predicate` pairs (influenced/authored/member-of/
    about/part-of + inverses); extended `inverse`/`label`/`predicate_key`/`forward_choices`;
    `TargetKind` + `forward_choices_for` (advisory curation, `From<NodeType>`).
  - **PR 3** (`63c36a4`) — `resolve_target` (`Target::{Entry,Node,Dangling}`) + private
    `RelationHost` refactor so relations span notes ∪ nodes; no M2 behaviour change absent nodes.
  - `cargo-sources.json` was regenerated (byte-identical — only local crate versions moved).
  **Not pushed.** Ready for Cal to run `./dev-build.sh` in `kartoteka/`.

### Committed on top of dev6 (not tagged, not pushed) — M3 PRs 4–6
- **PR 4** — `fsck` node checks: parse (`unparseable_nodes`) + well-formed-slug filename
  (`malformed_node_slugs`); `reconcile_relations` made corruption-tolerant (skips + reports an
  unparseable host instead of erroring).
- **PR 5** — `fond-index` indexes `nodes/` into the shared schema with a `kind` discriminator
  (`kind:node`, `type:person`), reusing title/type/note + adding alias/identifier;
  `SearchHit.kind` added; `kartoteka search --json` emits `kind`.
- **PR 6** — GUI Nodes manager (`kartoteka-ui-gtk`): menu → "Nodes…" opens a filterable node
  list + create/edit editor (type/label/aliases/identifiers/prose); slug-stable on edit,
  label-derived collision-free slug on create; relations preserved; silent reindex on save.
  No delete yet. Verified end-to-end headless (list + editor render correctly, no panics).

All under `CHANGELOG.md` `[Unreleased]`; fold into the next dev tag when one is prepped.

### Working tree
Clean once PR 6 is committed.

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
- **Extension-M3** (`M3-SPEC.md`): knowledge-graph nodes — **PRs 1–6 done** (non-GUI library
  layer + the GUI Nodes manager). Remaining: **PR 7** — nodes as relation targets in the
  relations dialog (`relations_dialog` at ~`app_window.rs:2770`) + node-appropriate predicates
  via `Predicate::forward_choices_for(TargetKind)`; **PR 8** — node detail/neighbours view +
  author→node linkage. See `docs/M3-SPEC.md` §6.

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
