//! BetterBibTeX `.bib` import tests: key preservation, tags → notes, attachment
//! copying, and the migration report.

use std::fs;

use fond_bib::import::ImportOptions;
use fond_bib::library::Library;

fn temp_library() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::init(dir.path()).unwrap();
    (dir, lib)
}

fn bib_with_attachment(pdf_path: &str) -> String {
    format!(
        "@book{{berdyaev1937destiny,\n  title = {{The Destiny of Man}},\n  author = {{Berdyaev, Nikolai}},\n  date = {{1937}},\n  keywords = {{existentialism, russian philosophy}},\n  file = {{{pdf_path}}}\n}}\n\n@article{{cone1970black,\n  title = {{Black Theology and Black Power}},\n  author = {{Cone, James H.}},\n  year = {{1970}},\n  keywords = {{liberation theology}}\n}}\n"
    )
}

#[test]
fn imports_entries_preserving_keys_with_tags_and_attachment() {
    let (dir, lib) = temp_library();

    // A real file to stand in for a Zotero PDF attachment.
    let pdf = dir.path().join("Berdyaev.pdf");
    fs::write(&pdf, b"%PDF-1.7 fake pdf bytes").unwrap();

    let source = bib_with_attachment(pdf.to_str().unwrap());
    let report = lib
        .import_bibtex(&source, &ImportOptions::default())
        .unwrap();

    // Keys preserved exactly (would break citations otherwise).
    assert!(lib.entry_path("berdyaev1937destiny").exists());
    assert!(lib.entry_path("cone1970black").exists());
    assert_eq!(report.imported.len(), 2);

    // Tags landed in the notes, not the entry files.
    let note = lib.load_note("berdyaev1937destiny").unwrap().unwrap();
    assert_eq!(
        note.frontmatter.tags,
        vec!["existentialism", "russian philosophy"]
    );
    assert!(note.frontmatter.date_added.is_some());
    let entry_yaml = lib.read_entry_raw("berdyaev1937destiny").unwrap();
    assert!(
        !entry_yaml.contains("existentialism"),
        "tags must not leak into the entry"
    );

    let cone_note = lib.load_note("cone1970black").unwrap().unwrap();
    assert_eq!(cone_note.frontmatter.tags, vec!["liberation theology"]);

    // Attachment hashed + copied; record present.
    assert_eq!(note.frontmatter.attachments.len(), 1);
    let att = &note.frontmatter.attachments[0];
    assert!(att.hash.starts_with("blake3:"));
    assert_eq!(att.filename, "Berdyaev.pdf");
    let hex = att.hash.trim_start_matches("blake3:");
    assert!(lib.attachment_blob_path(hex).exists());
    assert_eq!(report.attachments_copied.len(), 1);

    // library.yml regenerated and valid.
    let libyml = fs::read_to_string(lib.library_yml_path()).unwrap();
    assert!(libyml.contains("berdyaev1937destiny"));
    assert!(libyml.contains("cone1970black"));

    assert!(report.is_clean(), "expected clean report, got {report:?}");
}

#[test]
fn missing_attachment_is_reported_not_fatal() {
    let (_dir, lib) = temp_library();
    let source = bib_with_attachment("/nonexistent/path/ghost.pdf");
    let report = lib
        .import_bibtex(&source, &ImportOptions::default())
        .unwrap();

    assert_eq!(report.imported.len(), 2);
    assert_eq!(report.attachments_missing.len(), 1);
    assert_eq!(report.attachments_missing[0].0, "berdyaev1937destiny");
    assert!(!report.is_clean());
}

#[test]
fn re_import_skips_existing_keys_unless_overwrite() {
    let (_dir, lib) = temp_library();
    let source = "@book{smith2020faith,\n  title = {Faith},\n  author = {Smith, John},\n  date = {2020}\n}\n";

    let first = lib
        .import_bibtex(source, &ImportOptions::default())
        .unwrap();
    assert_eq!(first.imported.len(), 1);

    let second = lib
        .import_bibtex(source, &ImportOptions::default())
        .unwrap();
    assert_eq!(second.imported.len(), 0);
    assert_eq!(second.skipped_key_collisions, vec!["smith2020faith"]);

    let overwrite = ImportOptions {
        overwrite: true,
        ..ImportOptions::default()
    };
    let third = lib.import_bibtex(source, &overwrite).unwrap();
    assert_eq!(third.imported, vec!["smith2020faith"]);
}

#[test]
fn reports_unmapped_fields() {
    let (_dir, lib) = temp_library();
    // `mendeley-tags` is a Mendeley-ism Kartoteka does not track; it should be flagged.
    // (`annotation` is intentionally NOT flagged — Hayagriva maps it to the note field.)
    let source = "@book{doe2000x,\n  title = {X},\n  author = {Doe, Jane},\n  date = {2000},\n  annotation = {a private note},\n  mendeley-tags = {foo}\n}\n";
    let report = lib
        .import_bibtex(source, &ImportOptions::default())
        .unwrap();
    assert_eq!(report.imported.len(), 1);
    let (key, fields) = &report.unmapped_fields[0];
    assert_eq!(key, "doe2000x");
    assert!(fields.iter().any(|f| f == "mendeley-tags"));
    assert!(!fields.iter().any(|f| f == "annotation"));
}

const CSL_EXPORT: &str = r#"[
  {"id":"cone1970black","type":"article-journal","title":"Black Theology and Black Power",
   "author":[{"family":"Cone","given":"James H."}],"container-title":"Christianity and Crisis",
   "volume":"30","issue":"1","page":"6-13","issued":{"date-parts":[[1970]]},
   "DOI":"10.2307/abc","keyword":"christology, liberation"},
  {"id":"http://zotero.org/users/1/items/AB12","type":"book","title":"The Destiny of Man",
   "author":[{"family":"Berdyaev","given":"Nikolai"}],"publisher":"Geoffrey Bles",
   "publisher-place":"London","issued":{"date-parts":[[1937]]},"ISBN":"9780140449136"}
]"#;

#[test]
fn csl_json_import_keeps_better_bibtex_keys_makes_the_rest_and_writes_tags() {
    use fond_bib::entry::read_fields;
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::init(dir.path()).unwrap();

    let report = lib
        .import_csl_json(CSL_EXPORT, &ImportOptions::default())
        .unwrap();
    assert_eq!(report.imported.len(), 2, "{report:?}");
    assert!(report.imported.contains(&"cone1970black".to_string()));
    // The Zotero-URL id is not a key; a fresh one is made from the entry.
    assert!(
        report.imported.contains(&"berdyaev1937destiny".to_string()),
        "{:?}",
        report.imported
    );

    let cone = read_fields(&lib.load_entry("cone1970black").unwrap().entry);
    assert_eq!(cone.entry_type, "article");
    assert_eq!(
        (
            cone.container.as_str(),
            cone.volume.as_str(),
            cone.issue.as_str(),
            cone.pages.as_str()
        ),
        ("Christianity and Crisis", "30", "1", "6-13")
    );
    assert_eq!(
        (cone.year.as_str(), cone.doi.as_str()),
        ("1970", "10.2307/abc")
    );
    let book = read_fields(&lib.load_entry("berdyaev1937destiny").unwrap().entry);
    assert_eq!(
        (
            book.publisher.as_str(),
            book.location.as_str(),
            book.isbn.as_str()
        ),
        ("Geoffrey Bles", "London", "9780140449136")
    );

    let note = lib.load_note("cone1970black").unwrap().unwrap();
    assert_eq!(note.frontmatter.tags, ["christology", "liberation"]);

    // library.yml is regenerated, and the imported entries render as proper citations.
    assert!(std::fs::read_to_string(lib.library_yml_path())
        .unwrap()
        .contains("cone1970black"));
    let style = fond_bib::resolve_style("chicago-author-date").unwrap();
    let rendered = lib
        .bibliography_for_all(&style, fond_bib::BufWriteFormat::Plain)
        .unwrap();
    let text = rendered
        .iter()
        .map(|r| r.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Christianity and Crisis"), "{text}");
    assert!(text.contains("Geoffrey Bles"), "{text}");
}

#[test]
fn importing_the_same_export_twice_adds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::init(dir.path()).unwrap();
    lib.import_csl_json(CSL_EXPORT, &ImportOptions::default())
        .unwrap();
    let again = lib
        .import_csl_json(CSL_EXPORT, &ImportOptions::default())
        .unwrap();
    assert!(again.imported.is_empty(), "{again:?}");
    assert_eq!(again.skipped_key_collisions.len(), 2);
    assert_eq!(lib.keys_sorted().unwrap().len(), 2);

    // The same books arriving as RIS (no keys) are recognised by DOI / ISBN too.
    let ris = "TY  - JOUR\nTI  - Black Theology and Black Power\nAU  - Cone, James H.\nDO  - 10.2307/ABC\nER  -\n\nTY  - BOOK\nTI  - The Destiny of Man\nAU  - Berdyaev, Nikolai\nSN  - 0-14-044913-2\nER  -\n";
    let from_ris = lib.import_ris(ris, &ImportOptions::default()).unwrap();
    assert!(from_ris.imported.is_empty(), "{from_ris:?}");
    assert_eq!(lib.keys_sorted().unwrap().len(), 2);
}

#[test]
fn ris_import_generates_keys_and_collision_free_ones_for_lookalikes() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::init(dir.path()).unwrap();
    let ris = "TY  - JOUR\nAU  - Cone, James H.\nTI  - Black Theology\nPY  - 1970\nKW  - christology\nER  -\n\nTY  - JOUR\nAU  - Cone, James H.\nTI  - Black Power\nPY  - 1970\nER  -\n";
    let report = lib.import_ris(ris, &ImportOptions::default()).unwrap();
    assert_eq!(report.imported.len(), 2, "{report:?}");
    let mut keys = report.imported.clone();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 2, "keys collided: {:?}", report.imported);
    assert!(keys.iter().all(|k| k.starts_with("cone1970")), "{keys:?}");
    let tagged = lib.load_note(&report.imported[0]).unwrap().unwrap();
    assert_eq!(tagged.frontmatter.tags, ["christology"]);
}

#[test]
fn a_file_that_is_not_the_format_is_a_clear_error_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::init(dir.path()).unwrap();
    assert!(lib
        .import_ris("this is not ris", &ImportOptions::default())
        .is_err());
    assert!(lib
        .import_csl_json("{oops", &ImportOptions::default())
        .is_err());
    assert!(lib.keys_sorted().unwrap().is_empty());
}
