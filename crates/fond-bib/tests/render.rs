//! CSL rendering tests: bundled SBL and archived Chicago-notes styles, and the annotated
//! Typst output.

use std::fs;

use fond_bib::library::Library;
use fond_bib::render::{render_bibliography, resolve_style};
use fond_bib::BufWriteFormat;

const BERDYAEV: &str = "berdyaev1937destiny:\n  type: book\n  title: The Destiny of Man\n  author: Berdyaev, Nikolai\n  date: 1937\n  publisher:\n    name: Geoffrey Bles\n    location: London\n";
const CONE: &str = "cone1970black:\n  type: article\n  title: Black Theology and Black Power\n  author: Cone, James H.\n  date: 1970\n";

fn seed() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::init(dir.path()).unwrap();
    fs::write(lib.entry_path("berdyaev1937destiny"), BERDYAEV).unwrap();
    fs::write(lib.entry_path("cone1970black"), CONE).unwrap();
    fs::write(
        lib.collection_path("theology"),
        "name: Theology\nkeys:\n- berdyaev1937destiny\n- cone1970black\n",
    )
    .unwrap();
    (dir, lib)
}

#[test]
fn resolves_bundled_and_archived_styles() {
    assert!(resolve_style("sbl").is_ok());
    assert!(resolve_style("chicago-notes").is_ok());
    assert!(resolve_style("chicago-author-date").is_ok());
    assert!(resolve_style("no-such-style-xyz").is_err());
}

#[test]
fn renders_sbl_reference_list() {
    let (_dir, lib) = seed();
    let style = resolve_style("sbl").unwrap();
    let rendered = lib
        .bibliography_for_collection("theology", &style, BufWriteFormat::Plain)
        .unwrap();

    assert_eq!(rendered.len(), 2);
    let all: String = rendered
        .iter()
        .map(|r| r.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all.contains("Berdyaev"), "SBL output missing author: {all}");
    assert!(all.contains("Cone"), "SBL output missing author: {all}");
    assert!(
        all.contains("Destiny of Man"),
        "SBL output missing title: {all}"
    );
}

#[test]
fn renders_chicago_notes_reference_list() {
    let (_dir, lib) = seed();
    let style = resolve_style("chicago-notes").unwrap();
    let rendered = lib
        .bibliography_for_collection("theology", &style, BufWriteFormat::Plain)
        .unwrap();
    assert_eq!(rendered.len(), 2);
    let keys: Vec<&str> = rendered.iter().map(|r| r.key.as_str()).collect();
    assert!(keys.contains(&"berdyaev1937destiny"));
    assert!(keys.contains(&"cone1970black"));
}

#[test]
fn direct_render_over_entry_slice() {
    let (_dir, lib) = seed();
    let a = lib.load_entry("berdyaev1937destiny").unwrap().entry;
    let b = lib.load_entry("cone1970black").unwrap().entry;
    let style = resolve_style("chicago-notes").unwrap();
    let rendered = render_bibliography(&[&a, &b], &style, BufWriteFormat::Plain).unwrap();
    assert_eq!(rendered.len(), 2);
}

#[test]
fn annotated_typ_pairs_entries_with_notes_in_collection_order() {
    let (_dir, lib) = seed();
    fs::write(
        lib.note_path("berdyaev1937destiny"),
        "---\ntags: [existentialism]\n---\nBerdyaev's personalist reading of freedom.\n",
    )
    .unwrap();

    let style = resolve_style("sbl").unwrap();
    let typ = lib.annotated_bibliography_typ("theology", &style).unwrap();

    assert!(typ.starts_with("= Annotated Bibliography — Theology"));
    assert!(typ.contains("Berdyaev's personalist reading of freedom."));
    assert!(typ.contains("#block(inset: (left: 1em))["));
    // Collection order: Berdyaev block appears before Cone's rendered entry.
    let berd = typ.find("Berdyaev").unwrap();
    let cone = typ.find("Cone").unwrap();
    assert!(berd < cone, "annotated output not in collection order");
}
