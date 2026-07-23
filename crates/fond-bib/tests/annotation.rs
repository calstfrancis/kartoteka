//! Annotation sidecar round-trip and fsck validation.

use std::fs;

use fond_bib::annotation::{Annotation, AnnotationKind, AnnotationSidecar};
use fond_bib::library::Library;

fn temp_library() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::init(dir.path()).unwrap();
    (dir, lib)
}

const ENTRY: &str = "cone1970black:\n  type: article\n  title: Black Theology and Black Power\n  author: Cone, James H.\n  date: 1970\n";

fn highlight(id: &str) -> Annotation {
    Annotation {
        id: id.into(),
        kind: AnnotationKind::Highlight,
        page: 7,
        quadpoints: vec![[72.0, 640.1, 523.4, 640.1, 72.0, 628.7, 523.4, 628.7]],
        snippet: Some("a gospel of liberation".into()),
        snippet_prefix: None,
        snippet_suffix: None,
        color: Some("#f6c344".into()),
        note: None,
        created: None,
        modified: None,
    }
}

#[test]
fn write_then_load_round_trips() {
    let (_dir, lib) = temp_library();
    let mut sidecar = AnnotationSidecar::new("cone1970black");
    sidecar.pdf_hash = Some("blake3:abcd".into());
    sidecar.annotations.push(highlight("01AAA"));

    lib.write_annotations(&sidecar).unwrap();
    let loaded = lib.load_annotations("cone1970black").unwrap().unwrap();
    assert_eq!(loaded, sidecar);
}

#[test]
fn fsck_raises_no_annotation_problems_when_pdf_hash_matches_a_recorded_attachment() {
    let (dir, lib) = temp_library();
    fs::write(lib.entry_path("cone1970black"), ENTRY).unwrap();

    // A real blob so the attachment itself is clean; the note records its true hash.
    let bytes = b"%PDF-1.7 pretend";
    let hex = blake3::hash(bytes).to_hex().to_string();
    fs::create_dir_all(dir.path().join("attachments")).unwrap();
    fs::write(dir.path().join("attachments").join(&hex), bytes).unwrap();
    fs::write(
        lib.note_path("cone1970black"),
        format!("---\nattachments:\n  - hash: blake3:{hex}\n    filename: cone.pdf\n    bytes: {}\n---\n", bytes.len()),
    )
    .unwrap();

    let mut sidecar = AnnotationSidecar::new("cone1970black");
    sidecar.pdf_hash = Some(format!("blake3:{hex}"));
    sidecar.annotations.push(highlight("01AAA"));
    lib.write_annotations(&sidecar).unwrap();

    let report = lib.fsck().unwrap();
    assert!(report.is_clean(), "expected clean, got {report:?}");
}

#[test]
fn fsck_flags_annotation_problems() {
    let (_dir, lib) = temp_library();
    fs::write(lib.entry_path("cone1970black"), ENTRY).unwrap();

    // (1) pdf_hash referencing an attachment that is not recorded on the entry.
    let mut sidecar = AnnotationSidecar::new("cone1970black");
    sidecar.pdf_hash = Some("blake3:ghost".into());
    sidecar.annotations.push(highlight("01AAA"));
    lib.write_annotations(&sidecar).unwrap();

    // (2) filename/inner-key mismatch.
    fs::write(
        lib.annot_path("wrongname"),
        "{\"schema\":1,\"key\":\"actualkey\",\"annotations\":[]}",
    )
    .unwrap();

    // (3) unparseable JSON.
    fs::write(lib.annot_path("broken"), "{ not json").unwrap();

    let report = lib.fsck().unwrap();
    assert!(!report.is_clean());
    assert_eq!(report.annotation_pdf_unmatched.len(), 1);
    assert_eq!(report.annotation_pdf_unmatched[0].0, "cone1970black");
    assert_eq!(report.annotation_key_mismatches.len(), 1);
    assert_eq!(report.unparseable_annotations.len(), 1);
}
