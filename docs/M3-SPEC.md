# Kartoteka M3 — Implementation Spec: Knowledge-Graph Nodes

Status: **in progress.** PR 1 (node types + parse/to_text + slug helper + `nodes/` in
`init`) is **built and tested**; PRs 2–8 not yet. See the PR sequence and `docs/STATUS.md`.

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

## 4. `fsck` additions

- Node files parse; filename/slug agreement (mirrors entry key-filename check).
- Relation targets resolve to entry **or** node (extends the M2 dangling-target check).
- Inverse-edge reconciliation spans notes ∪ nodes (§3).
- Orphaned identifier check is out of scope (we don't validate ORCID/VIAF against the network).

---

## 5. Indexing (`fond-index`)

- Index nodes as their own document kind: `label`, `aliases`, `node-type`, `identifiers`,
  and body prose, with a `kind:node` discriminator so search can scope to/from nodes
  (`kind:node augustine`).
- A node hit needs a distinct `SearchHit` shape or a `kind` field on the existing one
  (nodes have no author/year). Prefer adding `kind` + making author/year optional-ish
  (empty for nodes) to avoid a second result type.

---

## 6. GUI (`kartoteka-ui-gtk`)

Deferred sub-track, sequenced after the library layer (mirrors how M2 split lib vs. GUI):

- **Node list & editor** — a view alongside entries (a sidebar filter "Nodes", or a mode
  toggle). Create/edit `node-type`, `label`, `aliases`, `identifiers`, prose.
- **Nodes as relation targets** — the relations dialog (`relations_dialog`) currently lists
  only entries; add nodes to the pickable targets, and offer node-appropriate predicates
  (§2 context).
- **Node detail + graph view** — show a node's relations grouped by predicate (reuse the
  detail-panel grouping already built for entries); optionally a simple graph/neighbours view.
- **Author-node linkage** — offer to create/link a person node from an entry's author
  (this is how §1 author IDs get captured in practice).

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
2. Predicate vocabulary extension (inverse/label/sort/forward-choices) + tests.
3. `resolve_target` + `RelationHost` refactor of `set_relations`/reconcile to span notes ∪
   nodes + tests (the load-bearing PR; keep it isolated).
4. `fsck` node checks + tests.
5. `fond-index` node indexing + `kind` scoping + tests.
6. GUI: node list/editor.
7. GUI: nodes as relation targets + node-appropriate predicates.
8. GUI: node detail / neighbours view + author→node linkage.

Each PR additive and independently shippable; a vault written by an earlier PR parses under
a later one.

---

## Open items for Cal

1. ~~**`node-type` default**~~ — **Resolved in PR 1: defaults to `concept` when omitted**
   (the forgiving choice, and what lets `NodeFrontmatter` derive `Default`). A missing/blank
   `label`, by contrast, *is* a hard parse error. Revisit if a required `node-type` is wanted.
2. **Predicate domain enforcement** — keep the vocabulary fully open (any predicate, any
   target types) with only UI curation, or validate domains in `fsck` (warn on e.g.
   `authored` pointing at another person)? (Recommend open + UI curation, matching M2.)
3. **Graph view depth** — is a grouped-relations list per node enough for M3, with a visual
   graph deferred, or is the visual graph part of the M3 bar?
4. ~~**Slug scheme for concepts/events**~~ — **PR 1 implements kebab-of-label for all types**
   (`node_slug`: ASCII-fold each whitespace-separated word, join with `-`). The spec's
   "person → family-name-ish slug" nuance is deferred to the author→node linkage PR (8),
   where a structured author name is available; the library-layer primitive is kebab-of-label.
