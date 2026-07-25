# Kartoteka M2 — GUI & Index Surfacing Plan

Status: **living plan.** Tracks the GUI/index work that surfaces the M2 `fond-bib` layer
(see `docs/M2-SPEC.md` for the storage design and `docs/DATA-MODEL-EXTENSIONS.md` for the
scope decisions). This file is the single place the front-end plan for all M2 work is
written down.

The `fond-bib` and `fond-index` layers are **done and tested** (dev5 + the post-dev5 index
work). What remains is GTK wiring in `kartoteka-ui-gtk/src/ui/app_window.rs`.

---

## Legend

- ✅ done — landed and tested
- 🔨 in progress — being implemented now
- ⏳ deferred — planned, not started

---

## 1. Search index (`fond-index`) — ✅ done

- ✅ `facet` field: faceted tags (`discipline:theology`) indexed so `facet:discipline`
  scopes search. `fond_bib::split_facet` parses the convention.
- ✅ `ai` field: AI-sidecar text (summary/keywords/concepts/claims/counterarguments) indexed,
  free-text searchable and scopable via `ai:` so it filters apart from curated text.
- Tests: `crates/fond-index/tests/search.rs::facet_scoping_and_ai_text_are_indexed`.

## 2. Typed relations — display (detail panel) — ✅ done

- ✅ Detail panel groups an entry's `relations` by predicate ("Cites", "Critiqued by", …) as
  navigable link buttons; legacy untyped `related` folds into the "Related" group.
- ✅ `Predicate::label()` supplies display text.
- Reference: `app_window.rs`, the relations block after the note-derived fields.

## 3. Typed relations — editing dialog — ✅ done

**Goal:** replace the untyped "Related…" checkbox dialog with one that authors *typed*
forward edges, writing through `Library::set_relations` (which maintains inverse edges).

**Verified end-to-end** via an isolated headless launch (Xvfb + `dbus-run-session` + full
XDG isolation, per the root `CLAUDE.md` gotchas): the dialog renders with per-row predicate
dropdowns (greyed until the row is checked); driving check → pick predicate → Save wrote
`predicate: cites → cone1970black` on the subject's note and auto-materialized
`predicate: cited-by, inverse: true` on the target's note. No panics.

### Design

- Rename `related_dialog` → `relations_dialog`; button label "Related…" → "Relations…".
- Reuse the existing filterable checkbox-list of every other entry, **plus a per-row
  predicate `DropDown`**:
  - The dropdown model is the labels of `Predicate::forward_choices()` — the 10 authorable
    predicates (symmetric `Related` + the primary direction of each asymmetric pair). Inverse
    halves are omitted; they're maintained automatically on the other endpoint.
  - A row's dropdown is **sensitive only when its checkbox is active** (an unchecked row has
    no predicate to pick).
  - Initialized from the key's current **forward** `relations` (inverse edges are not shown —
    they belong to other notes): a target with a forward edge is checked and its predicate
    selected; others default to unchecked / `Related`.
- **On Save:** collect `Relation::forward(predicate, target)` for every checked row and call
  `set_relations(key, &forward)`. That single call rewrites this key's forward edges and
  reconciles every target's inverse edge. Then `reload_current` to refresh the panel.

### Scope boundaries (deliberate)

- **One predicate per target** in this dialog (the common case). A target with *multiple*
  distinct forward predicates is legitimate on disk but rare; the dialog shows/writes one.
  Power users hand-edit for the multi-predicate case; documented in the dialog's doc comment.
- **Legacy `related` is not folded in here.** The dialog manages typed `relations` only;
  lifting legacy links is the job of the global `migrate_related_to_relations`. (Both still
  display merged in the detail panel, §2.) This avoids the two-sided-symmetric-`related`
  cleanup problem inside a per-key dialog.
- **Data-safety:** if a current forward edge uses a predicate outside `forward_choices()`
  (e.g. a hand-authored inverse half), it is appended to that dialog's option list so Save
  round-trips it instead of silently dropping it.

### Supporting `fond-bib` addition — ✅ done

- `Predicate::forward_choices() -> [Predicate; 10]` — the authorable predicate set for the
  dropdown, stable order.

## 4. Remaining GUI work — ⏳ deferred (future increment)

Kept out of this pass to keep it focused and low-risk:

- **Facet chip grouping** in the entry editor / tag manager (display side; the search side is
  done in §1). Group tag chips by `split_facet` facet name.
- **"Promote AI keyword → tag"** action: read `ai/<key>.yml`, offer its keywords, write the
  chosen one into the note's `tags` (one-directional, never automatic).
- **"Used in" panel:** the `fond-bib` scan (`scan_usage`/`write_usage`) exists; the GUI needs
  (a) a scan trigger (on library open / reindex) and (b) a `usage.json` loader to show
  "Used in: Project (doc)" on the detail panel.
- **Derived global task view:** aggregate note `tasks` across the library into one list.
- **Progress / cite / tasks editing** in the entry editor (currently only round-tripped on
  disk, not editable in-GUI).

---

## Test / verification strategy

- `fond-bib` / `fond-index` logic is covered by crate tests (relations writer/reconcile,
  facet split, index scoping).
- GUI dialogs are verified by build + clippy + a manual smoke launch (the app is
  GTK4/libadwaita; headless capture needs the XDG/dbus isolation documented in the root
  `CLAUDE.md` gotchas). No automated GTK UI tests in this repo yet.
