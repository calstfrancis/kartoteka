# Kartoteka — Architecture

Status: **planning draft, awaiting approval.** No code written yet.

Kartoteka (Картотека, "card catalogue") is a reference manager and PDF library — a
Zotero replacement for a solo academic writing theology in Typst. It is part of the
**Fond** suite (Zerkalo, Skrizhal, …). This document defines the crate boundaries, the
one rule everything else follows, the Zerkalo/Skrizhal integration seam, and the
Linux/Windows story.

---

## 1. The one rule: files are authoritative, everything else is disposable

There is exactly one source of truth: **the plain files on disk in the library repo.**
Hayagriva YAML entries, Markdown notes, JSON annotation sidecars, YAML collections. All
human-readable, all git-versioned, all editable in `vim`.

Everything else — the tantivy index, the SQLite metadata cache, `library.yml` — is
**derived state**. It is rebuildable from the files by a single command
(`kartoteka reindex`) and may be deleted at any time with zero data loss:

```
rm -rf library/.kartoteka/     # nuke all derived state
kartoteka reindex              # fully reconstructed from entries/ notes/ annots/ collections/
```

This rule is load-bearing for the whole design. Any time a component wants to store
something, the question is: *can this be regenerated from the files?* If yes, it goes in
`.kartoteka/` and is gitignored. If no — if it is user intent that cannot be
recomputed — it goes in a tracked file. There is no third category.

`library.yml` is a special derived file: it is committed (see §6) but still fully
regenerable, so it never counts as authoritative. If it disagrees with `entries/`,
`entries/` wins and `library.yml` is rewritten.

---

## 2. Workspace and crate layout

A single Cargo workspace (Rust 2021). The `fond-*` crates are **shared library crates**;
the `kartoteka-*` crates are the application. Per the license decision (see
`LICENSES.md`), the split is deliberate: `fond-*` are MIT so Zerkalo and Skrizhal can
depend on them; `kartoteka-*` are proprietary, because Kartoteka is the paid product.

```
kartoteka/                     (cargo workspace, this repo)
├── crates/
│   ├── fond-bib/              MIT  — Hayagriva model, on-disk layout, keys, CSL rendering
│   ├── fond-vault/            MIT  — git-backed repo ops (status/stage/commit/pull/push)
│   ├── fond-doc/              MIT  — PDF render, text extraction, outline, annotation model
│   └── fond-index/            MIT  — tantivy full-text + sqlite derived cache
├── kartoteka-cli/             prop — headless binary; the real product surface, M1–M3
├── kartoteka-ui-gtk/          prop — Linux GTK4/libadwaita shell   (deferred, not this session)
└── kartoteka-ui-tauri/        prop — Windows Tauri v2 shell        (deferred, not this session)
```

**Why the `fond-*` crates live here for now.** The three consumers (Kartoteka, Zerkalo,
Skrizhal) already exist, but the shared crates do not — they are born and iterated
through Milestones 1–3 while their public APIs are still molten. Extracting them to a
standalone `fond` repo now would buy cross-repo version friction before there is a stable
API to justify it, and Zerkalo/Skrizhal adoption is explicitly deferred. Because their
public APIs are designed UI-agnostic and consumer-neutral from day one (§4), later
extraction to a standalone `fond` repo is a mechanical `git mv` + dependency-path change,
not a redesign. Zerkalo/Skrizhal will consume them as a git dependency on this repo when
they adopt them.

### Crate responsibilities

| Crate | Owns | Never contains |
|---|---|---|
| `fond-bib` | Hayagriva `Entry` model, the `entries/`/`notes/`/`collections/` layout, citation-key generation + collision, `library.yml` regeneration, CSL rendering (SBL, Chicago-notes) | GTK, Tauri, PDF, tantivy, network |
| `fond-vault` | libgit2 (via `git2`) repo lifecycle: init, status, stage, commit, pull/push, conflict surfacing; `notify` file-watching | Bibliographic semantics, UI |
| `fond-doc` | PDFium rendering, text-layer extraction, outline, the annotation model (sidecar schema + read/write) | Search indexing, git, UI |
| `fond-index` | tantivy schema + queries, the `sqlite` derived cache, `reindex` (rebuild-from-scratch) | Authoritative data — it may only ever *cache* what the files already say |
| `kartoteka-cli` | Argument parsing, orchestration of the four crates, human/JSON output, the `kartoteka` command surface | Reusable library logic (that belongs in a `fond-*` crate) |

`kartoteka-cli` is where the crates are composed; it is intentionally thin. If logic in
the CLI would be useful to Zerkalo, it is in the wrong place and belongs in a `fond-*`
crate.

---

## 3. Threading and IO — blocking by default

This is a local-first, single-user app. **No async runtime.** Threads and blocking IO
throughout. The specific places concurrency is justified, and nowhere else:

- **Indexing** (`fond-index`): PDF text extraction and tantivy writes run on a worker
  thread pool so a large reindex does not block the caller. `std::thread` +
  `crossbeam`/`std::sync::mpsc`, not `tokio`.
- **File-watching** (`fond-vault`): `notify` runs its own watcher thread and delivers
  change events over a channel. This is the mechanism Zerkalo uses to see library changes
  live (§5).
- **Network** (`fond-bib`/acquisition, M5): DOI/ISBN/arXiv lookups. `reqwest`'s
  **blocking** client on a worker thread, not the async client. One request at a time is
  fine for a human dropping in a PDF.

If a future feature needs real concurrency it must name the specific reason, per the
brief. The default answer is a blocking call on a thread.

---

## 4. UI-framework-agnostic public APIs (the seam that must not break)

`fond-bib`, `fond-vault`, `fond-doc`, and `fond-index` expose **no GTK types and no Tauri
types** in their public APIs — no `glib`, `gtk`, `gio`, `tauri`, or `serde_json::Value`
leaking as a lingua franca. Public types are plain Rust: owned structs, `enum`s,
`PathBuf`, `Result<T, FondError>`. Errors are concrete crate-specific error enums
(`thiserror`), never `Box<dyn Error>` across the seam.

This is what lets the same core drive **three** frontends:

- `kartoteka-cli` — headless, the product surface for M1–M3.
- `kartoteka-ui-gtk` — Linux, GTK4/libadwaita, matching the rest of Fond.
- `kartoteka-ui-tauri` — Windows, Tauri v2 (GTK4-on-Windows is not a serious option for a
  paid product).

…and to let **Zerkalo get its own Tauri port eventually** without the shared crates
having to change. A frontend is a thin translation layer: core types in, widgets out.

No wrapper traits with a single implementation (per the brief's annoyances). We take
`git2`, `pdfium-render`, and `tantivy` types where they are already good abstractions;
we wrap only where we genuinely need to hide a backing choice (e.g. the derived cache
being SQLite is an implementation detail of `fond-index` and is not exposed).

---

## 5. Zerkalo / Skrizhal integration — two couplings, filesystem-only

The entire integration surface is **the filesystem plus a shared crate.** No daemon, no
socket, no IPC protocol.

**Loose coupling (works today, for any tool).** Anything — Zerkalo, a Typst build, a
one-off script — can point at `library.yml` as a flat Hayagriva bibliography file,
exactly as it points at a BetterBibTeX `.bib` today:

```typst
#bibliography("…/library/library.yml")
```

Because Kartoteka regenerates `library.yml` on every write (§6), there is no export step
to remember and no staleness window (unlike BetterBibTeX's timed `.bib` export).

**Tight coupling (Zerkalo depends on `fond-bib`/`fond-vault` directly).** Zerkalo reads
structured `Entry` objects from the same vault directory via `fond-bib`, and watches the
directory with `fond-vault`'s `notify` wrapper. The moment Kartoteka writes a new entry,
Zerkalo's citation-key autocomplete sees it — against real keys, not a stale cache. The
two apps never talk to each other; they both talk to the files.

**Skrizhal** (Hayagriva-format CV builder) shares `fond-vault` for its git-backed
storage and can share `fond-bib`'s Hayagriva model. It already uses `serde_yaml_ng`,
which fixes our YAML library choice for consistency (see `DATA-MODEL.md` §YAML).

Adoption by Zerkalo/Skrizhal is **deferred** — this project designs the crates so that
adoption is possible and does not refactor those apps now.

---

## 6. `library.yml` — committed, deterministically regenerated

**Decision: `library.yml` is committed to the library repo** (not gitignored), and is
regenerated on every write with a stable, deterministic ordering.

Rationale:

- It is the **loose-coupling contract**. A fresh clone (before the tantivy index or
  SQLite cache is built, before any Kartoteka command has run) must be immediately usable
  by `#bibliography("library.yml")` and by any external tool. A gitignored `library.yml`
  would be absent on clone until Kartoteka first regenerates it — breaking the "no export
  step to remember" promise for anyone who has not run Kartoteka yet.
- The churn objection (a generated file in git) is neutralised by **deterministic
  emission**: entries are concatenated in a fixed order (citation key, ascending), fields
  in Hayagriva's canonical field order, stable formatting. Editing one entry changes only
  that entry's block in the `library.yml` diff. It is still derived — `kartoteka reindex`
  rewrites it byte-for-byte — so it never becomes authoritative; `entries/` always wins.

The alternative (gitignore it, regenerate lazily) was rejected because it trades a small,
well-behaved diff for a broken first-clone experience against the single most important
external contract the app has.

---

## 7. Attachments — out of git, first-class "missing" state

PDFs never go in git (binary, large, undiffable; git-lfs needs a server we will not run
for an offline paid product). See `DATA-MODEL.md` for the on-disk detail. Architecturally:

- The **entry's file record** carries the attachment's `blake3` content hash, original
  filename, byte size, and page count — enough to describe the blob without the blob.
- Blobs live in `attachments/`, named by hash, gitignored, synced out-of-band
  (rclone/pCloud).
- **"Attachment present" vs "attachment missing" is a first-class state**, surfaced in
  CLI and (later) UI — never an error. A library cloned before the blobs land opens,
  searches metadata + notes, and builds bibliographies normally.
- `kartoteka fsck` reports missing, orphaned, and hash-mismatched blobs.

All git operations are **in-process via `git2`/libgit2 (vendored)**. We never shell out
to `git` — it will not be installed on the Windows machines.

---

## 8. Linux / Windows

Linux is delivered first; Windows follows once the Linux product is complete. Both are
thin shells over the identical headless crates.

| Concern | Linux | Windows |
|---|---|---|
| Frontend | GTK4/libadwaita (`kartoteka-ui-gtk`) | Tauri v2 (`kartoteka-ui-tauri`) |
| PDF engine | PDFium via `pdfium-render` (dynamic lib bundled) | PDFium via `pdfium-render` (bundled DLL) |
| git | vendored libgit2 (`git2`) | vendored libgit2 — **no system `git` assumed** |
| Packaging | flatpak (Fond convention) | signed installer, proprietary |
| Config / data | `~/.config/kartoteka`, `~/.local/share/kartoteka` | `%APPDATA%`/`%LOCALAPPDATA%` via `directories` crate |

Cross-platform path and dir handling goes through the `directories` crate rather than
hand-rolled `$HOME` logic, so the same code resolves correctly on both. The headless
crates and `kartoteka-cli` are validated on both OSes in CI before either UI is built.

---

## 9. What this session does and does not build

**This session, after approval:** scaffold the workspace and implement **Milestone 1
only** (the vault — see `ROADMAP.md`). No UI. No network. No PDFs. No dependency added
that is not in the approved plan.

**Explicitly deferred:** both UI crates, PDF handling (`fond-doc` beyond a stub),
acquisition/network, Zotero import. They appear in the layout so the boundaries are
fixed, but carry no implementation this session.
