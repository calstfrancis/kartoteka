//! Round-trip and behaviour tests for the on-disk library (per the brief: test the
//! round-trips, not tautologies).

use std::collections::HashSet;
use std::fs;

use fond_bib::library::Library;

const BERDYAEV: &str = "berdyaev1937destiny:\n  type: book\n  title: The Destiny of Man\n  author: Berdyaev, Nikolai\n  date: 1937\n";

const CONE: &str = "cone1970black:\n  type: article\n  title: Black Theology and Black Power\n  author: Cone, James H.\n  date: 1970\n";

fn temp_library() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::init(dir.path()).unwrap();
    (dir, lib)
}

#[test]
fn init_creates_layout_and_gitignore() {
    let (dir, _lib) = temp_library();
    for sub in ["entries", "notes", "annots", "collections"] {
        assert!(dir.path().join(sub).is_dir(), "missing {sub}/");
    }
    let ignore = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(ignore.contains("attachments/"));
    assert!(ignore.contains(".kartoteka/"));
}

#[test]
fn entry_yaml_round_trips_byte_identical_modulo_formatting() {
    let (_dir, lib) = temp_library();
    fs::write(lib.entry_path("berdyaev1937destiny"), BERDYAEV).unwrap();

    let parsed = lib.load_entry("berdyaev1937destiny").unwrap();
    let once = fond_bib::entry::serialize_entry(&parsed.entry).unwrap();
    // Reparse the canonical form and serialize again — must be idempotent.
    let reparsed = fond_bib::entry::parse_single(&once, std::path::Path::new("x")).unwrap();
    let twice = fond_bib::entry::serialize_entry(&reparsed.entry).unwrap();
    assert_eq!(once, twice, "serialization is not stable");
}

#[test]
fn add_generates_keys_and_handles_collision() {
    let (_dir, lib) = temp_library();
    // Two entries that generate the same base key.
    let a =
        "_a:\n  type: article\n  title: Faith and Reason\n  author: Smith, John\n  date: 2020\n";
    let b = "_b:\n  type: article\n  title: Faith Alone\n  author: Smith, Jane\n  date: 2020\n";

    let k1 = lib.add_from_yaml(a).unwrap();
    let k2 = lib.add_from_yaml(b).unwrap();
    assert_eq!(k1, vec!["smith2020faith"]);
    assert_eq!(k2, vec!["smith2020faithb"]);
    assert!(lib.entry_path("smith2020faith").exists());
    assert!(lib.entry_path("smith2020faithb").exists());
}

#[test]
fn library_yml_regeneration_is_deterministic() {
    let (_dir, lib) = temp_library();
    fs::write(lib.entry_path("berdyaev1937destiny"), BERDYAEV).unwrap();
    fs::write(lib.entry_path("cone1970black"), CONE).unwrap();

    lib.regenerate_library_yml().unwrap();
    let first = fs::read_to_string(lib.library_yml_path()).unwrap();
    lib.regenerate_library_yml().unwrap();
    let second = fs::read_to_string(lib.library_yml_path()).unwrap();

    assert_eq!(first, second, "library.yml regeneration is not byte-stable");
    // Both entries appear, and it is valid Hayagriva (parses as two entries).
    assert!(first.contains("berdyaev1937destiny"));
    assert!(first.contains("cone1970black"));
    let reparsed = fond_bib::entry::parse_all(&first, std::path::Path::new("library.yml")).unwrap();
    assert_eq!(reparsed.len(), 2);
    // Deterministic order: berdyaev sorts before cone.
    let bpos = first.find("berdyaev1937destiny").unwrap();
    let cpos = first.find("cone1970black").unwrap();
    assert!(bpos < cpos, "entries not in ascending-key order");
}

#[test]
fn nuke_library_yml_and_reindex_reproduces_it() {
    let (_dir, lib) = temp_library();
    lib.add_from_yaml(BERDYAEV).unwrap();
    let before = fs::read_to_string(lib.library_yml_path()).unwrap();

    fs::remove_file(lib.library_yml_path()).unwrap();
    lib.regenerate_library_yml().unwrap();
    let after = fs::read_to_string(lib.library_yml_path()).unwrap();

    assert_eq!(before, after);
}

#[test]
fn fsck_clean_on_good_library() {
    let (_dir, lib) = temp_library();
    lib.add_from_yaml(BERDYAEV).unwrap();
    let report = lib.fsck().unwrap();
    assert!(report.is_clean(), "expected clean, got {report:?}");
}

#[test]
fn fsck_reports_every_seeded_problem() {
    let (dir, lib) = temp_library();
    lib.add_from_yaml(BERDYAEV).unwrap();

    // (1) filename/inner-key mismatch.
    fs::write(
        lib.entry_path("wrongname"),
        "actualkey:\n  type: book\n  title: Mislabeled\n  author: Doe, Jane\n  date: 2000\n",
    )
    .unwrap();

    // (2) dangling collection reference.
    fs::write(
        lib.collection_path("broken"),
        "name: Broken\nkeys:\n- doesnotexist\n",
    )
    .unwrap();

    // (3) missing attachment: a note records a blob that is not present.
    fs::write(
        lib.note_path("berdyaev1937destiny"),
        "---\nattachments:\n  - hash: blake3:deadbeef\n    filename: ghost.pdf\n    bytes: 10\n---\nnote body\n",
    )
    .unwrap();

    // (4) orphaned + (5) hash-mismatched blob.
    let attach = dir.path().join("attachments");
    fs::create_dir_all(&attach).unwrap();
    fs::write(attach.join("0000orphanhash"), b"orphan bytes").unwrap();

    let report = lib.fsck().unwrap();
    assert!(!report.is_clean());
    assert_eq!(report.key_filename_mismatches.len(), 1);
    assert_eq!(report.dangling_collection_refs.len(), 1);
    assert_eq!(report.missing_attachments.len(), 1);
    // The orphan blob is both unrecorded and hashes to something other than its name.
    assert_eq!(report.orphaned_attachments.len(), 1);
    assert_eq!(report.hash_mismatched_attachments.len(), 1);
}

#[test]
fn existing_keys_reads_from_filenames() {
    let (_dir, lib) = temp_library();
    fs::write(lib.entry_path("berdyaev1937destiny"), BERDYAEV).unwrap();
    fs::write(lib.entry_path("cone1970black"), CONE).unwrap();
    let keys: HashSet<String> = lib.existing_keys().unwrap();
    assert!(keys.contains("berdyaev1937destiny"));
    assert!(keys.contains("cone1970black"));
    assert_eq!(keys.len(), 2);
}

#[test]
fn finds_and_merges_duplicates() {
    let (_dir, lib) = temp_library();
    // Two entries with the same DOI → a duplicate group.
    fs::write(
        lib.entry_path("cone1970a"),
        "cone1970a:\n  type: article\n  title: Black Theology\n  author: Cone, James\n  date: 1970\n  serial-number:\n    doi: 10.1/cone\n",
    )
    .unwrap();
    fs::write(
        lib.entry_path("cone1970b"),
        "cone1970b:\n  type: article\n  title: Black Theology and Black Power\n  author: Cone, James H.\n  date: 1970\n  serial-number:\n    doi: 10.1/CONE\n",
    )
    .unwrap();
    fs::write(
        lib.note_path("cone1970b"),
        "---\ntags: [christology]\n---\nnote from b\n",
    )
    .unwrap();

    let dups = lib.find_duplicates().unwrap();
    assert_eq!(dups.len(), 1);
    assert_eq!(dups[0].len(), 2);

    lib.merge_group(&dups[0], "cone1970a").unwrap();
    assert!(lib.entry_path("cone1970a").exists());
    assert!(!lib.entry_path("cone1970b").exists());
    // b's tag folded into a's note.
    let note = lib.load_note("cone1970a").unwrap().unwrap();
    assert!(note.frontmatter.tags.contains(&"christology".to_string()));
    assert!(note.body.contains("note from b"));
}

#[test]
fn set_related_is_symmetric() {
    let (_dir, lib) = temp_library();
    for k in ["a", "b", "c"] {
        fs::write(
            lib.entry_path(k),
            format!("{k}:\n  type: book\n  title: Title {k}\n"),
        )
        .unwrap();
    }

    // Link a → {b, c}: both should link back to a.
    lib.set_related("a", &["b".into(), "c".into()]).unwrap();
    assert_eq!(lib.related("a").unwrap(), vec!["b", "c"]);
    assert_eq!(lib.related("b").unwrap(), vec!["a"]);
    assert_eq!(lib.related("c").unwrap(), vec!["a"]);

    // Drop c from a's list: c must lose a, b keeps it.
    lib.set_related("a", &["b".into()]).unwrap();
    assert_eq!(lib.related("a").unwrap(), vec!["b"]);
    assert_eq!(lib.related("b").unwrap(), vec!["a"]);
    assert!(lib.related("c").unwrap().is_empty());

    // Self-links are ignored.
    lib.set_related("a", &["a".into(), "b".into()]).unwrap();
    assert_eq!(lib.related("a").unwrap(), vec!["b"]);
}
