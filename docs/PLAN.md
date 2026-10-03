# Kartoteka — plan: what's left from the UX review

Date: 2026-10-03. Companion to `UX-REVIEW.md` (the findings) and `CHANGELOG.md` `[Unreleased]`
(what's already done on this branch). Ground rules from the review: **citation keys stay
visible**; simplicity first; plain words; nothing hidden that a Typst writer needs.

Sizes: **S** ≈ a day, **M** ≈ a few days, **L** ≈ a week or more.

## Done on this branch (for reference)

`#cite(<key>)` scanner fix · editable journal/volume/issue/pages/URL · key shown under the title
with copy · export without collections + every CSL style (`kartoteka styles`, `--all`) ·
GitHub-backup disclosure · EPUB/folder drop · Cite with a page (`fond_bib::cite`) · **Add box**
(`fond_bib::identify`, `acquire::search_works`, duplicate check, `app_window/add_box.rs`).

## 0. Finish the Add box (S)

- [ ] **Verify the network paths on a real connection.** Crossref and OpenLibrary search,
      and DOI/arXiv/ISBN fetch, could not be reached from the dev sandbox; only parsers and
      classification are tested. Try: a DOI, an arXiv URL, an ISBN, a title like "Black Theology
      and Black Power", a publisher URL.
- [x] Multi-line BibTeX paste into the single-line field — verified in the running app (record
      added, library reloaded, new entry selected). Only `%` comment lines remain untested.
- [ ] After adding by DOI, offer "Find a PDF" (Unpaywall helper already exists:
      `find_pdf_unpaywall`).
- [ ] Remove menu rows now covered by the box (Add PDF / EPUB / folder / New item stay reachable
      from the box and the empty-library page) once the box has had real use.
- [ ] A quick-add accelerator (e.g. Ctrl+Shift+A); add to `SHORTCUT_GROUPS`.
- [ ] Re-fetch metadata for an existing entry ("Update from DOI") — reuses `fetch_doi_bibtex`.

## 1. First-run and layout (S–M)

- [x] **Detail pane too narrow at the default window** — creator row slimmed (extras behind a
      "⋮" menu) and the default split widened; verified at 1300px.
- [ ] First-run flow that ends with a first source: after "New library", land in the Add box.
- [x] Copy `@key` from a spreadsheet row (right-click → "Copy citation (@key)"). Still
      possible: click-to-copy on the key cell itself.
- [ ] Rename jargon in the UI: "Nodes…" → "People & ideas…", "Reindex search" → "Repair search",
      "Back up (git commit)…" → hidden under Advanced.

## 2. Menu and backup consolidation (M)

- [ ] Hamburger from ~35 rows to ~8: Add, Cite, Collections, Backup, Settings; Nodes, Relations
      map, Custom fields, Tasks, Columns, Reindex, Rename attachments behind an **Advanced**
      switch in Settings.
- [ ] One **Backup** dialog replacing six entries (Save a copy, Set up backup, git commit, GitHub
      sign-in, WebDAV, Automatic backups).
- [ ] Status-bar backup state: "Backed up 4 min ago · PDFs not included".
- [ ] Real fix for PDFs not in GitHub backups: offer a second destination for `attachments/`
      (WebDAV already includes them) or Git LFS; at minimum a one-click "Back up files too".

## 3. Search (M)

- [ ] Half-typed words and accent-insensitive across *all* fields, not just the title/author
      fallback (Zerkalo already does this).
- [ ] Show where a hit was found: "PDF p. 14: …context…", note, annotation.
- [ ] Debounce the live filter and stop rebuilding the detail pane on every keystroke.

## 4. Switching from Zotero / Mendeley (M–L)

- [ ] **One-click Zotero import**: detect `~/Zotero`, import items, collections, notes and files
      with progress. Today needs a BetterBibTeX export first.
- [ ] RIS and CSL-JSON import (a `.ris` drop currently says it can't be read).
- [ ] Mendeley export path documented.

## 5. Zerkalo integration (M) — mostly in the Zerkalo repo

- [ ] **Locators in Zerkalo's `@` popup**, using `fond_bib::cite::typst_citation` so the text
      matches Kartoteka's Cite box exactly. (Zerkalo pins `fond-bib v0.9.0`; needs a bump.)
- [ ] **Vault discovery:** a small shared pointer (e.g. `~/.local/share/fond/libraries.json`)
      so Zerkalo offers "Use your Kartoteka library"; enable "Tell Kartoteka" at that moment, with
      consent.
- [ ] **"Cited but never read" / "cited but missing from the library"** views, built on the
      now-accurate "Used in".
- [ ] Document `library.yml` as the stable contract (always valid Hayagriva/Typst).

## 6. Pereplyot integration (M) — mostly in the Pereplyot repo

- [ ] **Copy as Typst quote:** `#quote(block: true)[…] @key[p. 12]` using the printed page label.
      Pereplyot is launched with `--key=`, so it already knows the entry.
- [ ] **`AnnotationSidecar::to_typst`** next to `to_markdown`, so highlights drop into a Zerkalo
      file with `@key[p. N]` supplements. (`fond-bib`, UI-agnostic.)
- [ ] When Pereplyot isn't installed, say so once with an install link instead of silently opening
      a generic viewer and losing annotation capture.
- [ ] "Open in Kartoteka" from Pereplyot.

## 7. Data safety across the suite (S–M)

- [x] **Unknown relation predicates are preserved, not fatal** (notes and nodes) — set aside on
      read, written back verbatim. Still to consider: unknown *fields* inside a known relation, and
      having `fsck` mention how many unrecognised relations a library holds.
- [ ] Align pinned `fond-bib` tags across Zerkalo, Pereplyot and Kartoteka; add a CI check.

## 8. Engineering (L, incremental)

- [ ] Split `app_window.rs` (~11.9k lines, 142 fns) into modules — `add_box.rs` is the first
      (detail pane, backup, search, export next).
- [ ] Replace `unsafe` widget-data row-key lookups with a typed `GObject` row.
- [ ] A thin action/state layer to prevent the re-entrant-lock and UI-thread keyring bug classes.
- [ ] Break the dialog reference cycles (closures capturing their own dialog) as in `add_box.rs`.
- [ ] GUI regression tests for the logic that can be isolated (the Add box's `describe`, drop
      classification are the pattern).

## 9. Strategic (decide, don't just build)

- [ ] **Windows/macOS.** Linux flatpak only, proprietary: "better than Zotero" currently reaches
      Linux users. The Tauri plan in `ARCHITECTURE.md` is the real adoption question.
- [ ] Release: this branch is `[Unreleased]`; choose a version and run the release workflow
      (version lives in two `Cargo.toml` files plus metainfo).

## Suggested order

1. §0 (verify, then polish the Add box) → §1 (first-run, pane width, key copy)
2. §7 (forward-compatible parsing — cheap, protects data) → §5/§6 locator and quote features
3. §2 (menu/backup) → §3 (search) → §4 (Zotero import)
4. §8 alongside everything; §9 when ready to widen the audience
