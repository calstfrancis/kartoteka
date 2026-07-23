# Kartoteka — Licence Audit

Status: **planning draft, awaiting approval.**

Kartoteka ships as a **proprietary paid binary** (Windows especially). Every dependency
compiled into `kartoteka-cli`/`-ui` must permit proprietary, closed-source distribution.
This document lists every proposed dependency, its licence, and whether it permits that.
Anything ambiguous is flagged rather than assumed.

**Project licence split** (per the architecture decision): the shared `fond-*` crates are
**MIT** (so Zerkalo/Skrizhal can consume them); the `kartoteka-*` app crates are
**proprietary/all-rights-reserved**. The current repo-root `LICENSE` is MIT — it should be
replaced with a proprietary licence for the app, with the MIT text retained for the
`crates/fond-*` members (recommended: per-crate `LICENSE` files, or a `license` field in
each `Cargo.toml`). **This relicensing is a required pre-implementation step and is
flagged for Cal's confirmation.**

Legend: ✅ permits proprietary distribution · ⚠️ permits it but with a condition to honour
· ⛔ incompatible, do not use.

---

## Rust crate dependencies

| Crate | Version (verify at add) | Licence | Proprietary OK? | Notes |
|---|---|---|---|---|
| `hayagriva` | 0.9.1 | MIT OR Apache-2.0 | ✅ | Core data model + CSL rendering. Zerkalo already uses 0.9. |
| `biblatex` | 0.11 | MIT OR Apache-2.0 | ✅ | BibTeX side of import (M2). |
| `pdfium-render` | 0.8.37 | MIT OR Apache-2.0 | ✅ | Rust wrapper only; the PDFium **binary** is licensed separately — see below. |
| `tantivy` | 0.26.1 | MIT | ✅ | Full-text index. |
| `rusqlite` (feature `bundled`) | 0.3x (verify) | MIT | ✅ | `bundled` compiles SQLite, which is **public domain**. Derived cache only. |
| `git2` | 0.20 | MIT OR Apache-2.0 | ⚠️ | Rust bindings are MIT/Apache; **vendored libgit2 is GPLv2 _with a linking exception_** that explicitly permits linking into proprietary software. Acceptable, but see the flag below — use the `vendored-libgit2` feature and do **not** enable a GPL SSH/TLS backend. |
| `notify` | 6 | CC0-1.0 (public domain) | ✅ | Filesystem watching for the Zerkalo live seam. |
| `reqwest` | 0.12 (verify) | MIT OR Apache-2.0 | ⚠️ | ✅ *only if* built with `rustls-tls`, not `native-tls`/OpenSSL — see flag. Blocking client, M5. |
| `serde` / `serde_yaml_ng` | serde 1, serde_yaml_ng 0.10 | MIT OR Apache-2.0 | ✅ | `serde_yaml_ng` chosen over unmaintained `serde_yaml`; matches Skrizhal. |
| `serde_json` | 1 | MIT OR Apache-2.0 | ✅ | Annotation sidecars. |
| `blake3` | 1 | CC0-1.0 OR Apache-2.0 (OR Apache-2.0-w/-LLVM-exception) | ✅ | Attachment content hashing. |
| `thiserror` | 2 | MIT OR Apache-2.0 | ✅ | Crate-specific error enums across the UI-agnostic seam. |
| `directories` | 5 (verify) | MIT OR Apache-2.0 | ✅ | Cross-platform config/data dirs (Linux + Windows). |
| `crossbeam-channel` | 0.5 | MIT OR Apache-2.0 | ✅ | Worker-thread channels (indexing). |
| `ulid` | 1 | MIT | ✅ | Stable annotation IDs. |
| `clap` | 4 | MIT OR Apache-2.0 | ✅ | CLI parsing (`kartoteka-cli`). |
| `walkdir` | 2 | MIT OR Apache-2.0 | ✅ | Directory scans for reindex/fsck. |

Versions above are the current-latest at planning time and must be re-checked with
`cargo add`/`cargo tree` when actually added — the brief is explicit that these numbers are
not to be trusted blindly.

---

## Native / bundled binaries

| Component | Licence | Proprietary OK? | Notes |
|---|---|---|---|
| **PDFium** (the C++ library behind `pdfium-render`) | BSD-3-Clause | ✅ | Brief explicitly approves PDFium. Distributed as a prebuilt dynamic library per platform (`.so`/`.dll`). BSD-3 requires the copyright notice be reproduced in distribution docs — trivially satisfied. |
| **libgit2** (vendored by `git2`) | GPLv2 **with linking exception** | ⚠️ | The linking exception exists precisely to allow linking into non-GPL/proprietary programs. Compliant as long as we link (not fork-and-modify-and-hide) and do not pull in a GPL-only transitive backend. |
| **SQLite** (via `rusqlite` `bundled`) | Public domain | ✅ | No conditions. |

---

## CSL styles (the one real ambiguity — flagged)

`hayagriva` renders citations using **CSL style files** from the
`citation-style-language/styles` repository. The two required styles both exist there:

- `society-of-biblical-literature-fullnote-bibliography` (SBL — built on Chicago notes)
- `chicago-notes-bibliography`

**Licence:** individual CSL style files are **CC BY-SA 3.0**. This is the item to get
right for a proprietary product:

- CC BY-SA's copyleft attaches to the **style file and its derivatives**, *not* to the
  application that reads it. Bundling the styles **verbatim** and shipping proprietary code
  that consumes them does **not** force the app open.
- Conditions to honour: (1) **attribution** — credit the CSL project and the individual
  style authors; (2) **share-alike** — keep the style files themselves under CC BY-SA and
  provide the (unmodified) source of any style we distribute.

**Recommendation:** ship the required `.csl` files as **separate, clearly CC-BY-SA-licensed
data assets** (a `styles/` resource dir with an accompanying `LICENSES-STYLES.md`
attribution/notice), *not* compiled or minified into proprietary source. Prefer bundling
only the styles actually shipped (SBL + Chicago-notes and their dependencies) rather than
the whole 2600-style corpus, to keep the attribution surface small and auditable. If Cal
wants zero third-party-copyleft assets in the product at all, the alternative is to author
or commission the two styles from scratch — noted, but almost certainly not worth it.

This is the single dependency I would not ship without an explicit sign-off, so it is
called out here rather than buried.

---

## Explicitly excluded (do not reach for these)

| Component | Licence | Why excluded |
|---|---|---|
| **MuPDF** | AGPL-3.0 / commercial | ⛔ AGPL is incompatible with proprietary closed distribution; commercial licence is a paid Artifex agreement we are not entering. Brief forbids it. |
| **Poppler** | GPL-2.0 / GPL-3.0 | ⛔ GPL forces the whole app open. Brief forbids it. |
| **git CLI (shelling out)** | — | Not a licence issue but a portability one: not assumed present on Windows. Use vendored libgit2. |
| `serde_yaml` | MIT/Apache | Not a licence issue — **unmaintained**. Replaced by `serde_yaml_ng`. |

---

## Open flags for Cal

1. **Relicense the app crates** from the current MIT `LICENSE` to proprietary, keeping
   `fond-*` MIT. Required before implementation; confirm.
2. **CSL styles are CC BY-SA 3.0.** Confirm the "bundle verbatim + attribution + keep the
   style files themselves CC-BY-SA" approach is acceptable for the paid product.
3. **libgit2 GPLv2-with-linking-exception** — acceptable and standard, but noting it so the
   proprietary-distribution decision is made with eyes open. Do not enable any GPL-only
   `git2` backend feature.
4. **`reqwest` must be pinned to `rustls-tls`** (no OpenSSL/native-tls) — both to avoid
   OpenSSL's licensing/build friction and to keep the Windows build self-contained. Applies
   at M5, listed now so it is not forgotten.
