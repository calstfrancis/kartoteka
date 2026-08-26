# Kartoteka — Data Model

Status: **planning draft, awaiting approval.**

This document specifies the exact on-disk file formats with worked examples, the
citation-key rules, the annotation sidecar schema, and how collisions and git conflicts
are handled. The governing rule (`ARCHITECTURE.md` §1): the files here are authoritative;
`.kartoteka/` is disposable.

---

## On-disk layout

```
library/
  entries/
    berdyaev1937destiny.yml       # ONE Hayagriva key per file — pure Hayagriva
    cone1970black.yml
  notes/
    berdyaev1937destiny.md        # annotation prose + frontmatter: tags, read status,
                                  # rating, date-added. Optional, one per key.
  annots/
    berdyaev1937destiny.json      # PDF highlights & marginalia sidecar. Optional.
  collections/
    liberation-theology.yml       # ordered list of keys + optional per-collection notes
  library.yml                     # GENERATED, committed: concatenation of entries/ (Typst)
  attachments/                    # gitignored — PDF blobs, named by blake3 hash
  .gitignore
  .kartoteka/                     # gitignored — all derived state
    index/                        # tantivy
    cache.sqlite                  # derived metadata cache
```

Filename == citation key throughout (`entries/<key>.yml`, `notes/<key>.md`,
`annots/<key>.json`). One key per file. This is what makes git merges survivable: a
single `library.yml`-style monolith would conflict on every concurrent edit; per-key
files conflict only when the *same* entry is edited two ways.

### `library/.gitignore` (committed)

```gitignore
attachments/
.kartoteka/
```

`library.yml` is deliberately **not** ignored (`ARCHITECTURE.md` §6).

---

## `entries/<key>.yml` — pure Hayagriva

Each file is a single-key Hayagriva mapping and **nothing but valid Hayagriva fields**.
No `tags:`, no `rating:`, no Kartoteka-private keys. Worked example:

```yaml
berdyaev1937destiny:
  type: book
  title: The Destiny of Man
  author: Berdyaev, Nikolai
  date: 1937
  publisher:
    name: Geoffrey Bles
    location: London
  translator: Duddington, Natalie
  language: en
  serial-number:
    isbn: "9780060102845"
```

An article with a parent (Hayagriva's parent mechanism, which avoids field duplication):

```yaml
cone1970black:
  type: article
  title: Black Theology and Black Power
  author: Cone, James H.
  date: 1970
  page-range: 6-13
  parent:
    type: periodical
    title: Christianity and Crisis
    volume: 30
    issue: 1
```

**Why entries stay pure Hayagriva.** Hayagriva 0.9.1 does not use
`#[serde(deny_unknown_fields)]`, so it would in fact *silently ignore* extra keys — a
`tags:` field in an entry would not currently break Typst's `#bibliography`. We do **not**
rely on that. Tags/workflow metadata live in `notes/` (next section) because:

1. `library.yml` is a verbatim concatenation of `entries/`; keeping entries pure
   guarantees the generated file is clean Hayagriva forever, independent of whether a
   future Hayagriva release tightens to `deny_unknown_fields` or adds a colliding field.
2. Authoritative bibliographic data and personal workflow state are genuinely different
   things with different lifetimes; mixing them makes both uglier to hand-edit.
3. It keeps the "fix my library in `vim`" promise honest — an entry file is exactly what
   a citation is, with no app cruft.

### Serialization / YAML library

`serde_yaml` is unmaintained. We use **`serde_yaml_ng`** — actively maintained, a
near drop-in for `serde_yaml`, and **already the choice in Skrizhal** (`0.10`), so this
keeps the shared `fond-bib`/`fond-vault` crates consistent with an existing Fond consumer.

`saphyr` was considered (lower-level YAML AST, can preserve comments/formatting) and
rejected for the entry files: Kartoteka *owns and rewrites* `entries/` and `library.yml`,
so a normalizing serde round-trip is correct, not lossy. The round-trip test we hold
ourselves to is "YAML in → model → YAML out, **byte-identical modulo formatting**" (per
the brief), which serde satisfies. If we later need comment-preserving edits of
hand-authored files, `saphyr` can be reconsidered narrowly there without touching the
model.

Hayagriva itself provides `Entry` (de)serialization; `fond-bib` wraps its
`from_yaml_str` / `to_yaml_str` and adds the one-key-per-file layout on top.

---

## `notes/<key>.md` — prose + workflow frontmatter

Optional, one per key. Markdown body is free annotation prose. YAML frontmatter owns
everything Hayagriva has no field for:

```markdown
---
tags: [liberation-theology, christology, 20th-century]
read-status: read          # unread | reading | read
rating: 4                  # 1–5, optional
date-added: 2026-07-23
date-read: 2026-08-01      # optional
---

Cone's pivot from Barth toward an explicitly Black christology. Compare with the
later *God of the Oppressed* — the epistemology hardens. Useful for the chapter on
theological method.
```

Frontmatter schema (all fields optional except none is required — a bare note is valid):

| Field | Type | Values |
|---|---|---|
| `tags` | list of strings | free-form, kebab-case by convention |
| `read-status` | enum | `unread` \| `reading` \| `read` |
| `rating` | int 1–5 | optional |
| `date-added` | ISO date | set by Kartoteka on first add if absent |
| `date-read` | ISO date | optional |

Tags, read-status, and rating are **indexed** (`fond-index`) so search can scope on them
(§Search in `ARCHITECTURE`/`ROADMAP`), but the note file is authoritative — the index is
rebuilt from it.

---

## `collections/<slug>.yml` — ordered key lists

```yaml
name: Liberation Theology
description: Sources for the dissertation chapter on liberation hermeneutics.
parent: theology
keys:
  - cone1970black
  - berdyaev1937destiny
  - gutierrez1971teologia
```

`keys` is **ordered** and may repeat a key across collections. A key listed here that has
no `entries/<key>.yml` is a dangling reference — reported by `kartoteka fsck`, never a
hard crash. Collections are the unit `kartoteka bib <collection>` and
`kartoteka annotated-bib <collection>` operate on (M3).

`parent` (optional) is another collection's slug, making this one its child in the sidebar's
tree — omitted for a top-level collection, which is how every collection worked before
nesting existed. A cycle (a collection listed as its own ancestor) is invalid and the UI
refuses to create one, but the field itself carries no built-in cycle protection at the
storage layer — a hand-edited file forming a cycle is a `kartoteka fsck` concern, not a
parse error.

---

## `library.yml` — generated, committed

A single Hayagriva document that is the concatenation of every `entries/<key>.yml`, in
**ascending citation-key order**, each entry's fields in Hayagriva's canonical field
order. Regenerated on every write and by `kartoteka reindex`. It is committed
(`ARCHITECTURE.md` §6) but never authoritative: if it disagrees with `entries/`,
`entries/` wins and it is rewritten. Deterministic emission keeps its git diff minimal —
editing one entry touches only that entry's block.

---

## Citation keys

### Scheme

`<lastname><year><titleword>`, all lowercase ASCII:

- **lastname** — first author's family name, transliterated to ASCII, diacritics folded,
  non-alphanumerics stripped (`Berdyaev, Nikolai` → `berdyaev`; `Gutiérrez` →
  `gutierrez`). Corporate author → the org name, same folding. No author → first
  significant title word takes this slot.
- **year** — 4-digit year from `date`. No date → `nodate`.
- **titleword** — first "significant" word of the title (skipping a leading article
  `a`/`an`/`the` and their common non-English equivalents), lowercased, ASCII-folded,
  alphanumerics only (`The Destiny of Man` → `destiny`; `Black Theology and Black Power`
  → `black`).

Result: `berdyaev1937destiny`, `cone1970black`. Matches the brief's examples.

### Collision handling

Two entries can generate the same key (same author, year, and first title word). On
collision, a lowercase-letter suffix is appended in insertion order: `smith2020faith`,
then `smith2020faitha`? — no. To keep keys readable and stable we suffix **both from the
first collision onward is avoided**; instead:

- The **first** entry to claim a key keeps the bare key (`smith2020faith`).
- Each subsequent colliding entry gets the next free suffix `b`, `c`, … appended:
  `smith2020faithb`, `smith2020faithc`. (We start at `b` so a suffixed key is visually
  distinct and never collides with a hypothetical future bare `…a`.)

Keys are **stable once assigned** — the assigned key is written into a Kartoteka-managed
record so re-import or metadata edits never silently renumber an existing key (which would
break every `@key` reference in the user's Typst documents). Assignment is checked against
the set of *all* existing keys across `entries/` (the filename is the authority), so
collision detection needs no database — it is a directory listing.

Manual override: a user may rename `entries/<key>.yml` (and the matching `notes/`,
`annots/` files) in `vim`; Kartoteka treats the filename as the key and never fights it.
`kartoteka fsck` flags a file whose inner mapping key disagrees with its filename.

---

## Attachments

The blob itself is out of git. The entry's **attachment record** describes it. Because
Hayagriva entries stay pure (§entries), the attachment record is **not** stored in
`entries/<key>.yml`. It lives in the notes frontmatter's companion — a dedicated
`attachments` block in the note file, keeping `entries/` clean while remaining tracked and
hand-editable:

```markdown
---
tags: [christology]
read-status: read
date-added: 2026-07-23
attachments:
  - hash: blake3:9f2b...c4        # content address; also the filename in attachments/
    filename: Cone-BlackTheology-1970.pdf
    bytes: 4823117
    pages: 214
---
```

- **`hash`** — `blake3` of the file bytes, `blake3:<hex>`. Doubles as the on-disk name:
  `attachments/9f2b...c4.pdf`.
- Multiple attachments per entry are allowed (list).
- **Present vs missing is first-class.** With the record but no blob in `attachments/`,
  the entry is fully usable (metadata, notes, bibliography) and simply marked
  attachment-missing. `kartoteka fsck` reports:
  - **missing** — record exists, blob absent.
  - **orphaned** — blob in `attachments/` with no record referencing its hash.
  - **hash-mismatch** — blob present but its bytes do not hash to its name/record.

(If, on implementation, threading the attachment record through the note file proves
awkward for entries that have an attachment but no note, the fallback is a dedicated
gitignored-free `attachments/<key>.yml` sidecar. The invariant that matters and will not
change: **the record is tracked, hand-editable, and never inside the pure Hayagriva
entry file.** This is called out for review.)

---

## Annotation sidecar — `annots/<key>.json`

PDF highlights and marginalia. **Must survive the PDF being re-downloaded from a
different source**, so anchoring is on page + quadpoints + a text snippet for fuzzy
re-anchoring — never on internal PDF object IDs (which differ between copies of the "same"
paper).

```json
{
  "schema": 1,
  "key": "cone1970black",
  "pdf_hash": "blake3:9f2b...c4",
  "annotations": [
    {
      "id": "01J8Z…",
      "kind": "highlight",
      "page": 7,
      "quadpoints": [[72.0, 640.1, 523.4, 640.1, 72.0, 628.7, 523.4, 628.7]],
      "snippet": "the gospel of Jesus is a gospel of liberation",
      "snippet_prefix": "…what I mean is that ",
      "snippet_suffix": " for the oppressed of the land.",
      "color": "#f6c344",
      "note": "core thesis statement",
      "created": "2026-07-23T14:11:02Z",
      "modified": "2026-07-23T14:11:02Z"
    }
  ]
}
```

Field notes:

- **`pdf_hash`** — which blob these annotations were authored against. If the present
  blob's hash differs, Kartoteka knows to re-anchor by text rather than trust coordinates
  blindly.
- **`page`** — 0-based or 1-based fixed at implementation (documented in the schema; the
  `schema` version guards it). Page number alone is a coarse anchor; the snippet is the
  fine one.
- **`quadpoints`** — standard PDF highlight quads (arrays of 8 floats per rect), in PDF
  user space. Primary anchor when the blob matches.
- **`snippet` + `snippet_prefix`/`snippet_suffix`** — the highlighted text plus a short
  window of surrounding text. This is the **re-anchoring key**: on a different copy of the
  PDF (re-flowed, re-OCR'd, differently produced), Kartoteka fuzzy-matches the snippet in
  context to relocate the highlight even when quadpoints have shifted. This is why we
  never anchor on PDF object IDs.
- **`kind`** — `highlight` | `underline` | `strikeout` | `note` (freestanding
  marginalia). Extensible; guarded by `schema`.
- **`id`** — app-generated stable id (ULID-style) so an annotation survives edits and can
  be referenced.
- JSON, not YAML, because annotation files are machine-written far more than
  hand-edited and can grow large; JSON keeps them compact and unambiguous. They remain
  readable and git-diffable (one annotation per object, stable key order).

This sidecar is authoritative for annotations. Import/export to *embedded* PDF
annotations (M4) round-trips through this schema; the sidecar is never derived from the
PDF (the PDF is the disposable copy, the sidecar is the record).

---

## Git conflict handling

Per-key files mean conflicts are rare and local. When two machines edit the *same* key:

- **`entries/<key>.yml` / `notes/<key>.md`** — a normal git text conflict on a small,
  human-readable file. `fond-vault` surfaces the conflicted paths; the user resolves in
  `vim` (or, later, a UI merge view). Nothing is auto-resolved silently.
- **`library.yml`** — never hand-resolved. On conflict, take either side and run
  `kartoteka reindex`; it is regenerated deterministically from the now-merged
  `entries/`. `fond-vault` can auto-resolve `library.yml` conflicts by regeneration since
  it is derived.
- **`annots/<key>.json`** — conflicts resolved per-annotation by `id`: union the
  annotation lists, and for a genuinely conflicting edit to the same `id`, keep both and
  flag (annotations are cheap; losing one silently is not acceptable).
- **`.kartoteka/`** — never in git, never conflicts.

The design goal from the brief holds throughout: **if it cannot be fixed with `vim` and
`git`, the design has failed.** Every authoritative file above is small, text (or
readable JSON), and per-key.
