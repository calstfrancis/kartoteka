# Kartoteka — UX and integration review

Date: 2026-10-03 · Reviewed at v0.18.2.

Scope: Kartoteka as an academic citation manager that works with Zerkalo (Typst authoring)
and Pereplyot (PDF/EPUB reading and annotation), judged for academics, new users,
non-technical users, and people leaving Zotero or Mendeley. The guiding principle is the
project's own: intuitive design, simplicity as beauty.

**Method.** Read the four `fond-*` crates, the CLI, the GTK app source, and the Zerkalo and
Pereplyot sides of the integration. Ran the CLI and the `fond-bib` tests. The GTK app itself
was not run for this review, so GUI findings come from reading the code.

**Decision recorded: citation keys stay visible.** The Key column and the "Citation key"
row in the detail pane are essential to the Typst workflow and are not to be hidden or made
optional. Where this review touches keys, the aim is to make them *easier to use* (one-click
copy, visible everywhere a citation is made), never less present.

The foundation is right: plain files, a `library.yml` that is always current, a git-backed
vault. The weak spots are an interface that has grown by accretion, and handoffs between the
three apps that still depend on the user knowing what happens underneath.

---

## 1. Verified problems

| # | Finding | Evidence | Status |
|---|---|---|---|
| 1 | **"Used in" under-counts Typst citations.** `scan_typst_citation_keys` finds `@key` but not `#cite(<key>)`. Zerkalo's "Tell Kartoteka" feature feeds this scanner. | Probe: 3 of 6 common citation forms missed. The old unit test asserted `<berdyaev1937destiny>` was absent. | Fixed |
| 2 | **Journal, volume, issue and pages can't be edited after an entry exists.** The New-item form has them; the detail-pane editor (`EntryFields`) does not. | `fond_bib::entry::EntryFields` has type, title, creators, year, publisher, location, DOI, ISBN only. A DOI lookup with incomplete data can't be corrected in the UI. | Fixed |
| 3 | **Export bibliography refuses to open with no collections**, and offers a fixed list of four styles. | Toast: "No collections to export (import from Zotero creates them)". `turabian` fails and there is no way to list valid styles. | Fixed |
| 4 | **GitHub backup silently leaves out PDFs.** `attachments/` is gitignored and the backup wizard never says so. | WebDAV does include attachments; GitHub does not. | Fixed (disclosed in the backup dialogs; a real fix is to back PDFs up too) |
| 5 | **Drag-and-drop accepts PDFs only.** | EPUB, `.bib` and `.ris` drops give "Only PDF files can be dropped". | Fixed (EPUB, folders; `.bib` redirects to Import; `.ris` is still unsupported) |
| 6 | **Docs are stale.** | README said 0.11.0; `docs/ROADMAP.md` and `M2-GUI-PLAN.md` are referenced but absent. | Fixed |

---

Also observed while running the app headless: at the default 1300px window the detail pane is
only ~320px wide, narrower than the creator-editor row, so the right edge (the "Single field",
reorder and delete buttons) is clipped. A wider default pane, or a creator row that wraps, would
fix the first-run view. Not yet addressed.

## 2. Design moves, simplest first

1. **One Add box instead of "Acquire a reference…".** Today the user picks DOI, arXiv or ISBN
   from a dropdown before typing anything. Accept anything pasted or dropped: detect DOI,
   arXiv ID, ISBN, URL, BibTeX, PDF/EPUB, or title text. Add Crossref title search and a
   second ISBN source for when OpenLibrary has no match (theology monographs are patchy there).
2. **Shrink the hamburger menu (about 35 rows) to about 8.** Keep Add, Cite, Collections,
   Backup and Settings. Put Nodes, Relations map, Custom fields, Tasks, Reindex and Columns
   behind an "Advanced" switch. Fold the six backup entries into one Backup dialog. Rename
   jargon: Acquire → Add, Nodes → People & Ideas, Reindex → Repair search.
3. **Honest backup status in the status bar**, e.g. "Backed up 4 min ago · PDFs not included".
4. **Forgiving search.** Match half-typed words and ignore accents (Zerkalo already does);
   show where a hit was found ("PDF p. 14: …context…").
5. **One-click Zotero switch.** Detect `~/Zotero`, import items, collections, notes and files
   with a progress bar. Today it requires a BetterBibTeX export first. Add RIS and CSL-JSON.
6. **A first-run flow that ends with a first source in the library.**
7. **Keys stay visible; make them handier.** ✔ The key now shows under the title in the detail
   pane with a copy button. Still to do: copy from the spreadsheet row; the Cite action stays
   on Ctrl+K.

---

## 3. Zerkalo and Pereplyot integration

- **Locators.** The Cite picker copies only `@key`, and Zerkalo's citation panel inserts only
  `@key`. Academics cite pages. Add an optional page field producing `@key[p. 12]`
  (Typst `#cite(<key>, supplement: [p. 12])` for the long form).
- **Quotes with citations.** Pereplyot is launched with `--key=` so it already knows the
  entry. Add "Copy as Typst quote" → `#quote(block: true)[…] @key[p. 12]` using the printed
  page label. `AnnotationSidecar::to_markdown` should gain a Typst sibling so highlights drop
  straight into a Zerkalo literature-review file.
- **Vault discovery.** Zerkalo makes the user choose the vault folder by hand, and "Tell
  Kartoteka" is off by default. A small shared pointer file (e.g.
  `~/.local/share/fond/libraries.json`) would let Zerkalo offer "Use your Kartoteka
  library" and enable "Used in" at that moment, with consent.
- **Close the loop with the writing.** With accurate "Used in": "cited in your essay but never
  read", and "cited but missing from the library".
- **Version drift.** Zerkalo pins `fond-bib v0.9.0`; Kartoteka is at 0.18.2. Three apps at
  different versions write one vault, yet an unknown relation predicate still makes a note
  fail to parse (listed as "deliberately not changed" in the 0.17.2 changelog). Unknown
  fields and predicates should be preserved and ignored, not fatal.
- **Pereplyot absent.** "Read" quietly falls back to a generic viewer, and the user loses
  annotation capture without being told. Say so once, with an install link.
- **`library.yml` is the contract.** Document that it is always valid Hayagriva and Typst.
  Typst chooses the citation style at compile time, so the in-app Export dialog is secondary.

---

## 4. By audience

- **Leaving Zotero:** one-click migration, RIS/CSL-JSON import, a full style picker (Turabian
  and SBL matter for theology), accurate "Used in" counts.
- **New users:** one Add box, a guided first source, a much smaller menu.
- **Non-technical users:** plain words, honest backup status, no git vocabulary on the main path.

## 5. Strategic and engineering notes

- **Platform.** Linux flatpak only, and proprietary. That suits the Fond ecosystem, but it
  limits "better than Zotero" to Linux users. The Windows/Tauri plan in `ARCHITECTURE.md` is
  the real adoption question.
- **Maintainability.** `app_window.rs` is one file of ~11.9k lines and 142 functions, with
  `unsafe` widget-data lookups for row keys. Splitting it into modules and giving rows a typed
  GObject would make the UX changes above safer. Past crashes (a re-entrant lock panic, a
  keyring freeze on the UI thread) came from ad-hoc state handling in this layer.

## 6. Suggested order

1. **Days:** ✔ scanner fix, ✔ missing detail fields, ✔ export without collections and a full
   style list, ✔ backup disclosure, ✔ wider drop support, ✔ README refresh. Still open from this
   tier: the detail-pane width at the default window size.
2. **Weeks:** unified Add box, menu consolidation, Backup status, locators in Cite,
   Typst annotation export, vault discovery.
3. **Larger:** Zotero auto-import, "cited but unread" views, search snippets, forward-compatible
   parsing, the Windows question.
