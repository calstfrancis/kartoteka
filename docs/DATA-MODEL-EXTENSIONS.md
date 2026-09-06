# Kartoteka — Data Model Extensions (proposed M2+)

> **Naming note.** The "M1/M2/M3" here is the **knowledge-base extension track**, *not*
> `ROADMAP.md`'s "Milestone 1–5" (the original brief, all done). See `docs/STATUS.md`.
> This doc is the **master map**: the "Where each brainstorm layer lands" table below places
> all 23 sections of the original brainstorm. Detailed specs: `M2-SPEC.md` (built),
> `M3-SPEC.md` (nodes, not built).

Status: **scope decisions approved (2026-07-24). M2 built; M3 spec'd — see `docs/STATUS.md`.**

This extends `DATA-MODEL.md` (the approved M1 on-disk model) to cover the richer
knowledge-base layers sketched in the feature brainstorm — typed relationships, a
non-work knowledge graph, semantic facets, AI-generated metadata, reading progress,
writing/usage tracking, and per-item tasks.

**Scope decisions are settled** (see the Decisions section at the end). The milestone
column in the table below reflects them: **M2** ships typed relations, semantic facets, the
small frontmatter fields, `ai/` sidecars, and project/usage; **M3** adds the non-work
knowledge-graph nodes.

It is written to **inherit every M1 invariant**, not renegotiate them:

1. `entries/<key>.yml` stays **pure Hayagriva** — no Kartoteka-private keys, ever.
2. **One key (or slug) per file** — merge survivability comes from small per-key files.
3. **`vim` + `git` must always be enough** to fix anything authoritative.
4. **`.kartoteka/` is disposable** — anything rebuildable lives there and is never committed
   as source of truth.
5. **Authoritative vs. derived is a hard line.** Curated facts are committed and
   hand-editable; scan results, indexes, and model output are either derived (`.kartoteka/`)
   or clearly-namespaced regenerable sidecars that never bleed into curated files.

The organizing decision for every feature below is one question: **does this belong in a
committed, hand-curated file, or is it derived/rebuildable?** That split, not the feature
list, is the architecture.

---

## Where each brainstorm layer lands

| Brainstorm section | Lands in | Authoritative? | Milestone |
|---|---|---|---|
| §1 core bib metadata | `entries/<key>.yml` (Hayagriva `serial-number`, `parent`, roles) | ✅ committed | done (M1) |
| §1 author-level IDs (ORCID, VIAF, Wikidata) | `nodes/<slug>.md` person entities | ✅ committed | **M3** |
| §2 attachments | note frontmatter `attachments:` (M1) | ✅ committed | done (M1) |
| §3 annotations | `annots/<key>.json` (M1) | ✅ committed | done (M1) |
| §4 notes | `notes/<key>.md` body (M1) | ✅ committed | done (M1) |
| §5 tags | note frontmatter `tags:` (M1) | ✅ committed | done (M1) |
| §5 nested/faceted tags | tag facet convention (below) | ✅ committed | **M2 (convention)** |
| §6 collections | `collections/<slug>.yml` (M1) | ✅ committed | done (M1) |
| §6 saved searches / smart collections | `.kartoteka/` (derived) | ❌ derived | done (GUI) |
| §7 **typed relationships** | note frontmatter `relations:` (below) | ✅ committed | **M2** |
| §8 reading status | note frontmatter `read-status:` (M1) | ✅ committed | done (M1) |
| §8 **reading progress** | note frontmatter `progress:` (below) | ✅ committed | **M2** |
| §9 **knowledge graph** | `nodes/<slug>.md` + typed relations (below) | ✅ committed | **M3** |
| §10 semantic metadata | tag facets (below) | ✅ committed | **M2 (convention)** |
| §11 reading metadata (rating, dates) | note frontmatter (M1) | ✅ committed | done (M1) |
| §12 **AI metadata** | `ai/<key>.yml` namespaced sidecar (below) | ✅ committed, regenerable | **M2** |
| §13 preferred citation style / short cite | note frontmatter `cite:` (below) | ✅ committed | **M2** |
| §13 citation history | `.kartoteka/` (derived) | ❌ derived | later |
| §14 search index | `.kartoteka/index/` (M1, tantivy) | ❌ derived | done (M1) |
| §15/§16 importers/exporters | code, not storage | — | done (GUI) |
| §17 duplicate detection | `.kartoteka/` scores; merge writes files | ❌ derived | done (GUI) |
| §18 version history | **git itself** — do not reinvent | ❌ (git) | done (git) |
| §19 plugin API | out of scope for the data model | — | later |
| §20 **tasks** | note frontmatter `tasks:` (below) | ✅ committed | **M2** |
| §21/§22 **project & writing/usage** | `projects/<slug>.yml` (declare) + `.kartoteka/` (derive) | mixed | **M2** |
| §23 Typst-native export | code (`fond-doc`) over `library.yml` | — | done (M1) |

**Reading of the table:** most of the brainstorm is already served by M1, or is derived
state that belongs in `.kartoteka/`. The genuinely new *authoritative schema* is small and
concentrated: typed relations, non-work graph nodes, a few frontmatter fields, and one new
regenerable sidecar for AI output. That is the whole proposal.

---

## 1. Typed relationships (§7) — upgrade `related`

M1 already stores relationships: `NoteFrontmatter.related: Vec<String>`, symmetric,
auto-written to both endpoints (`library.rs::set_related`). The only gap is that they are
**untyped** — every edge is a bare "related." The brainstorm's §7 wants predicates
(`cites`, `critiques`, `translation-of`, …).

### Proposed shape

Add an optional typed block alongside the existing `related` (kept for back-compat and for
genuinely untyped "see also" links):

```markdown
---
tags: [christology]
read-status: read
relations:
  - predicate: critiques
    target: barth1932kd
  - predicate: translation-of
    target: gutierrez1971teologia
  - predicate: cited-by            # inverse edge, auto-materialized (see below)
    target: cone1975god
    inverse: true
---
```

### Symmetric vs. directional edges — the one real design decision here

Relationships have **inverse predicates**, not symmetric ones. `A cites B` implies
`B cited-by A`, not `B cites A`. So M1's "write the same thing to both files" rule needs an
inverse table:

| Predicate | Inverse |
|---|---|
| `cites` | `cited-by` |
| `critiques` | `critiqued-by` |
| `reviews` | `reviewed-by` |
| `commentary-on` | `has-commentary` |
| `translation-of` | `has-translation` |
| `edition-of` | `has-edition` |
| `supersedes` | `superseded-by` |
| `replies-to` | `replied-to-by` |
| `expands` | `expanded-by` |
| `related` (untyped) | `related` (self-inverse) |

**Rule:** the user asserts the **forward** edge on the subject's note; Kartoteka
auto-writes the **inverse** edge (marked `inverse: true`) on the target's note, exactly as
`set_related` does today — just carrying the mapped predicate instead of a symmetric copy.
`inverse: true` edges are Kartoteka-maintained: `fsck` rebuilds them from the forward
edges, so a hand-edit that desyncs them is self-healing, not a corruption.

Keep the predicate set **closed and documented** (the table above), not free-form — a typo
predicate is a silent broken edge. `fsck` rejects unknown predicates. Adding a predicate is
a one-line table change plus its inverse.

### Why frontmatter and not a `graph.yml`

A central graph file would conflict on every concurrent edit — the exact monolith problem
M1 rejects for `library.yml`. Per-key edges in per-key note files conflict only when the
*same* endpoint is edited two ways, and resolve in `vim`.

---

## 2. Knowledge graph nodes (§9) — a new authoritative file type — **M3**

> **Deferred to M3 — now spec'd in `docs/M3-SPEC.md`.** M2 ships typed relations between
> existing `entries/` only. Nodes are the bigger lift (new file type, target resolver, node
> UI) and the bigger differentiator, so they get their own milestone. The typed-relation
> design in §1 is authored to accept a node slug as a `target` from day one, so M3 slots in
> without a format change.

The brainstorm's Augustine → *City of God* → influenced → Aquinas graph needs first-class
**non-work entities**: people, concepts, schools, events. These are not citation entries
and must not be faked as empty Hayagriva entries.

### Proposed: `nodes/<slug>.md`

Structurally identical to a note (frontmatter + prose), so the graph is homogeneous — a
relation's `target` may be either a citation key or a node slug, and Kartoteka resolves it
against `entries/` first, then `nodes/`.

```markdown
---
node-type: person            # person | concept | school | event | place | work-uncataloged
label: Augustine of Hippo
identifiers:
  wikidata: Q8018
  viaf: "66806872"
  orcid: null
relations:
  - predicate: influenced
    target: aquinas            # another node slug
  - predicate: authored
    target: augustine0426city  # a real entries/ key
---

Bishop of Hippo, 354–430. Anchor node for the Augustinian tradition facet.
```

This is where **author-level identifiers** (ORCID, VIAF, Wikidata — brainstorm §1) live,
because they **cannot** go in `entries/` without breaking the pure-Hayagriva invariant
(Hayagriva's person model has no ORCID field). A person node solves author-IDs and the
knowledge graph in one structure.

**Additional predicates for nodes:** `influenced`/`influenced-by`, `authored`/`authored-by`,
`member-of`/`has-member` (schools), `about`/`discussed-in` (concepts), `part-of`/`has-part`
(events). Same closed-table + inverse discipline as §1.

`nodes/` is **optional and additive** — a library with zero nodes is fully valid; the graph
is a layer users opt into, not a required ontology. This keeps the "just a bib manager if
you want" promise while enabling the "knowledge base" ceiling.

---

## 3. Semantic facets (§10) & nested tags (§5) — convention, not new fields

Discipline, subdiscipline, methodology, framework, tradition (§10) are structured tags, not
distinct data types. Rather than sprout a dozen typed frontmatter fields (schema sprawl,
and every one needs a UI), use a **facet convention over the existing `tags:` list**:

```yaml
tags:
  - discipline:theology
  - subdiscipline:systematic
  - tradition:reformed
  - framework:liberation
  - christology              # plain topical tag, no facet
```

- A tag containing `:` is a **faceted** tag (`facet:value`); Kartoteka can group/filter by
  facet in the UI and index the facet separately, but on disk it is still one flat list —
  no new schema, no migration, hand-editable.
- Nested/hierarchical tags (§5, `theology/liberation`) use `/` within the value, orthogonal
  to facets: `discipline:theology/systematic`.
- The facet vocabulary (which facet names are "known") is a **soft convention** documented
  here, not enforced — unknown facets are allowed (extensibility, §schema-extensibility),
  known ones just get first-class UI. This preserves M1's free-form tags exactly.

**Recommendation:** ship the facet *convention* + UI grouping; do **not** add typed
`discipline:`/`tradition:` frontmatter keys. If a real need for typed, validated facets
emerges later, it can be layered on without a disk-format change.

---

## 4. Small frontmatter additions (§8, §13, §20)

All optional, all in `notes/<key>.md` frontmatter, all `skip_serializing_if`-elided when
empty so bare notes stay bare:

```markdown
---
read-status: reading
progress: { page: 112, of: 420 }      # §8 reading progress
cite:                                  # §13 per-entry citation prefs
  short: "Cone, Black Theology"
  preferred-style: chicago-author-date
tasks:                                 # §20 per-item tasks
  - text: Verify the Barth quotation on p.112
    done: false
    due: 2026-08-15
  - text: Re-read chapter 3
    done: true
---
```

- **`progress`** — coarse position; pairs with `read-status: reading`. Purely informational,
  indexed for "what am I mid-way through."
- **`cite`** — user overrides for how *this* entry cites. `preferred-style` is advisory to
  export; `short` is a hand-authored short form. Citation *history* (§13) is derived UI
  state → `.kartoteka/`, not here.
- **`tasks`** — cheap per-item to-dos. They live with the item so they merge per-key and are
  fixable in `vim`. A global task view is a *derived* aggregation (`.kartoteka/`) over these.

---

## 5. AI-generated metadata (§12) — `ai/<key>.yml`, regenerable sidecar

The hard requirement (§12): **AI output must never overwrite curated fields.** The clean way
to guarantee that structurally is a **separate, namespaced, regenerable file** — not a block
inside `notes/` where a bad model run could clobber hand-written prose.

```yaml
# library/ai/cone1970black.yml
schema: 1
generated-by: claude-opus-4-8         # provenance — so a bad batch is identifiable
generated-at: 2026-07-24T10:00:00Z
source-hash: blake3:9f2b...c4         # which attachment/text this was generated from
summary: >
  Argues that the gospel is intrinsically a message of liberation for the oppressed…
keywords: [black theology, liberation, christology, Barth]
concepts: [theological method, divine solidarity]
claims:
  - The gospel is a gospel of liberation.
counterarguments:
  - Critiqued as under-specifying the eschatological horizon.
```

- **Committed, but never authoritative over curated data.** It is regenerable from
  `source-hash`; deleting `ai/<key>.yml` loses nothing a re-run can't rebuild. Committing it
  (vs. `.kartoteka/`) is a judgment call — recommend committing, because regeneration costs
  tokens and the output is worth versioning, but it carries a provenance header so it is
  clearly machine-origin and safe to `rm` en masse.
- **One-directional merge into curated fields is a user action, never automatic.** A user
  may promote an AI keyword into `tags:`; Kartoteka never does it silently. This is the whole
  reason for the file boundary.
- Indexed for search (summaries/keywords are high-value search text) but flagged as
  AI-origin so results can be filtered.

**Decided:** commit `ai/<key>.yml` (not `.kartoteka/`), for the versioning + token-cost
reasons above, with the provenance header making it clearly machine-origin and safe to
`rm` en masse.

---

## 6. Projects & writing/usage tracking (§21, §22) — declare authoritative, derive usage

Two halves, split by the authoritative/derived line:

**Declare (authoritative) — `projects/<slug>.yml`:** like a collection, but points at the
Typst documents a project comprises, so Kartoteka knows where to look for `@key` usage.

```yaml
# library/projects/dissertation.yml
name: Doctoral Dissertation
documents:
  - ~/Writing/dissertation/main.typ
  - ~/Writing/dissertation/ch3-method.typ
```

**Derive (disposable) — "used in":** the reverse map ("this source is cited in
dissertation ch.3", §22) is the result of **scanning** the declared documents for `@key`
citations. That is a scan result — it churns and is rebuildable — so it lives in
`.kartoteka/usage.*`, **never** written back into `notes/`. The UI shows "Used in:
Dissertation (ch3), Sermon 2026-07-20" from the derived map; git never sees it.

This keeps the feature (which is genuinely differentiating — §22 is a real gap in other
reference managers) without polluting per-key files with volatile scan output.

---

## What explicitly does NOT get a schema

- **§18 version history** — this is git. Do not build v1/v2/v3 metadata; `git log` and
  `git restore` already are it. A UI over `git log <file>` is fine; a parallel versioning
  format is a violation of "if it can't be fixed with git, the design has failed."
- **§14 search index, §17 dedupe scores, §6 saved searches, §13 citation history,
  §22 usage map** — all **derived** → `.kartoteka/`, rebuildable by `reindex`.
- **§19 plugin API** — a code surface, not a data-model question; defer.

---

## Migration & compatibility

- **Purely additive.** Every new field is optional with `skip_serializing_if`, so existing
  vaults parse unchanged and round-trip byte-identical when the new fields are absent.
- **`related` → `relations`:** keep `related: Vec<String>` working (untyped "see also");
  `relations:` is the typed superset. A `kartoteka migrate` can offer to lift bare `related`
  edges into `relations` with `predicate: related`, but it is not forced.
- **`fsck` gains:** unknown-predicate detection, inverse-edge reconciliation, node/key
  target resolution, and dangling `projects/` document paths — all warnings, never crashes,
  consistent with M1's `fsck` philosophy.

---

## Decisions (2026-07-24)

Resolved with Cal; the sections above reflect these:

1. **AI metadata → commit `ai/<key>.yml`** with a provenance header (not `.kartoteka/`).
2. **Graph scope → typed relations in M2, `nodes/` in M3.** M2 upgrades `related` to typed
   predicates between existing entries; the non-work knowledge graph is M3.
3. **Semantic metadata → facet convention over `tags:`** (`discipline:theology/systematic`),
   not typed frontmatter fields. Ship the UI grouping; no disk-format change.
4. **Predicate vocabulary → start with the general set** (§1 table + inverses). Add
   theology-specific predicates later only as concrete needs surface — don't bake an ontology
   in up front.

### M2 build order (implied by the above)

1. `relations:` typed block in `NoteFrontmatter` + closed predicate/inverse table +
   auto-materialized inverse edges (extends existing `set_related`).
2. Facet-aware tag parsing + UI grouping (no schema change).
3. Small frontmatter fields: `progress`, `cite`, `tasks` — all optional, elided when empty.
4. `ai/<key>.yml` sidecar: schema, provenance header, indexing (flagged AI-origin), the
   never-auto-merge boundary, and a "promote to tags/notes" user action.
5. `projects/<slug>.yml` declaration + derived `.kartoteka/usage.*` scan for "used in".
6. `fsck` additions: unknown-predicate detection, inverse-edge reconciliation, dangling
   `projects/` paths.

### Deferred to M3

- `nodes/<slug>.md` non-work knowledge graph (people, concepts, schools, events) — and with
  it, author-level identifiers (ORCID/VIAF/Wikidata), which live on person nodes because they
  can't go in pure-Hayagriva entries.
