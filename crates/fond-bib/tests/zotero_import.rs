//! Zotero SQLite augmentation: build a minimal but real Zotero schema, import a `.bib`
//! alongside it, and verify collections and notes land on the right citation keys.

use std::path::Path;

use fond_bib::import::ImportOptions;
use fond_bib::library::Library;
use rusqlite::Connection;

const BIB: &str = "@article{cone1970black,\n  title = {Black Theology and Black Power},\n  author = {Cone, James H.},\n  year = {1970},\n  doi = {10.1/cone}\n}\n\n@book{gutierrez1971teologia,\n  title = {Teolog\\'ia de la liberaci\\'on},\n  author = {Guti\\'errez, Gustavo},\n  date = {1971}\n}\n";

/// Build a minimal Zotero database exercising DOI-only matching (Cone: Zotero title
/// deliberately differs so only the DOI links it) and title+year matching (Gutiérrez).
fn build_zotero_db(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "
        CREATE TABLE itemTypes (itemTypeID INTEGER PRIMARY KEY, typeName TEXT);
        CREATE TABLE items (itemID INTEGER PRIMARY KEY, key TEXT, itemTypeID INTEGER);
        CREATE TABLE fields (fieldID INTEGER PRIMARY KEY, fieldName TEXT);
        CREATE TABLE itemDataValues (valueID INTEGER PRIMARY KEY, value TEXT);
        CREATE TABLE itemData (itemID INTEGER, fieldID INTEGER, valueID INTEGER);
        CREATE TABLE collections (collectionID INTEGER PRIMARY KEY, collectionName TEXT, parentCollectionID INTEGER);
        CREATE TABLE collectionItems (collectionID INTEGER, itemID INTEGER, orderIndex INTEGER);
        CREATE TABLE itemNotes (itemID INTEGER, parentItemID INTEGER, note TEXT);
        CREATE TABLE deletedItems (itemID INTEGER PRIMARY KEY);

        INSERT INTO itemTypes VALUES (1,'book'),(2,'journalArticle'),(3,'note'),(4,'attachment');
        INSERT INTO fields VALUES (1,'title'),(2,'DOI'),(3,'date');

        -- 10: Cone (journalArticle), 11: Gutiérrez (book),
        -- 12: child note on Cone, 13: standalone note, 14: trashed item.
        INSERT INTO items VALUES (10,'ZCONE',2),(11,'ZGUT',1),(12,'ZNOTE',3),(13,'ZSTAND',3),(14,'ZDEAD',1);
        INSERT INTO deletedItems VALUES (14);

        INSERT INTO itemDataValues VALUES
            (1,'A Completely Different Title'),
            (2,'10.1/cone'),
            (3,'1970-00-00 1970'),
            (4,'Teología de la liberación'),
            (5,'1971-00-00 1971'),
            (6,'Ghost Item');

        -- Cone: title(1)=different, DOI(2), date(3)
        INSERT INTO itemData VALUES (10,1,1),(10,2,2),(10,3,3);
        -- Gutiérrez: title(4), date(5) — no DOI
        INSERT INTO itemData VALUES (11,1,4),(11,3,5);
        -- Trashed item has a title but must be ignored
        INSERT INTO itemData VALUES (14,1,6);

        INSERT INTO collections VALUES (100,'Liberation Theology',NULL);
        INSERT INTO collectionItems VALUES (100,10,0),(100,11,1);

        INSERT INTO itemNotes VALUES
            (12,10,'<p>Cone reframes the gospel as <strong>liberation</strong>.</p>'),
            (13,NULL,'<p>A standalone reading-list note.</p>');
        ",
    )
    .unwrap();
}

#[test]
fn augments_import_with_collections_and_notes() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::init(dir.path()).unwrap();

    let db = dir.path().join("zotero.sqlite");
    build_zotero_db(&db);

    let opts = ImportOptions {
        zotero_db: Some(db),
        ..ImportOptions::default()
    };
    let report = lib.import_bibtex(BIB, &opts).unwrap();

    assert_eq!(report.imported.len(), 2);

    // Collection written with both keys, in Zotero order (Cone via DOI, Gutiérrez via title+year).
    assert_eq!(report.collections_created, vec!["liberation-theology"]);
    let coll = lib.load_collection("liberation-theology").unwrap();
    assert_eq!(coll.name, "Liberation Theology");
    assert_eq!(coll.keys, vec!["cone1970black", "gutierrez1971teologia"]);

    // Child note merged into Cone's note prose (HTML → Markdown).
    assert_eq!(report.notes_imported, 1);
    let note = lib.load_note("cone1970black").unwrap().unwrap();
    assert!(
        note.body
            .contains("Cone reframes the gospel as **liberation**."),
        "note body was: {:?}",
        note.body
    );

    // Standalone note could not attach to a key and is reported, not dropped silently.
    assert_eq!(report.standalone_notes_skipped, 1);

    // Everything referenced matched, so no unmatched items.
    assert!(
        report.zotero_unmatched.is_empty(),
        "unmatched: {:?}",
        report.zotero_unmatched
    );
}

#[test]
fn reports_unmatched_collection_items() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::init(dir.path()).unwrap();

    let db = dir.path().join("zotero.sqlite");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "
        CREATE TABLE itemTypes (itemTypeID INTEGER PRIMARY KEY, typeName TEXT);
        CREATE TABLE items (itemID INTEGER PRIMARY KEY, key TEXT, itemTypeID INTEGER);
        CREATE TABLE fields (fieldID INTEGER PRIMARY KEY, fieldName TEXT);
        CREATE TABLE itemDataValues (valueID INTEGER PRIMARY KEY, value TEXT);
        CREATE TABLE itemData (itemID INTEGER, fieldID INTEGER, valueID INTEGER);
        CREATE TABLE collections (collectionID INTEGER PRIMARY KEY, collectionName TEXT, parentCollectionID INTEGER);
        CREATE TABLE collectionItems (collectionID INTEGER, itemID INTEGER, orderIndex INTEGER);
        CREATE TABLE itemNotes (itemID INTEGER, parentItemID INTEGER, note TEXT);
        CREATE TABLE deletedItems (itemID INTEGER PRIMARY KEY);
        INSERT INTO itemTypes VALUES (1,'book');
        INSERT INTO fields VALUES (1,'title');
        INSERT INTO items VALUES (20,'ZX',1);
        INSERT INTO itemDataValues VALUES (1,'An Unimported Book');
        INSERT INTO itemData VALUES (20,1,1);
        INSERT INTO collections VALUES (200,'Orphans',NULL);
        INSERT INTO collectionItems VALUES (200,20,0);
        ",
    )
    .unwrap();

    // Import an unrelated entry so the library is non-empty but nothing matches item 20.
    let report = lib
        .import_bibtex(
            "@book{smith2020faith,\n title={Faith},\n author={Smith, John},\n date={2020}\n}\n",
            &ImportOptions {
                zotero_db: Some(db),
                ..ImportOptions::default()
            },
        )
        .unwrap();

    assert!(
        report.collections_created.is_empty(),
        "empty collection should be skipped"
    );
    assert_eq!(report.zotero_unmatched, vec!["An Unimported Book"]);
    assert!(!report.is_clean());
}

/// Two subcollections with the same name under different parents used to share one slug slot,
/// so both wrote the same file (the first's items were lost) and a child could resolve to the
/// wrong parent. Re-running the import also used to duplicate every child note and overwrite
/// any existing collection with the same slug.
#[test]
fn same_named_subcollections_keep_their_own_items_and_parents_and_reimport_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::init(dir.path()).unwrap();
    let db = dir.path().join("zotero.sqlite");
    build_zotero_db(&db);
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "
            DELETE FROM collections; DELETE FROM collectionItems;
            INSERT INTO collections VALUES (1,'Theology',NULL),(2,'History',NULL),
                                           (3,'To read',1),(4,'To read',2);
            INSERT INTO collectionItems VALUES (3,10,0),(4,11,0);
            ",
        )
        .unwrap();
    }
    let opts = ImportOptions {
        zotero_db: Some(db),
        ..ImportOptions::default()
    };
    let report = lib.import_bibtex(BIB, &opts).unwrap();

    let mut to_read = Vec::new();
    for slug in &report.collections_created {
        let c = lib.load_collection(slug).unwrap();
        if c.name == "To read" {
            to_read.push(c);
        }
    }
    assert_eq!(
        to_read.len(),
        2,
        "one 'To read' was overwritten: {:?}",
        report.collections_created
    );
    let mut all_keys: Vec<String> = to_read.iter().flat_map(|c| c.keys.clone()).collect();
    all_keys.sort();
    assert_eq!(all_keys, vec!["cone1970black", "gutierrez1971teologia"]);
    let parents: std::collections::HashSet<_> = to_read.iter().map(|c| c.parent.clone()).collect();
    assert_eq!(parents.len(), 2, "both got the same parent");

    // Import again: child notes are not appended a second time.
    let before = lib.load_note("cone1970black").unwrap().unwrap().body;
    let _ = lib.import_bibtex(
        BIB,
        &ImportOptions {
            zotero_db: opts.zotero_db.clone(),
            overwrite: true,
            ..ImportOptions::default()
        },
    );
    let after = lib.load_note("cone1970black").unwrap().unwrap().body;
    assert_eq!(
        before.matches("liberation").count(),
        after.matches("liberation").count(),
        "note duplicated on re-import"
    );
}
