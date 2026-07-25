# Kartoteka M2 — Implementation Spec

> **Naming note.** "M2" is the **knowledge-base extension track**, *not* `ROADMAP.md`'s
> "Milestone 2" (Zotero migration, long done). See `docs/STATUS.md` for the disambiguation
> and current project state. M3 (knowledge-graph nodes) is spec'd in `docs/M3-SPEC.md`.

Status: **fond-bib + index + GUI implemented (see "Implementation status" below); the leftover
GUI items are tracked in `M2-GUI-PLAN.md` §4.**

Implements the M2 slice of `DATA-MODEL-EXTENSIONS.md`: typed relations, semantic facets,
the small frontmatter fields (`progress`/`cite`/`tasks`), the `ai/<key>.yml` sidecar, and
project/usage. Grounded in the current `fond-bib` code (`note.rs`, `library.rs`).

The whole M2 schema is **additive to `NoteFrontmatter`** plus **one new sidecar type** and
**one new declaration file**. Nothing touches `entries/` (pure Hayagriva stays pure).

---

## 1. Typed relations — the load-bearing piece

### 1.1 Types (`fond-bib/src/relation.rs`, new module)

```rust
use serde::{Deserialize, Serialize};

/// Closed relationship vocabulary. Each predicate has exactly one inverse; a self-inverse
/// predicate (`Related`) maps to itself. Unknown predicates are a hard parse error — a typo
/// predicate must never become a silent broken edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Predicate {
    Cites,          CitedBy,
    Critiques,      CritiquedBy,
    Reviews,        ReviewedBy,
    CommentaryOn,   HasCommentary,
    TranslationOf,  HasTranslation,
    EditionOf,      HasEdition,
    Supersedes,     SupersededBy,
    RepliesTo,      RepliedToBy,
    Expands,        ExpandedBy,
    Related,        // self-inverse
}

impl Predicate {
    pub fn inverse(self) -> Predicate {
        use Predicate::*;
        match self {
            Cites => CitedBy,             CitedBy => Cites,
            Critiques => CritiquedBy,     CritiquedBy => Critiques,
            Reviews => ReviewedBy,        ReviewedBy => Reviews,
            CommentaryOn => HasCommentary, HasCommentary => CommentaryOn,
            TranslationOf => HasTranslation, HasTranslation => TranslationOf,
            EditionOf => HasEdition,      HasEdition => EditionOf,
            Supersedes => SupersededBy,   SupersededBy => Supersedes,
            RepliesTo => RepliedToBy,     RepliedToBy => RepliesTo,
            Expands => ExpandedBy,        ExpandedBy => Expands,
            Related => Related,
        }
    }
    /// A predicate that is its own inverse (symmetric relation).
    pub fn is_self_inverse(self) -> bool { self.inverse() == self }
}

/// One relationship edge as stored in a note's frontmatter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Relation {
    pub predicate: Predicate,
    /// Target citation key (M2) or node slug (M3 — resolver already accepts either).
    pub target: String,
    /// True for a Kartoteka-maintained inverse edge; false/absent for a user-authored one.
    /// Derived: `fsck --fix` rebuilds every `inverse: true` edge from forward edges.
    #[serde(default, skip_serializing_if = "is_false")]
    pub inverse: bool,
}

fn is_false(b: &bool) -> bool { !*b }
```

**Serde note:** unknown predicate → `serde` fails the enum, surfacing as `BibError::Frontmatter`
with the path (same channel as today's frontmatter errors). That is the "typo predicate can't
silently rot" guarantee, for free.

### 1.2 `NoteFrontmatter` additions

```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub relations: Vec<Relation>,

// `related` is RETAINED unchanged for back-compat (legacy untyped "see also").
// Migration lifts it into `relations` with predicate: related (§1.5).
```

All new fields keep the `skip_serializing_if` discipline so bare notes stay byte-identical
and existing vaults round-trip unchanged (the `round_trips_through_model` test still holds).

### 1.3 Forward vs. inverse — the invariant

A note file carries two kinds of edge, distinguished by the `inverse` flag:

- **Forward edges** (`inverse: false`) — *authored here*. Authoritative. The user owns them.
- **Inverse edges** (`inverse: true`) — *maintained by Kartoteka* from some other note's
  forward edge. **Fully derived**: they can be dropped and regenerated from all forward
  edges in the vault with no information loss.

That single invariant is what makes both the writer and `fsck` simple:

> The set of forward edges is the whole truth. Every inverse edge is a function of it.

### 1.4 Writer — generalize `set_related` → `set_relations`

Mirrors today's `set_related` (old/new diff, write-only-what-changed, sorted for stable
diffs), extended to carry the mapped inverse predicate instead of a symmetric copy.

```rust
/// Replace `key`'s FORWARD edges with `forward` (inverse edges on `key` are preserved —
/// they belong to other notes). Maintains the matching inverse edge on each target.
pub fn set_relations(&self, key: &str, forward: &[Relation]) -> Result<()> {
    // Normalize: drop self-links, dedup by (predicate, target), force inverse=false,
    // and canonicalize self-inverse edges (§1.6) so only one endpoint holds the forward.
    let new = normalize_forward(key, forward);
    let old = self.forward_relations(key)?;               // inverse==false edges on key

    // 1. Rewrite key's note: new forward edges + key's EXISTING inverse edges, re-sorted.
    let mut own = self.load_note(key)?.unwrap_or_default();
    let kept_inverse = own.frontmatter.relations.iter().filter(|r| r.inverse).cloned();
    own.frontmatter.relations = new.iter().cloned().chain(kept_inverse).collect();
    sort_relations(&mut own.frontmatter.relations);
    self.write_note(key, &own)?;

    // 2. For each ADDED forward edge, add its inverse on the target.
    for r in added(&old, &new) {
        self.add_inverse_edge(&r.target, r.predicate.inverse(), key)?;
    }
    // 3. For each REMOVED forward edge, drop its inverse on the (old) target.
    for r in removed(&old, &new) {
        self.remove_inverse_edge(&r.target, r.predicate.inverse(), key)?;
    }
    Ok(())
}
```

`add_inverse_edge`/`remove_inverse_edge` are the per-target helpers, each load→mutate→
write-only-if-changed, exactly like the `new.difference(&old)` / `old.difference(&new)`
loops in today's `set_related`. Edge identity for all set ops is the pair
`(predicate, target)`; `inverse` is not part of identity.

**Convenience wrappers** so callers/GUI stay ergonomic:
`add_relation(key, predicate, target)` and `remove_relation(key, predicate, target)` load
the forward set, insert/remove one, and call `set_relations`.

### 1.5 `fsck` — inverse-edge reconciliation

Because inverse edges are derived, reconciliation is a full rebuild, not a fragile patch:

```
expected_inverse : Map<key, Set<(Predicate, key)>>   // empty
for each note N with key K:
    for each forward edge (P, T) on N:          // inverse == false
        if unknown predicate  -> ERROR (already caught at parse)
        if T not in entries/ (and, M3, not in nodes/) -> WARN dangling-target
        expected_inverse[T].insert((P.inverse(), K))

for each note N with key K:
    actual   = { (r.predicate, r.target) : r.inverse }        on N
    expected = expected_inverse[K]
    missing  = expected - actual   // --fix: add as inverse:true
    orphan   = actual   - expected // --fix: remove (stale inverse)
    report missing/orphan; rewrite N only if --fix and it changed
```

- **Read-only `fsck`** reports `missing`/`orphaned`/`dangling-target`/`unknown-predicate`
  as warnings — never crashes (M1 `fsck` philosophy).
- **`fsck --fix`** makes inverse edges exactly `expected_inverse`, leaving forward edges
  untouched. Deterministic and idempotent: a hand-edit that desyncs inverses self-heals.
- Ship this in the **same PR** as the `relations` field — the writer and the reconciler are
  two views of one invariant and must not diverge in time.

### 1.6 Self-inverse (`related`) — symmetric forward on both

For a symmetric predicate "which side authored it" is meaningless. Rather than a
lexicographic tiebreak (the original draft) — which added coordination for no user-visible
benefit — the self-inverse `Related` is stored as a **plain forward edge on *both*
endpoints**, exactly matching legacy `set_related`'s symmetric storage. No `inverse: true`
edges are involved for `Related`. `counterpart(Related, subject)` therefore returns a
`Relation::forward(Related, subject)`, not an inverse edge.

Reconciliation treats the two cases differently, which is the whole reason `Predicate::
is_self_inverse()` exists:

- **Asymmetric inverse edges** (`inverse: true`) are *fully derived* — dropped and
  regenerated from forward edges. An inverse edge with no forward behind it is **orphaned
  and removed**.
- **Self-inverse `related` forward edges** are *symmetric-completed* — a missing reciprocal
  is **added**, never silently removed (removal must go through `set_relations`, which drops
  both sides). This mirrors `set_related` precisely.

*(Implemented: `crates/fond-bib/src/relation.rs`, `library.rs::{set_relations, add_relation,
remove_relation, reconcile_relations, migrate_related_to_relations}`. PRs #1–#4 of the
sequence below are done and tested.)*

### 1.7 Migration `related` → `relations`

`kartoteka migrate` (opt-in, never automatic):
- For each note, lift every legacy `related: [T…]` into forward `Relation{Related, T}`,
  then clear `related`.
- Run the §1.5 reconciler afterward — it dedups the old two-sided symmetric storage into the
  canonical one-forward-one-inverse form (§1.6) and materializes any missing inverses.
- Idempotent: a note with no legacy `related` is untouched.
- `related` stays a valid field indefinitely for anyone who never migrates; the reader
  treats it as untyped "see also" and the GUI can show it merged with `Related` predicate
  edges (**decision needed**: merged list vs. two sections — see Open items).

---

## 2. Semantic facets (convention, no schema change)

No type changes. A tag containing `:` is `facet:value`; `/` nests within a value.

```rust
/// Parse "discipline:theology/systematic" -> ("discipline", "theology/systematic").
/// A tag with no ':' is facetless (returns None facet).
pub fn split_facet(tag: &str) -> (Option<&str>, &str);
```

- `tags:` stays `Vec<String>` on disk — hand-editable, unknown facets allowed.
- `fond-index` indexes the facet and value as separate terms so search can scope
  (`facet:discipline = theology`). Known-facet vocabulary is a soft doc convention; the
  index/UI just group by whatever facets appear.
- GUI: group the tag chips by facet; a facetless tag stays a plain chip.

---

## 3. Small frontmatter fields

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Progress { pub page: u32, pub of: u32 }     // §8

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CitePrefs {                                  // §13
    #[serde(default, skip_serializing_if = "Option::is_none")] pub short: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub preferred_style: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Task {                                       // §20
    pub text: String,
    #[serde(default)] pub done: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub due: Option<String>,
}

// added to NoteFrontmatter, all optional/elided:
#[serde(default, skip_serializing_if = "Option::is_none")] pub progress: Option<Progress>,
#[serde(default, skip_serializing_if = "CitePrefs::is_empty")] pub cite: CitePrefs,
#[serde(default, skip_serializing_if = "Vec::is_empty")] pub tasks: Vec<Task>,
```

Indexing: `progress`/`read-status` feed a "currently reading" filter; open `tasks` feed a
derived global task view (`.kartoteka/`, aggregated — the per-note tasks are authoritative).

---

## 4. AI sidecar — `ai/<key>.yml` (committed, regenerable)

```rust
// fond-bib/src/ai.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct AiMetadata {
    pub schema: u32,                 // = 1
    pub generated_by: String,        // model id, e.g. "claude-opus-4-8"
    pub generated_at: String,        // RFC3339
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>, // which attachment/text this came from
    #[serde(default, skip_serializing_if = "Option::is_none")] pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub concepts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub claims: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub counterarguments: Vec<String>,
}
```

- `Library::load_ai(key)` / `write_ai(&AiMetadata, key)` mirroring the annotation-sidecar
  accessors; path `ai/<key>.yml`. Committed (not `.gitignore`d).
- **Hard boundary:** nothing in the write path ever copies AI fields into `NoteFrontmatter`
  or `entries/`. Promotion (AI keyword → real `tag`) is an explicit GUI action that reads
  `ai/` and writes `notes/`; the reverse never happens.
- `fond-index` indexes AI text but tags each field AI-origin so results filter.

---

## 5. Projects & usage (§21/§22)

- **Authoritative:** `projects/<slug>.yml` (`Project { name, documents: Vec<PathBuf> }`),
  a new declaration file alongside `collections/`. Accessors mirror `load_collection`.
- **Derived:** a `usage` scanner walks each project's `documents` for `@key` citations and
  writes the reverse map to `.kartoteka/usage.json` (rebuilt by `reindex`, never committed,
  never written into `notes/`). GUI reads it for "Used in: …".
- `fsck` warns on a `documents` path that doesn't exist (dangling), same as dangling
  collection keys.

---

## Test obligations

- **Round-trip:** every new field, present and absent, `parse → to_text → parse` equal; a
  bare note still emits byte-identical to input (extend `round_trips_through_model`).
- **Relations writer:** add/remove forward edge materializes/removes exactly one inverse on
  the target; self-inverse canonicalization; write-only-what-changed (no spurious rewrites).
- **`fsck` reconcile:** desync an inverse by hand → `--fix` restores it; orphan inverse
  removed; dangling target warned not crashed; idempotent on a second run.
- **Migration:** legacy two-sided `related` collapses to canonical forward/inverse with no
  edge lost and no duplicate.
- **Unknown predicate:** fails parse as `BibError::Frontmatter` with the path.
- **AI boundary:** writing `ai/` never mutates the note; `load_ai` on a keyless entry → None.

---

## Implementation status

**The entire `fond-bib` layer of M2 is implemented and tested** (PRs #1–#8 below, minus the
GUI/index wiring noted). Landed in `crates/fond-bib`:

- `relation.rs` — `Predicate` (closed vocab + `inverse()`), `Relation`.
- `ai.rs` — `AiMetadata` sidecar + provenance.
- `project.rs` — `Project` + `scan_typst_citation_keys`.
- `note.rs` — `relations`, `progress`, `cite`, `tasks` frontmatter (+ `Progress`/`CitePrefs`/`Task`).
- `library.rs` — `set_relations`/`add_relation`/`remove_relation`, `reconcile_relations`,
  `migrate_related_to_relations`, `load_ai`/`write_ai`, project accessors, `scan_usage`/
  `write_usage`, and `fsck` extended (relation reconcile report + dangling project docs).

**Also landed (index + read-only GUI):**
- `fond-index` — `facet` field (faceted-tag scoping via `facet:`) and `ai` field (AI text,
  scopable via `ai:`); `fond_bib::split_facet` + `Predicate::label()`.
- GTK detail panel — typed relations displayed grouped by predicate, navigable, legacy
  `related` folded in.

**Still open (deliberately deferred to a later increment):**
- **Relation *editing*** — the current "Related…" dialog still writes only untyped
  `related`; a predicate picker (write via `add_relation`/`set_relations`) is the main
  remaining GUI rework in the ~3.3k-line `app_window.rs`.
- **Facet chip grouping** in the entry editor / tag manager (display side; search side done).
- **"Promote AI keyword → tag"** GUI action.
- **"Used in"** panel — needs a scan trigger + `usage.json` loader wired into the GUI; the
  `fond-bib` scan (`write_usage`) is done, the GUI read path is not.
- **Derived global task view** aggregating note `tasks`.

## Suggested PR sequence

1. `relation.rs` (types + `inverse()`) + `NoteFrontmatter.relations` + round-trip tests.
   *(schema only, no writer yet — smallest reviewable unit)*
2. `set_relations` writer + convenience wrappers + writer tests.
3. `fsck` reconciliation (read-only report + `--fix`) + tests. **Bundle with #2 if review
   prefers writer-and-reconciler-together** (they share the invariant).
4. `migrate` command + migration tests.
5. Facet parsing + index terms + GUI grouping.
6. Small fields (`progress`/`cite`/`tasks`) + derived task view.
7. `ai.rs` sidecar + accessors + index + GUI "promote to tag" action.
8. `projects/` declaration + `.kartoteka/usage` scanner + GUI "Used in".

Each PR is independently shippable and additive; a vault written by an earlier PR parses
under a later one and vice-versa (all fields optional).

---

## Open items for Cal

- **GUI:** legacy `related` + typed `Related` edges — one merged "Related" list, or two
  sections? (Affects only presentation; disk model handles both.)
- **`cite.preferred_style`** — validate against a known CSL-style list at write time, or
  accept any string? (Recommend: accept any string M2; validate later.)
