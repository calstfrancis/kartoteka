# Kartoteka M3 — Implementation Spec: Knowledge-Graph Nodes

Status: **complete.** All 8 PRs are **built and tested** — the library layer (node files,
predicate vocabulary, polymorphic targets + relation hosts, fsck checks, indexing) and the GUI
(node manager, nodes as relation targets, node detail/neighbours, author→node linkage). See the
PR sequence below and `docs/STATUS.md`.

> **Naming note.** "M3" here is the **knowledge-base extension track** (M2 = typed relations
> etc., already built; M3 = this), *not* `ROADMAP.md`'s "Milestone 3" (bibliography output,
> long done). See `docs/STATUS.md` for the two-track disambiguation.

Implements the deferred `nodes/` layer from `DATA-MODEL-EXTENSIONS.md` §2: first-class
**non-work entities** (people, concepts, schools, events, places) and the relationships
among them and to citation entries. This is the biggest differentiator from the original
brainstorm (§9 knowledge graph) and the home for author-level identifiers (§1: ORCID / VIAF
/ Wikidata), which cannot live in pure-Hayagriva `entries/`.

Inherits every M1/M2 invariant: pure-Hayagriva `entries/`, one slug per file, `vim`+`git`
fixable, `.kartoteka/` disposable, additive/optional (a library with zero nodes is valid).

The M2 relation design was authored to accept a node slug as a `target` from day one, so M3
adds a file type and a resolver — **not** a schema change to relations.

---

## 1. Node files — `nodes/<slug>.md`

Structurally a note (YAML frontmatter + Markdown prose), so the graph is homogeneous: a
relation `target` may be a citation key **or** a node slug, resolved against `entries/`
first, then `nodes/`.

```markdown
---
node-type: person
label: Augustine of Hippo
aliases: [Augustine, Aurelius Augustinus]
identifiers:
  wikidata: Q8018
  viaf: "66806872"
relations:
  - predicate: influenced
    target: aquinas               # another node slug
  - predicate: authored
    target: augustine0426city     # a real entries/ key
---

Bishop of Hippo, 354–430. Anchor node for the Augustinian tradition facet.
```

### Types (`fond-bib/src/node.rs`, new module)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeType { Person, Concept, School, Event, Place, WorkUncataloged }

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct NodeFrontmatter {
    pub node_type: NodeType,               // defaults to Concept if omitted? see Open items
    pub label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Free-form external identifiers (wikidata, viaf, orcid, lccn, …). A BTreeMap keeps
    /// key order stable for clean diffs; unknown keys are allowed (extensibility).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub identifiers: BTreeMap<String, String>,
    /// Same `Relation` type as notes — forward + maintained inverse edges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<Relation>,
}

pub struct Node { pub frontmatter: NodeFrontmatter, pub body: String }
```

`Node::parse`/`to_text` reuse the exact frontmatter split from `note.rs` (factor
`split_frontmatter` into a shared helper rather than copy it).

### Slugs

Node slugs mirror citation-key discipline (`key.rs`): ASCII-folded, lowercase, hyphenated,
collision-suffixed, **stable once assigned**. From `label` (a person → family-name-ish
slug; a concept → kebab of the label). Filename == slug, and `fsck` flags a file whose inner
data disagrees. A dedicated `node_slug(label)` in `key.rs` (or a shared `slug.rs`).

---

## 2. Predicate vocabulary extension

The M2 `Predicate` enum gains node-oriented predicates (each with its inverse; extend
`inverse()`, `label()`, `predicate_key()`, and the sort key):

| Predicate | Inverse | Typical domain |
|---|---|---|
| `influenced` | `influenced-by` | person/school → person/school |
| `authored` | `authored-by` | person → entry (work) |
| `member-of` | `has-member` | person → school |
| `about` | `discussed-in` | entry/node → concept |
| `part-of` | `has-part` | event/place → event/place |

- These join the existing work-to-work predicates in one enum (no separate type).
- `Predicate::forward_choices()` gains a **context** parameter or a second list so the
  relations dialog can offer domain-appropriate predicates (e.g. `authored` when the target
  is a person, `cites` when it's a work). Keep it advisory, not enforced — the vocabulary
  stays open; the UI just curates.
- The closed-set + unknown-predicate-is-a-parse-error guarantee from M2 still holds.

---

## 3. Target resolution — the core new mechanism

A relation `target` is now polymorphic. Introduce a resolver on `Library`:

```rust
pub enum Target { Entry(String), Node(String), Dangling(String) }

/// Resolve a relation target: an existing entry key, else an existing node slug, else
/// dangling. `entries/` wins on the (discouraged) chance a slug collides with a key.
pub fn resolve_target(&self, target: &str) -> Result<Target>;
```

Everything that previously assumed "target == entry key" consults this:

- **`reconcile_relations`** — the biggest change. Today it iterates `keys_sorted()` (entries)
  and their notes. In M3 the **relation-bearing universe** is *notes ∪ nodes*, and the
  **valid-target set** is *entry keys ∪ node slugs*. So:
  - Gather forward edges from every note **and** every node.
  - `expected_inverse[target]` may land on a note *or* a node file; the writer/reconciler
    must load/write the right file type by resolving the target.
  - Dangling = target resolves to neither. (Same warning, wider domain.)
- **`set_relations` / `add_relation` / `remove_relation`** — generalize from "note of key" to
  "relation host of id", where a host is a note (for an entry key) or a node (for a slug).
  Suggest a small internal `RelationHost` abstraction: load/save `Vec<Relation>` for an id
  regardless of backing file, so the M2 edge algorithms stay unchanged.

This is the one part that genuinely re-touches M2 code; everything else is additive.

---

## 4. `fsck` additions — ✅ **done (PR 4)**

- ✅ Node files parse (`unparseable_nodes`). Filename/slug agreement: since a node stores no
  inner slug and slugs are *stable across label edits*, "agreement" is that the filename is
  itself a well-formed slug (lowercase alnum + single hyphens); a hand-made file with spaces
  or uppercase — unreachable as a target — is flagged (`malformed_node_slugs`). We do **not**
  enforce `node_slug(label) == filename`, which would wrongly flag a legitimately renamed node.
- ✅ Relation targets resolve to entry **or** node (landed in PR 3's `reconcile_relations`).
- ✅ Inverse-edge reconciliation spans notes ∪ nodes (PR 3, §3). PR 4 also made `reconcile`
  corruption-tolerant: an unparseable host is skipped and reported, not fatal.
- Orphaned identifier check is out of scope (we don't validate ORCID/VIAF against the network).

---

## 5. Indexing (`fond-index`) — ✅ **done (PR 5)**

- ✅ Nodes indexed into the same schema, discriminated by a `kind` field (`entry`/`node`), so
  `kind:node augustine` scopes to nodes. A node reuses `title` (label), `type` (node-type),
  and `note` (body), and adds searchable `alias` + `identifier` text (each identifier's scheme
  *and* value indexed). `type:person` finds person nodes.
- ✅ Added a `kind` field to the existing `SearchHit` (no second result type); for a node hit
  `title` is the label and `author`/`year` are empty. `kartoteka search --json` emits `kind`.

---

## 6. GUI (`kartoteka-ui-gtk`)

Deferred sub-track, sequenced after the library layer (mirrors how M2 split lib vs. GUI):

- ✅ **Node list & editor (PR 6)** — shipped as a **Nodes manager window** (menu → "Nodes…"):
  a filterable list of nodes with a **+** to create, and a node editor for `node-type`,
  `label`, `aliases`, `identifiers`, prose. Chosen over a main-pane sidebar filter/mode-toggle
  to keep it self-contained and avoid touching the shared entry list/detail/search; the deeper
  main-pane integration is PR 8's node detail view. Relations preserved on edit; new nodes get
  a collision-free label-derived slug; existing nodes keep their stable slug. No delete yet
  (needs relation cleanup — deferred).
- ✅ **Nodes as relation targets (PR 7)** — `relations_dialog` now lists every node (label +
  type · slug) alongside entries as a checkable target; each row's predicate dropdown is
  curated to the target's kind via `Predicate::forward_choices_for(TargetKind)` (entry = Work;
  node = its `NodeType`), staying advisory (an out-of-set current predicate is appended so Save
  round-trips it). Verified headless: a person node shows Related/Influenced only.
- ✅ **Node detail + neighbours (PR 8)** — the node editor shows a read-only "Relations"
  section: the node's edges grouped by predicate with targets resolved to display names
  (via `group_relations_display`). Entry relation lists likewise resolve node targets to their
  labels. A visual graph view stays deferred (open item 3).
- ✅ **Author-node linkage (PR 8)** — an entry's "Link author…" action creates/links a person
  node per author (family-name slug) and relates it with `authored`; the user then adds §1
  identifiers in the node editor. Verified end-to-end headless (person node + `authored`/
  `authored-by` edges written correctly).

---

## 7. Migration & compatibility

- Purely additive: no `nodes/` dir, or an empty one, is valid; existing vaults untouched.
- `Library::init` should create `nodes/` alongside the other dirs (like `ai/`, `projects/`).
- No data migration needed — nodes are new; author IDs are opt-in when a user makes a person
  node.

---

## Suggested PR sequence

1. ✅ **Done.** `node.rs` (types + parse/to_text via shared frontmatter split) + slug helper
   + round-trip tests. `nodes/` added to `init` (+ `node_path`/`node_slugs`/`load_node`/
   `write_node`). `split_frontmatter` factored into `util`.
2. ✅ **Done.** Predicate vocabulary extension: five node-oriented pairs added to the enum,
   `inverse()`, `label()`, `predicate_key()` (sort), and `forward_choices()`. Added
   `TargetKind` + `Predicate::forward_choices_for(TargetKind)` as the advisory domain-curated
   list (the "context" mechanism), with `From<NodeType>`. Tests cover round-trip, inverses,
   and curation.
3. ✅ **Done.** `resolve_target` (`Target::{Entry,Node,Dangling}`) + a private `RelationHost`
   (note-or-node) refactor of `relations`/`set_relations`/`add`/`remove`/`reconcile_relations`
   to span notes ∪ nodes. `fsck` covers node relations via `reconcile`. No M2 behaviour change
   when no nodes are present (all M2 relation tests still pass) + new node-spanning tests.
4. ✅ **Done.** `fsck` node checks: parse (`unparseable_nodes`) + well-formed-slug filename
   (`malformed_node_slugs`); `reconcile` made corruption-tolerant. Tests added.
5. ✅ **Done.** `fond-index` indexes nodes into the shared schema with a `kind` discriminator
   (`kind:node`), reusing title/type/note and adding alias/identifier; `SearchHit.kind` added.
   Tests added.
6. ✅ **Done.** GUI: Nodes manager window (menu → "Nodes…") — filterable node list + create/
   edit editor (type/label/aliases/identifiers/prose). Verified end-to-end headless.
7. ✅ **Done.** GUI: `relations_dialog` offers nodes as targets with per-row predicate
   dropdowns curated by `forward_choices_for(TargetKind)`. Verified end-to-end headless.
8. ✅ **Done.** GUI: node editor "Relations" neighbours section + node-label resolution in
   entry relation lists (`group_relations_display`/`target_display`); "Link author…" creates/
   links a person node per author and relates it with `authored`. Verified end-to-end headless.

Each PR additive and independently shippable; a vault written by an earlier PR parses under
a later one.

---

## Open items for Cal

1. ~~**`node-type` default**~~ — **Resolved in PR 1: defaults to `concept` when omitted**
   (the forgiving choice, and what lets `NodeFrontmatter` derive `Default`). A missing/blank
   `label`, by contrast, *is* a hard parse error. Revisit if a required `node-type` is wanted.
2. ~~**Predicate domain enforcement**~~ — **Resolved in PR 2: open vocabulary + UI curation**,
   matching M2. `forward_choices_for(TargetKind)` surfaces domain-appropriate predicates but
   never restricts what can be stored; `fsck` does *not* validate predicate domains. Revisit
   only if a domain-mismatch warning proves wanted later.
3. ~~**Graph view depth**~~ — **Resolved by shipping: the grouped-relations list per node is
   the M3 bar** (node editor "Relations" section); a visual graph/neighbours canvas stays
   deferred to a future track. Reopen if a visual graph becomes wanted.
4. ~~**Slug scheme for concepts/events**~~ — **PR 1 implements kebab-of-label for all types**
   (`node_slug`: ASCII-fold each whitespace-separated word, join with `-`). The spec's
   "person → family-name-ish slug" nuance is deferred to the author→node linkage PR (8),
   where a structured author name is available; the library-layer primitive is kebab-of-label.
