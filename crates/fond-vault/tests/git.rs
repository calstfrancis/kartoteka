//! Git round-trip tests: init → write → stage → commit → status, and a local clone that
//! reads the committed content back.

use std::fs;

use fond_vault::{Identity, Vault};

fn identity() -> Identity {
    Identity {
        name: "Test Author".into(),
        email: "test@example.com".into(),
    }
}

#[test]
fn init_stage_commit_and_status() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::init(dir.path()).unwrap();

    // Untracked before staging.
    fs::write(dir.path().join("entry.yml"), "key:\n  type: book\n").unwrap();
    let status = vault.status().unwrap();
    assert_eq!(status.untracked, vec!["entry.yml"]);
    assert!(status.staged.is_empty());

    // Staged after staging.
    vault.stage(&["entry.yml"]).unwrap();
    let status = vault.status().unwrap();
    assert_eq!(status.staged, vec!["entry.yml"]);
    assert!(status.untracked.is_empty());

    // Clean after commit (first commit, unborn HEAD path).
    let oid = vault.commit("initial commit", &identity()).unwrap();
    assert_eq!(oid.len(), 40, "expected a full sha1 hex oid");
    assert!(vault.status().unwrap().is_clean());
}

#[test]
fn gitignore_keeps_attachments_and_derived_out_of_commits() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::init(dir.path()).unwrap();
    fs::write(dir.path().join(".gitignore"), "attachments/\n.kartoteka/\n").unwrap();
    fs::create_dir_all(dir.path().join("attachments")).unwrap();
    fs::write(dir.path().join("attachments").join("blob"), b"binary").unwrap();
    fs::create_dir_all(dir.path().join(".kartoteka")).unwrap();
    fs::write(dir.path().join(".kartoteka").join("cache"), b"derived").unwrap();
    fs::write(dir.path().join("entry.yml"), "key:\n").unwrap();

    vault.stage_all().unwrap();
    let status = vault.status().unwrap();
    // Only the entry and .gitignore are staged; ignored paths never appear.
    assert!(status.staged.contains(&"entry.yml".to_string()));
    assert!(status.staged.contains(&".gitignore".to_string()));
    assert!(!status.staged.iter().any(|p| p.starts_with("attachments/")));
    assert!(!status.staged.iter().any(|p| p.starts_with(".kartoteka/")));
}

#[test]
fn commit_then_clone_reads_content_back() {
    let src = tempfile::tempdir().unwrap();
    let vault = Vault::init(src.path()).unwrap();
    fs::write(
        src.path().join("library.yml"),
        "key:\n  type: book\n  title: X\n",
    )
    .unwrap();
    vault.stage_all().unwrap();
    vault.commit("seed", &identity()).unwrap();

    // Local clone via libgit2 (no network transport needed).
    let dst = tempfile::tempdir().unwrap();
    let clone_path = dst.path().join("clone");
    git2::Repository::clone(src.path().to_str().unwrap(), &clone_path).unwrap();

    // Normalize line endings before comparing: Windows git installs commonly default to
    // `core.autocrlf=true`, so a checkout (this clone included) can rewrite LF to CRLF on
    // disk even though the committed blob itself is untouched — that's a platform git-config
    // concern, not something this round-trip test should fail on.
    let cloned = fs::read_to_string(clone_path.join("library.yml")).unwrap();
    assert_eq!(
        cloned.replace("\r\n", "\n"),
        "key:\n  type: book\n  title: X\n"
    );
}
