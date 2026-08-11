//! Offline acquisition test: the BibTeX → entry mapping used by `acquire` (the network
//! fetch itself is exercised separately and only when a live-network test is requested).

use fond_bib::library::Library;

#[test]
fn add_bibtex_generates_fresh_keys() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::init(dir.path()).unwrap();

    // A record as doi.org would return it — note the source key is ignored.
    let bibtex = "@article{crossref_generated_key,\n  title = {A Study of Freedom},\n  author = {Doe, Jane},\n  year = {2020},\n  doi = {10.1000/xyz}\n}\n";

    let keys = lib.add_bibtex(bibtex).unwrap();
    assert_eq!(
        keys,
        vec!["doe2020study"],
        "should generate a key, not reuse the source key"
    );
    assert!(lib.entry_path("doe2020study").exists());

    // The DOI survived into the entry.
    let raw = lib.read_entry_raw("doe2020study").unwrap();
    assert!(raw.contains("10.1000/xyz"), "entry: {raw}");
}

#[cfg(feature = "acquire")]
#[test]
fn isbn_json_to_yaml_builds_an_entry_that_adds_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::init(dir.path()).unwrap();

    let json = r#"{
        "ISBN:9780140449136": {
            "title": "The Republic",
            "authors": [{"name": "Plato"}],
            "publish_date": "Oct 01, 2007",
            "publishers": [{"name": "Penguin Classics"}],
            "publish_places": [{"name": "London"}],
            "edition_name": "Revised edition",
            "number_of_pages": 496,
            "identifiers": {"oclc": ["123456"], "lccn": ["2007123456"]}
        }
    }"#;
    let yaml = fond_bib::acquire::isbn_json_to_yaml(json, "9780140449136").unwrap();

    // The real proof: Hayagriva itself accepts every new field (location, note, oclc,
    // lccn, and the fuller date), not just that the YAML text contains the substrings.
    let keys = lib.add_from_yaml(&yaml).unwrap();
    assert_eq!(keys.len(), 1);
    let entry = lib.load_entry(&keys[0]).unwrap().entry;
    let f = fond_bib::entry::read_fields(&entry);
    assert_eq!(f.title, "The Republic");
    assert_eq!(f.year, "2007");
    assert_eq!(f.publisher, "Penguin Classics");
    assert_eq!(f.isbn, "9780140449136");
}

#[cfg(feature = "acquire")]
#[test]
fn book_yaml_builds_multi_author_entry_that_adds_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::init(dir.path()).unwrap();

    let yaml = fond_bib::acquire::book_yaml(
        "Black Theology and Black Power",
        &["Cone, James H.".to_string(), "Doe, Jane".to_string()],
        Some("1969"),
        Some("Seabury Press"),
        Some("9780883441581"),
    )
    .unwrap();

    let keys = lib.add_from_yaml(&yaml).unwrap();
    assert_eq!(keys.len(), 1);
    let entry = lib.load_entry(&keys[0]).unwrap().entry;
    let f = fond_bib::entry::read_fields(&entry);
    assert_eq!(f.title, "Black Theology and Black Power");
    assert_eq!(f.authors, "Cone, James H.\nDoe, Jane");
    assert_eq!(f.year, "1969");
    assert_eq!(f.publisher, "Seabury Press");
    assert_eq!(f.isbn, "9780883441581");
}
