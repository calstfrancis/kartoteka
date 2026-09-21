//! Round-trip and behaviour tests for the on-disk library (per the brief: test the
//! round-trips, not tautologies).

use std::collections::HashSet;
use std::fs;

use fond_bib::library::Library;
use fond_bib::{Node, NodeType, Predicate, Relation};

/// Seed `keys` as minimal book entries in `lib`.
fn seed_entries(lib: &Library, keys: &[&str]) {
    for k in keys {
        fs::write(
            lib.entry_path(k),
            format!("{k}:\n  type: book\n  title: Title {k}\n"),
        )
        .unwrap();
    }
}

/// Seed a node file with the given type and label (no relations).
fn seed_node(lib: &Library, slug: &str, node_type: &str, label: &str) {
    fs::write(
        lib.node_path(slug),
        format!("---\nnode-type: {node_type}\nlabel: {label}\n---\n"),
    )
    .unwrap();
}

/// The `(predicate, target, inverse)` triples on `key`'s note, for terse assertions.
fn edges(lib: &Library, key: &str) -> Vec<(Predicate, String, bool)> {
    lib.relations(key)
        .unwrap()
        .into_iter()
        .map(|r| (r.predicate, r.target, r.inverse))
        .collect()
}

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
    for sub in [
        "entries",
        "notes",
        "annots",
        "collections",
        "ai",
        "projects",
        "nodes",
    ] {
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

/// Merging used to keep only tags/attachments/body/annotations and silently destroy the rest
/// of the merged-away entry: rating, read status, tasks, relations, child notes, AI sidecar —
/// and left other records' edges pointing at the deleted key.
#[test]
fn merge_group_keeps_everything_the_duplicate_carried() {
    use fond_bib::Predicate;
    let (_dir, lib) = temp_library();
    let mk = |k: &str, extra: &str| {
        fs::write(
            lib.entry_path(k),
            format!("{k}:\n  type: article\n  title: Black Theology\n  author: Cone, James\n  date: 1970\n{extra}"),
        )
        .unwrap();
    };
    mk("keep", "");
    // The duplicate is the one that has the DOI, the rating, the status and the edges.
    mk("dup", "  serial-number:\n    doi: 10.1/cone\n");
    mk("third", "");
    mk("cited", "");
    fs::write(
        lib.note_path("dup"),
        "---\nrating: 4\nread-status: reading\ntasks:\n  - text: reread ch.2\ncustom-fields:\n  mood: dense\n---\nnote from dup\n",
    )
    .unwrap();
    lib.add_relation("dup", Predicate::Cites, "cited").unwrap();
    lib.add_relation("third", Predicate::Cites, "dup").unwrap();
    let child = lib.create_child_note("dup", "a child note").unwrap();
    fs::create_dir_all(lib.root().join("ai")).unwrap();
    fs::write(
        lib.ai_path("dup"),
        "schema: 1\ngenerated-by: x\ngenerated-at: t\n",
    )
    .unwrap();

    lib.merge_group(&["keep".to_string(), "dup".to_string()], "keep")
        .unwrap();

    let note = lib.load_note("keep").unwrap().unwrap();
    assert_eq!(note.frontmatter.rating, Some(4), "rating lost");
    assert_eq!(
        note.frontmatter.read_status,
        Some(fond_bib::ReadStatus::Reading)
    );
    assert_eq!(note.frontmatter.tasks.len(), 1, "task lost");
    assert_eq!(
        note.frontmatter
            .custom_fields
            .get("mood")
            .map(String::as_str),
        Some("dense")
    );
    assert!(
        note.frontmatter
            .relations
            .iter()
            .any(|r| r.target == "cited"),
        "the duplicate's own edge was lost"
    );
    let fields = fond_bib::entry::read_fields(&lib.load_entry("keep").unwrap().entry);
    assert_eq!(
        fields.doi, "10.1/cone",
        "DOI only the duplicate had was lost"
    );

    // Child note and AI sidecar moved, not orphaned.
    assert!(lib.child_note_ids("keep").unwrap().contains(&child));
    assert!(!lib.child_note_dir("dup").exists(), "child notes orphaned");
    assert!(lib.ai_path("keep").exists() && !lib.ai_path("dup").exists());

    // Someone else's edge to the deleted key now points at the survivor.
    let third = lib.load_note("third").unwrap().unwrap();
    assert!(
        third
            .frontmatter
            .relations
            .iter()
            .all(|r| r.target != "dup"),
        "dangling edge to dup"
    );
    assert!(third
        .frontmatter
        .relations
        .iter()
        .any(|r| r.target == "keep"));
}

const CORRUPT_NOTE: &str =
    "---\nrelations:\n  - predicate: not-a-real-predicate\n    target: b\n---\nbody\n";

/// One corrupt note used to abort `fsck` at the `?` — no report at all, on exactly the
/// libraries that need one.
#[test]
fn fsck_reports_a_corrupt_note_instead_of_aborting() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["a", "b"]);
    fs::write(lib.note_path("a"), CORRUPT_NOTE).unwrap();
    let report = lib.fsck().expect("fsck must not abort on one bad note");
    assert!(
        report.unreadable.iter().any(|(p, _)| p.ends_with("a.md")),
        "corrupt note not reported: {report:?}"
    );
}

#[test]
fn fsck_flags_orphaned_records_and_bad_collection_parents() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["a"]);
    fs::write(lib.note_path("ghost"), "---\ntags: [x]\n---\n").unwrap();
    fs::write(lib.collection_path("x"), "name: X\nparent: y\nkeys: []\n").unwrap();
    fs::write(lib.collection_path("y"), "name: Y\nparent: x\nkeys: []\n").unwrap();
    fs::write(
        lib.collection_path("z"),
        "name: Z\nparent: nowhere\nkeys: []\n",
    )
    .unwrap();
    let report = lib.fsck().unwrap();
    assert!(report.orphaned_records.iter().any(|p| p.contains("ghost")));
    assert!(report
        .bad_collection_parents
        .iter()
        .any(|(s, d)| s == "z" && d.contains("does not exist")));
    assert!(report
        .bad_collection_parents
        .iter()
        .any(|(_, d)| d.contains("cycle")));
}

/// `fsck --fix` skipped an unreadable host, then judged every inverse edge pointing at it
/// "orphaned" and deleted it. It must refuse instead.
#[test]
fn reconcile_fix_refuses_when_a_host_is_unreadable() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["a", "b", "c"]);
    lib.add_relation("c", Predicate::Cites, "a").unwrap();
    let before = fs::read_to_string(lib.note_path("a")).unwrap();
    fs::write(lib.note_path("b"), CORRUPT_NOTE).unwrap();
    assert!(
        lib.reconcile_relations(true).is_err(),
        "repair ran with an unreadable host"
    );
    assert_eq!(
        fs::read_to_string(lib.note_path("a")).unwrap(),
        before,
        "a's inverse edge was touched"
    );
    // The read-only report still works.
    assert!(lib.reconcile_relations(false).is_ok());
}

/// An unreadable note elsewhere was treated as "references nothing", so deleting an entry
/// could delete a blob that note still owned.
#[test]
fn delete_entry_keeps_blobs_when_another_note_is_unreadable() {
    let (dir, lib) = temp_library();
    seed_entries(&lib, &["a", "b"]);
    let blobs = dir.path().join("attachments");
    fs::create_dir_all(&blobs).unwrap();
    fs::write(blobs.join("abcd1234"), b"pdf").unwrap();
    let att =
        "---\nattachments:\n  - hash: blake3:abcd1234\n    filename: x.pdf\n    bytes: 3\n---\n";
    fs::write(lib.note_path("a"), att).unwrap();
    fs::write(lib.note_path("b"), CORRUPT_NOTE).unwrap();
    lib.delete_entry("a").unwrap();
    assert!(
        blobs.join("abcd1234").exists(),
        "blob deleted although an unreadable note may own it"
    );
}

#[test]
fn delete_entry_cleans_legacy_related_and_derived_from_book() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["a", "b"]);
    fs::write(
        lib.note_path("b"),
        "---\nrelated: [a]\nderived-from-book: a\nderived-from-role: editor\n---\n",
    )
    .unwrap();
    lib.delete_entry("a").unwrap();
    let b = lib.load_note("b").unwrap().unwrap();
    assert!(b.frontmatter.related.is_empty());
    assert!(b.frontmatter.derived_from_book.is_none() && b.frontmatter.derived_from_role.is_none());
}

/// A chapter keeps its book's date only inside `parent:`; its key used to say `nodate`.
#[test]
fn book_part_key_uses_the_parent_books_year() {
    use fond_bib::{Creator, CreatorRole};
    let (_dir, lib) = temp_library();
    let book = fond_bib::entry::parse_single(
        "book:\n  type: anthology\n  title: Essays on Being\n  author: Doe, Jane\n  date: 1999\n",
        std::path::Path::new("b"),
    )
    .unwrap();
    let yaml = fond_bib::entry::book_part_yaml(
        &book.entry,
        CreatorRole::Editor,
        "chapter",
        "On Personhood",
        &[Creator::new(CreatorRole::Author, "Smith", "John")],
        "1-10",
    )
    .unwrap();
    let keys = lib.add_from_yaml(&yaml).unwrap();
    assert!(
        !keys[0].contains("nodate"),
        "key lost the parent's year: {}",
        keys[0]
    );
    assert!(keys[0].contains("1999"), "{}", keys[0]);
    // ...but the editable Year field must not copy the parent's year onto the child.
    let f = fond_bib::entry::read_fields(&lib.load_entry(&keys[0]).unwrap().entry);
    assert_eq!(f.year, "");
}

/// A conference's top-level `location:` is its event place — real data, not a stray
/// publisher location — and editing the publisher must not delete it.
#[test]
fn editing_publisher_keeps_a_conference_event_location() {
    use fond_bib::entry::{self, EntryFields};
    let (_dir, lib) = temp_library();
    fs::write(
        lib.entry_path("conf"),
        "conf:\n  type: conference\n  title: Proceedings\n  date: 2020\n  location: Halifax\n",
    )
    .unwrap();
    let current = entry::read_fields(&lib.load_entry("conf").unwrap().entry);
    assert_eq!(
        current.location, "",
        "event place must not show up as a publisher location"
    );
    let edited = EntryFields {
        publisher: "ACM".into(),
        ..current
    };
    lib.edit_fields("conf", &edited).unwrap();
    let raw = fs::read_to_string(lib.entry_path("conf")).unwrap();
    assert!(
        raw.contains("location: Halifax"),
        "event place deleted: {raw}"
    );
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

#[test]
fn asymmetric_relation_materializes_typed_inverse() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["a", "b"]);

    // a cites b  ⇒  a holds forward `cites`, b holds inverse `cited-by`.
    lib.add_relation("a", Predicate::Cites, "b").unwrap();
    assert_eq!(
        edges(&lib, "a"),
        vec![(Predicate::Cites, "b".into(), false)]
    );
    assert_eq!(
        edges(&lib, "b"),
        vec![(Predicate::CitedBy, "a".into(), true)]
    );

    // Removing the forward edge removes the inverse on b too.
    lib.remove_relation("a", Predicate::Cites, "b").unwrap();
    assert!(edges(&lib, "a").is_empty());
    assert!(edges(&lib, "b").is_empty());
}

#[test]
fn self_inverse_related_is_symmetric_forward() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["a", "b"]);

    // a related b  ⇒  both hold a plain forward `related` edge (no inverse flag).
    lib.add_relation("a", Predicate::Related, "b").unwrap();
    assert_eq!(
        edges(&lib, "a"),
        vec![(Predicate::Related, "b".into(), false)]
    );
    assert_eq!(
        edges(&lib, "b"),
        vec![(Predicate::Related, "a".into(), false)]
    );
}

#[test]
fn set_relations_preserves_inbound_inverse_edges() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["a", "b", "c"]);

    // b cites a  ⇒  a gains an inverse `cited-by` from b.
    lib.add_relation("b", Predicate::Cites, "a").unwrap();
    // Now edit a's OWN forward edges (a critiques c). a's inbound inverse must survive.
    lib.set_relations("a", &[Relation::forward(Predicate::Critiques, "c")])
        .unwrap();

    let a = edges(&lib, "a");
    assert!(a.contains(&(Predicate::Critiques, "c".into(), false)));
    assert!(
        a.contains(&(Predicate::CitedBy, "b".into(), true)),
        "editing a's forward edges must not drop its inbound inverse: {a:?}"
    );
    assert_eq!(
        edges(&lib, "c"),
        vec![(Predicate::CritiquedBy, "a".into(), true)]
    );
}

#[test]
fn reconcile_repairs_desynced_inverse_and_is_idempotent() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["a", "b"]);
    lib.add_relation("a", Predicate::Cites, "b").unwrap();

    // Corrupt by hand: blow away b's note entirely (drops the maintained inverse edge).
    fs::remove_file(lib.note_path("b")).unwrap();
    let report = lib.reconcile_relations(false).unwrap();
    assert_eq!(report.missing.len(), 1, "should detect the missing inverse");

    // Fix it, and the inverse comes back.
    let fixed = lib.reconcile_relations(true).unwrap();
    assert_eq!(fixed.missing.len(), 1);
    assert_eq!(
        edges(&lib, "b"),
        vec![(Predicate::CitedBy, "a".into(), true)]
    );

    // Idempotent: a second fix finds nothing.
    let again = lib.reconcile_relations(true).unwrap();
    assert!(again.is_clean(), "reconcile must be idempotent: {again:?}");
}

#[test]
fn reconcile_removes_orphaned_inverse() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["a", "b"]);

    // Hand-write an inverse edge on b with no forward edge behind it anywhere.
    lib.set_relations("a", &[]).unwrap(); // ensure a has no forwards
    let mut note = lib.load_note("b").unwrap().unwrap_or_default();
    note.frontmatter.relations.push(Relation {
        predicate: Predicate::CitedBy,
        target: "a".into(),
        inverse: true,
    });
    lib.write_note("b", &note).unwrap();

    let report = lib.reconcile_relations(true).unwrap();
    assert_eq!(
        report.orphaned.len(),
        1,
        "orphaned inverse should be flagged"
    );
    assert!(
        edges(&lib, "b").is_empty(),
        "orphaned inverse should be removed"
    );
}

#[test]
fn reconcile_flags_dangling_target() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["a"]);
    // a cites a non-existent entry: forward edge stays, but reconcile flags it.
    lib.add_relation("a", Predicate::Cites, "ghost").unwrap();
    let report = lib.reconcile_relations(false).unwrap();
    assert_eq!(report.dangling_targets.len(), 1);
}

#[test]
fn migrate_lifts_legacy_related_to_typed_edges() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["a", "b", "c"]);

    // Legacy symmetric `related` written the old two-sided way.
    lib.set_related("a", &["b".into(), "c".into()]).unwrap();
    assert_eq!(lib.related("a").unwrap(), vec!["b", "c"]);

    let changed = lib.migrate_related_to_relations().unwrap();
    assert!(changed >= 1);

    // Legacy field is cleared everywhere; typed symmetric `related` edges replace it.
    assert!(lib.related("a").unwrap().is_empty());
    assert_eq!(
        edges(&lib, "a"),
        vec![
            (Predicate::Related, "b".into(), false),
            (Predicate::Related, "c".into(), false),
        ]
    );
    assert_eq!(
        edges(&lib, "b"),
        vec![(Predicate::Related, "a".into(), false)]
    );
    assert_eq!(
        edges(&lib, "c"),
        vec![(Predicate::Related, "a".into(), false)]
    );

    // Idempotent: nothing left to migrate.
    assert_eq!(lib.migrate_related_to_relations().unwrap(), 0);
}

#[test]
fn ai_sidecar_write_never_touches_note_or_entry() {
    use fond_bib::AiMetadata;
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["a"]);

    // Give `a` a curated note with a real tag.
    fs::write(lib.note_path("a"), "---\ntags: [curated]\n---\nprose\n").unwrap();

    let mut ai = AiMetadata::new("claude-opus-4-8", "2026-07-24T10:00:00Z");
    ai.keywords = vec!["machine".into()];
    lib.write_ai("a", &ai).unwrap();

    // The AI file exists and round-trips…
    assert_eq!(lib.load_ai("a").unwrap().unwrap().keywords, vec!["machine"]);
    // …and the curated note is completely untouched (no AI keyword leaked into tags).
    let note = lib.load_note("a").unwrap().unwrap();
    assert_eq!(note.frontmatter.tags, vec!["curated"]);
    assert!(note.frontmatter.relations.is_empty());
    // A keyless entry has no AI sidecar.
    assert!(lib.load_ai("nonexistent").unwrap().is_none());
}

#[test]
fn scan_usage_maps_keys_to_projects() {
    use fond_bib::Project;
    let (dir, lib) = temp_library();
    seed_entries(&lib, &["cone1970black", "berdyaev1937destiny"]);

    // A Typst document that cites one of the entries.
    let doc = dir.path().join("ch3.typ");
    fs::write(&doc, "As @cone1970black argues, liberation is central.\n").unwrap();

    lib.save_project(
        "diss",
        &Project {
            name: "Dissertation".into(),
            description: None,
            documents: vec![doc.clone()],
        },
    )
    .unwrap();

    let usage = lib.write_usage().unwrap();
    let uses = usage
        .by_key
        .get("cone1970black")
        .expect("cited key present");
    assert_eq!(uses.len(), 1);
    assert_eq!(uses[0].0, "diss");
    // The uncited entry has no usage.
    assert!(!usage.by_key.contains_key("berdyaev1937destiny"));
    // Derived file was written under .kartoteka/.
    assert!(dir.path().join(".kartoteka/usage.json").is_file());

    // A dangling document path is flagged by fsck, not fatal.
    lib.save_project(
        "broken",
        &Project {
            name: "Broken".into(),
            description: None,
            documents: vec!["/no/such.typ".into()],
        },
    )
    .unwrap();
    let report = lib.fsck().unwrap();
    assert_eq!(report.dangling_project_docs.len(), 1);
}

#[test]
fn node_round_trips_through_disk() {
    let (_dir, lib) = temp_library();
    let node = Node::parse(
        "---\nnode-type: person\nlabel: Origen of Alexandria\naliases: [Origen]\nidentifiers:\n  wikidata: Q170472\n---\nThird-century theologian.\n",
        std::path::Path::new("nodes/origen.md"),
    )
    .unwrap();

    lib.write_node("origen", &node).unwrap();
    assert!(lib.node_path("origen").is_file());
    assert_eq!(lib.node_slugs().unwrap(), vec!["origen"]);

    let loaded = lib.load_node("origen").unwrap();
    assert_eq!(loaded, node);
    assert_eq!(loaded.frontmatter.node_type, NodeType::Person);
    assert_eq!(loaded.frontmatter.label, "Origen of Alexandria");
}

// ---- M3: relations spanning notes ∪ nodes (docs/M3-SPEC.md §3) ----------------------

#[test]
fn resolve_target_distinguishes_entry_node_and_dangling() {
    use fond_bib::Target;
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["augustine0426city"]);
    seed_node(&lib, "aquinas", "person", "Thomas Aquinas");

    assert_eq!(
        lib.resolve_target("augustine0426city").unwrap(),
        Target::Entry("augustine0426city".into())
    );
    assert_eq!(
        lib.resolve_target("aquinas").unwrap(),
        Target::Node("aquinas".into())
    );
    assert_eq!(
        lib.resolve_target("nobody").unwrap(),
        Target::Dangling("nobody".into())
    );
}

#[test]
fn forward_edge_on_entry_maintains_inverse_on_node() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["cone1970black"]);
    seed_node(&lib, "black-theology", "concept", "Black Theology");

    // An entry is `about` a concept node ⇒ the node gains the `discussed-in` inverse.
    lib.add_relation("cone1970black", Predicate::About, "black-theology")
        .unwrap();
    assert_eq!(
        edges(&lib, "cone1970black"),
        vec![(Predicate::About, "black-theology".into(), false)]
    );
    assert_eq!(
        edges(&lib, "black-theology"),
        vec![(Predicate::DiscussedIn, "cone1970black".into(), true)]
    );
    // The inverse really landed in the node file, and the node still parses.
    let node = lib.load_node("black-theology").unwrap();
    assert_eq!(node.frontmatter.label, "Black Theology");
    assert_eq!(node.frontmatter.relations.len(), 1);

    // Reconciliation sees nothing to fix (the universe spans notes ∪ nodes).
    assert!(lib.reconcile_relations(false).unwrap().is_clean());

    // Removing the forward edge removes the inverse from the node too.
    lib.remove_relation("cone1970black", Predicate::About, "black-theology")
        .unwrap();
    assert!(edges(&lib, "black-theology").is_empty());
}

#[test]
fn forward_edge_on_node_maintains_inverse_on_entry() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["augustine0426city"]);
    seed_node(&lib, "augustine", "person", "Augustine of Hippo");

    // A person node `authored` a work entry ⇒ the entry gains the `authored-by` inverse.
    lib.set_relations(
        "augustine",
        &[Relation::forward(Predicate::Authored, "augustine0426city")],
    )
    .unwrap();
    assert_eq!(
        edges(&lib, "augustine"),
        vec![(Predicate::Authored, "augustine0426city".into(), false)]
    );
    assert_eq!(
        edges(&lib, "augustine0426city"),
        vec![(Predicate::AuthoredBy, "augustine".into(), true)]
    );
    assert!(lib.reconcile_relations(false).unwrap().is_clean());
}

#[test]
fn reconcile_repairs_a_desynced_node_inverse_edge() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["augustine0426city"]);
    seed_node(&lib, "augustine", "person", "Augustine of Hippo");

    // Hand-write a forward edge on the node without its maintained counterpart.
    fs::write(
        lib.node_path("augustine"),
        "---\nnode-type: person\nlabel: Augustine of Hippo\nrelations:\n  - predicate: authored\n    target: augustine0426city\n---\n",
    )
    .unwrap();

    // Report finds the missing inverse; fix materializes it on the entry.
    let before = lib.reconcile_relations(false).unwrap();
    assert_eq!(before.missing.len(), 1, "{before:?}");
    lib.reconcile_relations(true).unwrap();
    assert_eq!(
        edges(&lib, "augustine0426city"),
        vec![(Predicate::AuthoredBy, "augustine".into(), true)]
    );
    // Idempotent: a second fix finds nothing.
    assert!(lib.reconcile_relations(true).unwrap().is_clean());
}

#[test]
fn reconcile_flags_target_that_is_neither_entry_nor_node() {
    let (_dir, lib) = temp_library();
    seed_node(&lib, "augustine", "person", "Augustine of Hippo");

    // A node forward edge pointing at something that resolves to neither entry nor node.
    fs::write(
        lib.node_path("augustine"),
        "---\nnode-type: person\nlabel: Augustine of Hippo\nrelations:\n  - predicate: influenced\n    target: ghost\n---\n",
    )
    .unwrap();

    let report = lib.reconcile_relations(false).unwrap();
    assert_eq!(report.dangling_targets.len(), 1);
    assert_eq!(report.dangling_targets[0].0, "augustine");
    assert_eq!(report.dangling_targets[0].2, "ghost");
}

#[test]
fn fsck_clean_with_valid_nodes() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["augustine0426city"]);
    seed_node(&lib, "augustine", "person", "Augustine of Hippo");
    lib.set_relations(
        "augustine",
        &[Relation::forward(Predicate::Authored, "augustine0426city")],
    )
    .unwrap();
    let report = lib.fsck().unwrap();
    assert!(report.is_clean(), "expected clean, got {report:?}");
}

#[test]
fn fsck_flags_unparseable_node() {
    let (_dir, lib) = temp_library();
    // A node file with no frontmatter (and thus no label) is a parse error.
    fs::write(lib.node_path("broken"), "just prose, no frontmatter\n").unwrap();
    let report = lib.fsck().unwrap();
    assert_eq!(report.unparseable_nodes.len(), 1, "{report:?}");
    assert!(report.unparseable_nodes[0].0.contains("broken.md"));
}

#[test]
fn fsck_flags_malformed_node_slug() {
    let (_dir, lib) = temp_library();
    // A hand-created file whose name isn't a valid slug (spaces + uppercase).
    fs::write(
        lib.node_path("Augustine Of Hippo"),
        "---\nnode-type: person\nlabel: Augustine of Hippo\n---\n",
    )
    .unwrap();
    let report = lib.fsck().unwrap();
    assert_eq!(report.malformed_node_slugs, vec!["Augustine Of Hippo"]);
    // The file itself parses fine — only its name is the problem.
    assert!(report.unparseable_nodes.is_empty());
}

// ---- delete_entry -------------------------------------------------------------------

#[test]
fn delete_entry_removes_files_relations_and_collection_membership() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["a", "b"]);

    // A relates to B (symmetric): B's note gains a maintained edge back to A.
    lib.add_relation("a", Predicate::Related, "b").unwrap();
    assert!(edges(&lib, "b").iter().any(|(_, t, _)| t == "a"));

    // A sits in a collection alongside B.
    lib.save_collection(
        "reading",
        &fond_bib::Collection {
            name: "Reading".into(),
            description: None,
            parent: None,
            keys: vec!["a".into(), "b".into()],
        },
    )
    .unwrap();

    let report = lib.delete_entry("a").unwrap();

    // The entry file (and its auto-created note) are gone.
    assert!(!lib.entry_path("a").exists());
    assert!(!lib.note_path("a").exists());
    assert!(report.files_removed.iter().any(|p| p.ends_with("a.yml")));

    // B no longer has any edge pointing at the deleted key.
    assert!(!edges(&lib, "b").iter().any(|(_, t, _)| t == "a"));
    assert_eq!(report.relations_cleared, 1, "{report:?}");

    // The collection dropped A but kept B.
    assert_eq!(lib.load_collection("reading").unwrap().keys, vec!["b"]);
    assert_eq!(report.collections_updated, vec!["reading"]);

    // Deleting again is a harmless no-op.
    let again = lib.delete_entry("a").unwrap();
    assert!(again.files_removed.is_empty());
}

// ---- add_to_collection / remove_from_collection --------------------------------------

#[test]
fn add_to_collection_is_idempotent() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["a"]);
    lib.save_collection(
        "reading",
        &fond_bib::Collection {
            name: "Reading".into(),
            ..Default::default()
        },
    )
    .unwrap();

    lib.add_to_collection("reading", "a").unwrap();
    lib.add_to_collection("reading", "a").unwrap();
    assert_eq!(lib.load_collection("reading").unwrap().keys, vec!["a"]);

    lib.remove_from_collection("reading", "a").unwrap();
    lib.remove_from_collection("reading", "a").unwrap();
    assert!(lib.load_collection("reading").unwrap().keys.is_empty());
}

// ---- create_collection / rename_collection / reparent_collection / delete_collection -----

#[test]
fn create_collection_dedupes_colliding_slugs() {
    let (_dir, lib) = temp_library();
    let first = lib.create_collection("Theology", None).unwrap();
    let second = lib.create_collection("theology!!", None).unwrap();
    assert_eq!(first, "theology");
    assert_ne!(second, "theology");
    // Both collections survive as separate files — the second didn't overwrite the first.
    assert_eq!(lib.load_collection(&first).unwrap().name, "Theology");
    assert_eq!(lib.load_collection(&second).unwrap().name, "theology!!");
}

#[test]
fn rename_collection_keeps_slug_and_parent_references() {
    let (_dir, lib) = temp_library();
    let parent = lib.create_collection("Theology", None).unwrap();
    let child = lib.create_collection("Christology", Some(&parent)).unwrap();

    lib.rename_collection(&parent, "Systematic Theology")
        .unwrap();

    assert_eq!(
        lib.load_collection(&parent).unwrap().name,
        "Systematic Theology"
    );
    // The child's parent reference is a slug, untouched by the rename.
    assert_eq!(
        lib.load_collection(&child).unwrap().parent.as_deref(),
        Some(parent.as_str())
    );
}

#[test]
fn reparent_collection_rejects_cycles() {
    let (_dir, lib) = temp_library();
    let a = lib.create_collection("A", None).unwrap();
    let b = lib.create_collection("B", Some(&a)).unwrap();
    let c = lib.create_collection("C", Some(&b)).unwrap();

    // A is not its own parent.
    assert!(lib.reparent_collection(&a, Some(a.as_str())).is_err());
    // A -> C would close the A -> B -> C -> A loop.
    assert!(lib.reparent_collection(&a, Some(&c)).is_err());
    assert_eq!(lib.load_collection(&a).unwrap().parent, None);

    // Moving C under A directly (skipping B) is fine — no cycle.
    lib.reparent_collection(&c, Some(&a)).unwrap();
    assert_eq!(
        lib.load_collection(&c).unwrap().parent.as_deref(),
        Some(a.as_str())
    );

    // Moving back to top level always succeeds.
    lib.reparent_collection(&b, None).unwrap();
    assert_eq!(lib.load_collection(&b).unwrap().parent, None);
}

#[test]
fn delete_collection_promotes_children_to_top_level() {
    let (_dir, lib) = temp_library();
    let parent = lib.create_collection("Theology", None).unwrap();
    let child = lib.create_collection("Christology", Some(&parent)).unwrap();

    lib.delete_collection(&parent).unwrap();

    assert!(lib.load_collection(&parent).is_err());
    assert_eq!(lib.load_collection(&child).unwrap().parent, None);
}

#[test]
fn delete_node_removes_file_and_incoming_relations() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["a"]);
    let node = Node::parse(
        "---\nnode-type: person\nlabel: Origen of Alexandria\n---\nThird-century theologian.\n",
        std::path::Path::new("nodes/origen.md"),
    )
    .unwrap();
    lib.write_node("origen", &node).unwrap();

    // Entry A relates to the node (authored) — the node gains a maintained inverse edge.
    lib.add_relation("a", Predicate::Authored, "origen")
        .unwrap();
    assert!(edges(&lib, "a").iter().any(|(_, t, _)| t == "origen"));

    let report = lib.delete_node("origen").unwrap();

    assert!(!lib.node_path("origen").exists());
    assert!(report
        .files_removed
        .iter()
        .any(|p| p.ends_with("origen.md")));
    assert!(report.collections_updated.is_empty());
    assert!(report.blobs_removed.is_empty());

    // A no longer has any edge pointing at the deleted node.
    assert!(!edges(&lib, "a").iter().any(|(_, t, _)| t == "origen"));
    assert_eq!(report.relations_cleared, 1, "{report:?}");

    // Deleting again is a harmless no-op.
    let again = lib.delete_node("origen").unwrap();
    assert!(again.files_removed.is_empty());
}

#[test]
fn delete_entry_gcs_unshared_blob_but_keeps_shared_one() {
    let (dir, lib) = temp_library();
    seed_entries(&lib, &["shared_a", "shared_b", "solo"]);

    // One file attached to two entries → one shared blob.
    let shared_src = dir.path().join("shared.pdf");
    fs::write(&shared_src, b"shared bytes").unwrap();
    let att = lib.store_attachment("shared_a", &shared_src, None).unwrap();
    lib.store_attachment("shared_b", &shared_src, None).unwrap();
    let hex = att.hash.rsplit(':').next().unwrap().to_string();
    assert!(lib.attachment_blob_path(&hex).exists());

    // A different file attached only to `solo` → its own blob.
    let solo_src = dir.path().join("solo.pdf");
    fs::write(&solo_src, b"solo bytes").unwrap();
    let solo_att = lib.store_attachment("solo", &solo_src, None).unwrap();
    let solo_hex = solo_att.hash.rsplit(':').next().unwrap().to_string();

    // Deleting one sharer keeps the shared blob (still referenced by the other).
    lib.delete_entry("shared_a").unwrap();
    assert!(
        lib.attachment_blob_path(&hex).exists(),
        "shared blob GC'd too early"
    );

    // Deleting the sole referencer GCs its blob.
    let report = lib.delete_entry("solo").unwrap();
    assert!(
        !lib.attachment_blob_path(&solo_hex).exists(),
        "solo blob not GC'd"
    );
    assert!(report.blobs_removed.iter().any(|p| p.ends_with(&solo_hex)));
}

// ---- edit_fields (structured citation editor) ---------------------------------------

#[test]
fn edit_fields_updates_managed_fields_and_preserves_others() {
    use fond_bib::entry::{self, EntryFields};
    use fond_bib::{Creator, CreatorRole};
    let (_dir, lib) = temp_library();

    // An entry with fields the form doesn't manage (edition, language) plus a publisher.
    fs::write(
        lib.entry_path("work"),
        "work:\n  type: book\n  title: Old Title\n  author:\n  - Doe, Jane\n  date: 1990\n  \
         publisher: Old Press\n  edition: 2\n  language: en\n",
    )
    .unwrap();

    let current = entry::read_fields(&lib.load_entry("work").unwrap().entry);
    assert_eq!(current.title, "Old Title");
    assert_eq!(
        current.creators,
        vec![Creator::new(CreatorRole::Author, "Doe", "Jane")]
    );
    assert_eq!(current.year, "1990");
    assert_eq!(current.publisher, "Old Press");

    // Edit title, add a second author, change the year, set a DOI — leave publisher untouched.
    let edited = EntryFields {
        title: "New Title".into(),
        creators: vec![
            Creator::new(CreatorRole::Author, "Doe", "Jane"),
            Creator::new(CreatorRole::Author, "Roe", "Richard"),
        ],
        year: "1991".into(),
        doi: "10.1000/new".into(),
        ..current.clone()
    };
    lib.edit_fields("work", &edited).unwrap();

    let after = lib.load_entry("work").unwrap().entry;
    let f = entry::read_fields(&after);
    assert_eq!(f.title, "New Title");
    assert_eq!(
        f.creators,
        vec![
            Creator::new(CreatorRole::Author, "Doe", "Jane"),
            Creator::new(CreatorRole::Author, "Roe", "Richard"),
        ]
    );
    assert_eq!(f.year, "1991");
    assert_eq!(f.doi, "10.1000/new");
    assert_eq!(f.publisher, "Old Press", "untouched publisher must survive");

    // Fields the form never exposes must still be on disk.
    let raw = fs::read_to_string(lib.entry_path("work")).unwrap();
    assert!(raw.contains("edition:"), "exotic field dropped: {raw}");
    assert!(raw.contains("language: en"), "exotic field dropped: {raw}");
}

#[test]
fn edit_fields_clearing_a_field_removes_it() {
    use fond_bib::entry::{self, EntryFields};
    let (_dir, lib) = temp_library();
    fs::write(
        lib.entry_path("work"),
        "work:\n  type: book\n  title: T\n  date: 2000\n  publisher: P\n",
    )
    .unwrap();

    let current = entry::read_fields(&lib.load_entry("work").unwrap().entry);
    let edited = EntryFields {
        publisher: String::new(), // clear it
        ..current
    };
    lib.edit_fields("work", &edited).unwrap();

    let raw = fs::read_to_string(lib.entry_path("work")).unwrap();
    assert!(
        !raw.contains("publisher"),
        "cleared field still present: {raw}"
    );
    assert!(raw.contains("date: 2000"), "unrelated field lost: {raw}");
}

/// Location must be written nested under `publisher:`, not as the entry's own top-level
/// `location:` field — that's the shape most citation styles' "place of publication"
/// element (CSL `publisher-place`, Zotero's "Place") actually reads (see
/// `EntryFields::location`'s doc comment). Regression test for a real bug: an earlier
/// version of `apply_fields_to_yaml` wrote a top-level field that no style could ever read.
#[test]
fn edit_fields_writes_location_nested_under_publisher() {
    use fond_bib::entry::{self, EntryFields};
    let (_dir, lib) = temp_library();
    fs::write(
        lib.entry_path("work"),
        "work:\n  type: book\n  title: T\n  date: 2000\n",
    )
    .unwrap();

    let current = entry::read_fields(&lib.load_entry("work").unwrap().entry);
    let edited = EntryFields {
        publisher: "Test Press".into(),
        location: "Testville".into(),
        ..current
    };
    lib.edit_fields("work", &edited).unwrap();

    let raw = fs::read_to_string(lib.entry_path("work")).unwrap();
    assert!(raw.contains("name: Test Press"), "got: {raw}");
    assert!(raw.contains("location: Testville"), "got: {raw}");
    assert!(
        !raw.lines().any(|l| l == "  location: Testville"),
        "location must not be a top-level field: {raw}"
    );

    let after = entry::read_fields(&lib.load_entry("work").unwrap().entry);
    assert_eq!(after.publisher, "Test Press");
    assert_eq!(after.location, "Testville");
}

/// An entry from before this was fixed has `location:` at the top level. It must still read
/// back into the editor (not silently appear blank, discarding data the user already
/// entered), and saving any publisher/location change must migrate it into the nested spot
/// and remove the stale top-level key rather than leaving both around.
#[test]
fn edit_fields_migrates_legacy_top_level_location() {
    use fond_bib::entry::{self, EntryFields};
    let (_dir, lib) = temp_library();
    fs::write(
        lib.entry_path("work"),
        "work:\n  type: book\n  title: T\n  date: 2000\n  publisher: Old Press\n  location: Oldville\n",
    )
    .unwrap();

    let current = entry::read_fields(&lib.load_entry("work").unwrap().entry);
    assert_eq!(
        current.location, "Oldville",
        "legacy top-level location must still populate the editor"
    );

    // Re-save with the location edited (the common real-world trigger: the user notices and
    // corrects/confirms it) — this must migrate the field, not just leave the old one in place.
    let edited = EntryFields {
        location: "Newville".into(),
        ..current
    };
    lib.edit_fields("work", &edited).unwrap();

    let raw = fs::read_to_string(lib.entry_path("work")).unwrap();
    assert!(raw.contains("location: Newville"), "got: {raw}");
    assert!(
        !raw.lines()
            .any(|l| l == "  location: Newville" || l == "  location: Oldville"),
        "stale top-level location key must be removed, not left alongside the nested one: {raw}"
    );
}

#[test]
fn edit_fields_writes_multiple_creator_types_to_separate_yaml_keys() {
    use fond_bib::entry::{self, EntryFields};
    use fond_bib::{Creator, CreatorRole};
    let (_dir, lib) = temp_library();
    fs::write(
        lib.entry_path("work"),
        "work:\n  type: book\n  title: T\n  date: 2000\n",
    )
    .unwrap();

    // Grouped author/editor/affiliated-contiguous, matching the order `parse_creators` will
    // read back (all authors, then all editors, then each affiliated role) — cross-type
    // interleaving isn't preserved across a save/reload (see `creator::parse_creators` docs),
    // so this is the input shape that actually round-trips byte-for-byte.
    let current = entry::read_fields(&lib.load_entry("work").unwrap().entry);
    let edited = EntryFields {
        creators: vec![
            Creator::new(CreatorRole::Author, "Doe", "Jane"),
            Creator::new_single_field(CreatorRole::Author, "UNESCO"),
            Creator::new(CreatorRole::Editor, "Roe", "Rick"),
            Creator::new(CreatorRole::Translator, "Lee", "Desmond"),
        ],
        ..current
    };
    lib.edit_fields("work", &edited).unwrap();

    let raw = fs::read_to_string(lib.entry_path("work")).unwrap();
    assert!(raw.contains("author:"), "got: {raw}");
    assert!(raw.contains("editor:"), "got: {raw}");
    assert!(raw.contains("affiliated:"), "got: {raw}");
    assert!(raw.contains("role: translator"), "got: {raw}");
    assert!(raw.contains("UNESCO"), "single-field org name lost: {raw}");

    // Round-trips back through the structured editor unchanged.
    let after = lib.load_entry("work").unwrap().entry;
    let f = entry::read_fields(&after);
    assert_eq!(f.creators, edited.creators);

    // Sort/citation key still prefers the plain author over the editor/translator.
    assert_eq!(entry::family_name(&after).as_deref(), Some("Doe"));
}

#[test]
fn family_name_falls_back_to_editor_for_an_editor_only_entry() {
    let (_dir, lib) = temp_library();
    let yaml = "_:\n  type: book\n  title: Essays\n  editor:\n    - Roe, Rick\n";
    let keys = lib.add_from_yaml(yaml).unwrap();
    let entry = lib.load_entry(&keys[0]).unwrap().entry;
    assert_eq!(
        fond_bib::entry::family_name(&entry).as_deref(),
        Some("Roe"),
        "an edited volume with no plain author should still sort/key under its editor"
    );
}

// --- Child and standalone notes (docs/NOTES-SPEC.md Tier 1) ---

#[test]
fn create_and_load_child_note_round_trips() {
    let (_dir, lib) = temp_library();
    lib.add_from_yaml(BERDYAEV).unwrap();

    let id = lib
        .create_child_note("berdyaev1937destiny", "A loose thought about chapter 3.")
        .unwrap();
    let loaded = lib.load_child_note("berdyaev1937destiny", &id).unwrap();
    assert_eq!(loaded.body, "A loose thought about chapter 3.");
    assert_eq!(lib.child_note_ids("berdyaev1937destiny").unwrap(), vec![id]);
}

#[test]
fn child_note_ids_empty_when_no_directory() {
    let (_dir, lib) = temp_library();
    lib.add_from_yaml(BERDYAEV).unwrap();
    assert!(lib
        .child_note_ids("berdyaev1937destiny")
        .unwrap()
        .is_empty());
}

#[test]
fn deleting_last_child_note_removes_the_directory() {
    let (_dir, lib) = temp_library();
    lib.add_from_yaml(BERDYAEV).unwrap();
    let id = lib
        .create_child_note("berdyaev1937destiny", "Only note.")
        .unwrap();
    lib.delete_child_note("berdyaev1937destiny", &id).unwrap();
    assert!(!lib.child_note_dir("berdyaev1937destiny").exists());
}

#[test]
fn deleting_one_of_several_child_notes_keeps_the_others() {
    let (_dir, lib) = temp_library();
    lib.add_from_yaml(BERDYAEV).unwrap();
    let id1 = lib
        .create_child_note("berdyaev1937destiny", "First.")
        .unwrap();
    let id2 = lib
        .create_child_note("berdyaev1937destiny", "Second.")
        .unwrap();
    lib.delete_child_note("berdyaev1937destiny", &id1).unwrap();
    assert_eq!(
        lib.child_note_ids("berdyaev1937destiny").unwrap(),
        vec![id2]
    );
}

#[test]
fn create_and_load_standalone_note_round_trips() {
    let (_dir, lib) = temp_library();
    let id = lib
        .create_standalone_note("An idea not tied to any one source.")
        .unwrap();
    let loaded = lib.load_standalone_note(&id).unwrap();
    assert_eq!(loaded.body, "An idea not tied to any one source.");
    assert_eq!(lib.standalone_note_ids().unwrap(), vec![id]);
}

#[test]
fn delete_standalone_note_removes_it() {
    let (_dir, lib) = temp_library();
    let id = lib.create_standalone_note("Temporary.").unwrap();
    lib.delete_standalone_note(&id).unwrap();
    assert!(lib.standalone_note_ids().unwrap().is_empty());
}

#[test]
fn fsck_flags_orphaned_child_note_dir_and_malformed_and_unparseable_notes() {
    let (_dir, lib) = temp_library();
    lib.add_from_yaml(BERDYAEV).unwrap();

    // A child note for a key that doesn't exist.
    fs::create_dir_all(lib.child_note_dir("doesnotexist")).unwrap();
    fs::write(
        lib.child_note_path("doesnotexist", "2026-09-06-abcd1234"),
        "orphaned",
    )
    .unwrap();

    // A malformed note id (not a `.md` stem `fsck` should trust).
    fs::create_dir_all(lib.child_note_dir("berdyaev1937destiny")).unwrap();
    fs::write(
        lib.child_note_path("berdyaev1937destiny", "not-a-note-id"),
        "body",
    )
    .unwrap();

    // An unparseable standalone note: malformed YAML frontmatter.
    fs::write(
        lib.standalone_note_path("2026-09-06-deadbeef"),
        "---\ntags: [unterminated\n---\nbody\n",
    )
    .unwrap();

    let report = lib.fsck().unwrap();
    assert_eq!(report.orphaned_child_note_dirs, vec!["doesnotexist"]);
    assert_eq!(report.malformed_note_ids.len(), 1);
    assert!(report.malformed_note_ids[0].contains("not-a-note-id"));
    assert_eq!(report.unparseable_notes.len(), 1);
    assert!(!report.is_clean());
}

#[test]
fn store_attachment_renames_using_citation_info() {
    let (dir, lib) = temp_library();
    lib.add_from_yaml(BERDYAEV).unwrap();

    let src = dir.path().join("libgen_scan_293847.pdf");
    fs::write(&src, b"pdf bytes").unwrap();
    let att = lib
        .store_attachment("berdyaev1937destiny", &src, None)
        .unwrap();

    assert_eq!(att.filename, "Berdyaev 1937 - The Destiny of Man.pdf");
    let note = lib.load_note("berdyaev1937destiny").unwrap().unwrap();
    assert_eq!(
        note.frontmatter.attachments[0].filename,
        "Berdyaev 1937 - The Destiny of Man.pdf"
    );
}

#[test]
fn store_attachment_falls_back_to_source_name_without_citation_info() {
    let (dir, lib) = temp_library();
    // No entries/*.yml at all for this key — an attachment stored before identification.
    let src = dir.path().join("libgen_scan_293847.pdf");
    fs::write(&src, b"pdf bytes").unwrap();

    let att = lib.store_attachment("unidentified", &src, None).unwrap();
    assert_eq!(att.filename, "libgen_scan_293847.pdf");
}

#[test]
fn store_attachment_dedupes_a_second_attachment_with_the_same_citation_name() {
    let (dir, lib) = temp_library();
    lib.add_from_yaml(BERDYAEV).unwrap();

    let preprint = dir.path().join("preprint.pdf");
    fs::write(&preprint, b"preprint bytes").unwrap();
    let published = dir.path().join("published.pdf");
    fs::write(&published, b"published bytes").unwrap();

    let a1 = lib
        .store_attachment("berdyaev1937destiny", &preprint, None)
        .unwrap();
    let a2 = lib
        .store_attachment("berdyaev1937destiny", &published, None)
        .unwrap();

    assert_eq!(a1.filename, "Berdyaev 1937 - The Destiny of Man.pdf");
    assert_eq!(a2.filename, "Berdyaev 1937 - The Destiny of Man (2).pdf");
}

#[test]
fn store_attachment_re_storing_identical_bytes_keeps_the_existing_record() {
    let (dir, lib) = temp_library();
    lib.add_from_yaml(BERDYAEV).unwrap();

    let src = dir.path().join("scan.pdf");
    fs::write(&src, b"same bytes").unwrap();
    let first = lib
        .store_attachment("berdyaev1937destiny", &src, None)
        .unwrap();
    let second = lib
        .store_attachment("berdyaev1937destiny", &src, None)
        .unwrap();

    assert_eq!(first, second);
    let note = lib.load_note("berdyaev1937destiny").unwrap().unwrap();
    assert_eq!(note.frontmatter.attachments.len(), 1);
}

#[test]
fn rename_attachments_backfills_old_names_to_citation_scheme() {
    let (dir, lib) = temp_library();
    lib.add_from_yaml(BERDYAEV).unwrap();

    // Simulate an attachment stored before this naming existed: a note with an
    // old-style filename but a real blob behind it.
    let src = dir.path().join("scan.pdf");
    fs::write(&src, b"pdf bytes").unwrap();
    let mut att = lib
        .store_attachment("berdyaev1937destiny", &src, None)
        .unwrap();
    att.filename = "libgen_scan_293847.pdf".to_string();
    let mut note = lib.load_note("berdyaev1937destiny").unwrap().unwrap();
    note.frontmatter.attachments = vec![att];
    lib.write_note("berdyaev1937destiny", &note).unwrap();

    let renamed = lib
        .rename_attachments_to_citation_names("berdyaev1937destiny", false)
        .unwrap();
    assert_eq!(
        renamed,
        vec![(
            "libgen_scan_293847.pdf".to_string(),
            "Berdyaev 1937 - The Destiny of Man.pdf".to_string()
        )]
    );

    let note = lib.load_note("berdyaev1937destiny").unwrap().unwrap();
    assert_eq!(
        note.frontmatter.attachments[0].filename,
        "Berdyaev 1937 - The Destiny of Man.pdf"
    );

    // Running again is a no-op: nothing left to rename.
    let again = lib
        .rename_attachments_to_citation_names("berdyaev1937destiny", false)
        .unwrap();
    assert!(again.is_empty());
}

#[test]
fn rename_attachments_dry_run_reports_without_writing() {
    let (dir, lib) = temp_library();
    lib.add_from_yaml(BERDYAEV).unwrap();

    let src = dir.path().join("scan.pdf");
    fs::write(&src, b"pdf bytes").unwrap();
    let mut att = lib
        .store_attachment("berdyaev1937destiny", &src, None)
        .unwrap();
    att.filename = "libgen_scan_293847.pdf".to_string();
    let mut note = lib.load_note("berdyaev1937destiny").unwrap().unwrap();
    note.frontmatter.attachments = vec![att];
    lib.write_note("berdyaev1937destiny", &note).unwrap();

    let renamed = lib
        .rename_attachments_to_citation_names("berdyaev1937destiny", true)
        .unwrap();
    assert_eq!(renamed.len(), 1);

    // Dry run must not have touched the note.
    let note = lib.load_note("berdyaev1937destiny").unwrap().unwrap();
    assert_eq!(
        note.frontmatter.attachments[0].filename,
        "libgen_scan_293847.pdf"
    );
}

#[test]
fn rename_attachments_leaves_unidentified_entries_alone() {
    let (dir, lib) = temp_library();
    // No entries/*.yml — attachment stored before identification.
    let src = dir.path().join("scan.pdf");
    fs::write(&src, b"pdf bytes").unwrap();
    lib.store_attachment("unidentified", &src, None).unwrap();

    let renamed = lib
        .rename_attachments_to_citation_names("unidentified", false)
        .unwrap();
    assert!(renamed.is_empty());
}

#[test]
fn deleting_entry_removes_its_child_notes() {
    let (_dir, lib) = temp_library();
    lib.add_from_yaml(BERDYAEV).unwrap();
    lib.create_child_note("berdyaev1937destiny", "A note.")
        .unwrap();
    assert!(lib.child_note_dir("berdyaev1937destiny").exists());

    lib.delete_entry("berdyaev1937destiny").unwrap();
    assert!(!lib.child_note_dir("berdyaev1937destiny").exists());
    // And fsck no longer sees it as orphaned, since the whole directory is gone.
    let report = lib.fsck().unwrap();
    assert!(report.orphaned_child_note_dirs.is_empty());
}

/// Notes are rewritten constantly (relations, attachments, tags, merges); frontmatter keys the
/// model doesn't know used to vanish on the first rewrite, and `custom-fields` came out in a
/// different random order every time (noisy git diffs).
#[test]
fn rewriting_a_note_keeps_unknown_keys_and_orders_custom_fields_stably() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["a", "b"]);
    fs::write(
        lib.note_path("a"),
        "---\nsource: my reading group\nzeta: [1, 2]\ncustom-fields:\n  z: 1\n  b: 2\n  m: 3\n  a: 4\n---\nbody\n",
    )
    .unwrap();
    lib.add_relation("a", Predicate::Cites, "b").unwrap();
    let raw = fs::read_to_string(lib.note_path("a")).unwrap();
    assert!(
        raw.contains("source: my reading group"),
        "unknown key dropped: {raw}"
    );
    assert!(raw.contains("zeta:"), "unknown key dropped: {raw}");
    let pos = |k: &str| raw.find(&format!("  {k}:")).unwrap();
    assert!(
        pos("a") < pos("b") && pos("b") < pos("m") && pos("m") < pos("z"),
        "custom-fields unsorted: {raw}"
    );
}

#[test]
fn rewriting_a_node_keeps_unknown_keys() {
    let (_dir, lib) = temp_library();
    fs::write(
        lib.node_path("augustine"),
        "---\nnode-type: person\nlabel: Augustine\nborn: 354\n---\n",
    )
    .unwrap();
    let node = lib.load_node("augustine").unwrap();
    lib.write_node("augustine", &node).unwrap();
    assert!(fs::read_to_string(lib.node_path("augustine"))
        .unwrap()
        .contains("born: 354"));
}

/// A batch with an unkeyable entry must write nothing, not the entries before it.
#[test]
fn add_entries_batch_is_all_or_nothing_on_a_key_failure() {
    let (_dir, lib) = temp_library();
    let good = "g:\n  type: book\n  title: Fine Title\n  author: Doe, Jane\n  date: 2000\n";
    let unkeyable = "u:\n  type: misc\n  date: 2001\n"; // no author, no title
    let mut entries = fond_bib::entry::parse_all(good, std::path::Path::new("x")).unwrap();
    entries.extend(fond_bib::entry::parse_all(unkeyable, std::path::Path::new("x")).unwrap());
    assert!(lib.add_entries(&entries).is_err());
    assert!(
        lib.existing_keys().unwrap().is_empty(),
        "first entry written despite the batch failing"
    );
}
