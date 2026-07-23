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
