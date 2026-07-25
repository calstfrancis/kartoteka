//! Search index tests: build from a library, then query by field and free text. Includes
//! the rebuild-from-scratch guarantee.

use std::fs;

use fond_bib::library::Library;
use fond_index::SearchIndex;

fn seed() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::init(dir.path()).unwrap();
    fs::write(
        lib.entry_path("cone1970black"),
        "cone1970black:\n  type: article\n  title: Black Theology and Black Power\n  author: Cone, James H.\n  date: 1970\n",
    )
    .unwrap();
    fs::write(
        lib.entry_path("berdyaev1937destiny"),
        "berdyaev1937destiny:\n  type: book\n  title: The Destiny of Man\n  author: Berdyaev, Nikolai\n  date: 1937\n",
    )
    .unwrap();
    fs::write(
        lib.note_path("cone1970black"),
        "---\ntags: [christology, liberation-theology]\n---\nCone reframes the gospel as liberation.\n",
    )
    .unwrap();
    (dir, lib)
}

fn index_dir(lib: &Library) -> std::path::PathBuf {
    lib.root().join(".kartoteka").join("index")
}

#[test]
fn free_text_search_hits_title_and_note() {
    let (_dir, lib) = seed();
    let idx = SearchIndex::rebuild(&lib, &index_dir(&lib), |_| None).unwrap();

    let hits = idx.search("liberation", 10).unwrap();
    assert!(
        hits.iter().any(|h| h.key == "cone1970black"),
        "hits: {hits:?}"
    );

    let hits = idx.search("destiny", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].key, "berdyaev1937destiny");
}

#[test]
fn field_scoped_queries() {
    let (_dir, lib) = seed();
    let idx = SearchIndex::rebuild(&lib, &index_dir(&lib), |_| None).unwrap();

    assert_eq!(
        idx.search("author:cone", 10).unwrap()[0].key,
        "cone1970black"
    );
    assert_eq!(
        idx.search("tag:christology", 10).unwrap()[0].key,
        "cone1970black"
    );
    assert_eq!(
        idx.search("type:book", 10).unwrap()[0].key,
        "berdyaev1937destiny"
    );
    assert_eq!(idx.search("year:1970", 10).unwrap()[0].key, "cone1970black");
    assert!(idx.search("tag:christology", 10).unwrap().len() == 1);
}

#[test]
fn indexes_supplied_pdf_text() {
    let (_dir, lib) = seed();
    let idx = SearchIndex::rebuild(&lib, &index_dir(&lib), |key| {
        (key == "berdyaev1937destiny").then(|| "existential freedom and the person".to_string())
    })
    .unwrap();

    let hits = idx.search("existential", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].key, "berdyaev1937destiny");
}

#[test]
fn rebuild_from_scratch_after_deletion_is_consistent() {
    let (_dir, lib) = seed();
    let dir = index_dir(&lib);
    let idx = SearchIndex::rebuild(&lib, &dir, |_| None).unwrap();
    let before = idx.search("theology", 10).unwrap().len();

    // Nuke the derived index and rebuild — identical results.
    fs::remove_dir_all(&dir).unwrap();
    let idx = SearchIndex::rebuild(&lib, &dir, |_| None).unwrap();
    let after = idx.search("theology", 10).unwrap().len();
    assert_eq!(before, after);
    assert!(after >= 1);

    // And a freshly opened handle over the same dir searches too.
    let reopened = SearchIndex::open(&dir).unwrap();
    assert_eq!(
        reopened.search("author:cone", 10).unwrap()[0].key,
        "cone1970black"
    );
}

#[test]
fn facet_scoping_and_ai_text_are_indexed() {
    use fond_bib::AiMetadata;
    let (_dir, lib) = seed();

    // Give the Cone entry a faceted tag and an AI sidecar.
    fs::write(
        lib.note_path("cone1970black"),
        "---\ntags: [christology, \"discipline:theology/systematic\"]\n---\nprose\n",
    )
    .unwrap();
    let mut ai = AiMetadata::new("claude-opus-4-8", "2026-07-25T00:00:00Z");
    ai.keywords = vec!["soteriology".into()];
    lib.write_ai("cone1970black", &ai).unwrap();

    let idx = SearchIndex::rebuild(&lib, &index_dir(&lib), |_| None).unwrap();

    // `facet:discipline` finds the item carrying that facet.
    assert_eq!(
        idx.search("facet:discipline", 10).unwrap()[0].key,
        "cone1970black"
    );
    // AI keyword is searchable (free text) and scopable via `ai:`.
    assert_eq!(idx.search("soteriology", 10).unwrap()[0].key, "cone1970black");
    assert_eq!(
        idx.search("ai:soteriology", 10).unwrap()[0].key,
        "cone1970black"
    );
    // The berdyaev entry has no facet, so facet scoping excludes it.
    assert!(idx.search("facet:discipline", 10).unwrap().len() == 1);
}

#[test]
fn nodes_are_indexed_and_scopable() {
    let (_dir, lib) = seed();
    // A person node with an alias and an external identifier, plus body prose.
    fs::write(
        lib.node_path("augustine"),
        "---\nnode-type: person\nlabel: Augustine of Hippo\naliases: [Aurelius Augustinus]\nidentifiers:\n  wikidata: Q8018\n---\nBishop of Hippo and Doctor of the Church.\n",
    )
    .unwrap();
    let idx = SearchIndex::rebuild(&lib, &index_dir(&lib), |_| None).unwrap();

    // Free-text on the label finds the node, and the hit is tagged kind=node.
    let hits = idx.search("augustine", 10).unwrap();
    let node_hit = hits.iter().find(|h| h.key == "augustine").expect("node hit");
    assert_eq!(node_hit.kind, "node");
    assert_eq!(node_hit.title, "Augustine of Hippo");
    assert!(node_hit.author.is_empty() && node_hit.year.is_empty());

    // `kind:node` restricts to nodes; entries never carry it.
    let node_only = idx.search("kind:node", 10).unwrap();
    assert_eq!(node_only.len(), 1);
    assert_eq!(node_only[0].key, "augustine");

    // Alias, identifier (scheme or value), body prose, and `type:person` all match.
    assert_eq!(idx.search("Augustinus", 10).unwrap()[0].key, "augustine");
    assert_eq!(idx.search("Q8018", 10).unwrap()[0].key, "augustine");
    assert_eq!(idx.search("wikidata", 10).unwrap()[0].key, "augustine");
    assert_eq!(idx.search("bishop", 10).unwrap()[0].key, "augustine");
    assert_eq!(idx.search("type:person", 10).unwrap()[0].key, "augustine");

    // Entries are still kind=entry and unaffected.
    let entry_hits = idx.search("kind:entry", 10).unwrap();
    assert!(entry_hits.iter().all(|h| h.kind == "entry"));
    assert!(entry_hits.iter().any(|h| h.key == "cone1970black"));
}
