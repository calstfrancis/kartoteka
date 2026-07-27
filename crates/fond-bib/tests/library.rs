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
    for sub in ["entries", "notes", "annots", "collections", "ai", "projects", "nodes"] {
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

#[test]
fn asymmetric_relation_materializes_typed_inverse() {
    let (_dir, lib) = temp_library();
    seed_entries(&lib, &["a", "b"]);

    // a cites b  ⇒  a holds forward `cites`, b holds inverse `cited-by`.
    lib.add_relation("a", Predicate::Cites, "b").unwrap();
    assert_eq!(edges(&lib, "a"), vec![(Predicate::Cites, "b".into(), false)]);
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
    assert_eq!(edges(&lib, "c"), vec![(Predicate::CritiquedBy, "a".into(), true)]);
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
    assert_eq!(edges(&lib, "b"), vec![(Predicate::CitedBy, "a".into(), true)]);

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
    assert_eq!(report.orphaned.len(), 1, "orphaned inverse should be flagged");
    assert!(edges(&lib, "b").is_empty(), "orphaned inverse should be removed");
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
    assert_eq!(edges(&lib, "b"), vec![(Predicate::Related, "a".into(), false)]);
    assert_eq!(edges(&lib, "c"), vec![(Predicate::Related, "a".into(), false)]);

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
    let uses = usage.by_key.get("cone1970black").expect("cited key present");
    assert_eq!(uses.len(), 1);
    assert_eq!(uses[0].0, "diss");
    // The uncited entry has no usage.
    assert!(!usage.by_key.contains_key("berdyaev1937destiny"));
    // Derived file was written under .kartoteka/.
    assert!(dir.path().join(".kartoteka/usage.json").is_file());

    // A dangling document path is flagged by fsck, not fatal.
    lib.save_project(
        "broken",
        &Project { name: "Broken".into(), description: None, documents: vec!["/no/such.typ".into()] },
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
    assert!(lib.attachment_blob_path(&hex).exists(), "shared blob GC'd too early");

    // Deleting the sole referencer GCs its blob.
    let report = lib.delete_entry("solo").unwrap();
    assert!(!lib.attachment_blob_path(&solo_hex).exists(), "solo blob not GC'd");
    assert!(report.blobs_removed.iter().any(|p| p.ends_with(&solo_hex)));
}

// ---- edit_fields (structured citation editor) ---------------------------------------

#[test]
fn edit_fields_updates_managed_fields_and_preserves_others() {
    use fond_bib::entry::{self, EntryFields};
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
    assert_eq!(current.authors, "Doe, Jane");
    assert_eq!(current.year, "1990");
    assert_eq!(current.publisher, "Old Press");

    // Edit title, add a second author, change the year, set a DOI — leave publisher untouched.
    let edited = EntryFields {
        title: "New Title".into(),
        authors: "Doe, Jane\nRoe, Richard".into(),
        year: "1991".into(),
        doi: "10.1000/new".into(),
        ..current.clone()
    };
    lib.edit_fields("work", &edited).unwrap();

    let after = lib.load_entry("work").unwrap().entry;
    let f = entry::read_fields(&after);
    assert_eq!(f.title, "New Title");
    assert_eq!(f.authors, "Doe, Jane\nRoe, Richard");
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
    assert!(!raw.contains("publisher"), "cleared field still present: {raw}");
    assert!(raw.contains("date: 2000"), "unrelated field lost: {raw}");
}
