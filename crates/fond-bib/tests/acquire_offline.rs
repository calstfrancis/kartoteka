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
