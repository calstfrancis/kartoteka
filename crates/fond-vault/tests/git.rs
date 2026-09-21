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

/// `add_all` alone never stages removals, so a deleted entry stayed in every later commit
/// (and came back on restore). `stage_all` must record the deletion.
#[test]
fn stage_all_stages_deletions() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::init(dir.path()).unwrap();
    fs::write(dir.path().join("a.yml"), "a\n").unwrap();
    fs::write(dir.path().join("b.yml"), "b\n").unwrap();
    vault.stage_all().unwrap();
    vault.commit("add", &identity()).unwrap();

    fs::remove_file(dir.path().join("a.yml")).unwrap();
    vault.stage_all().unwrap();
    vault.commit("delete a", &identity()).unwrap();

    assert!(vault.status().unwrap().is_clean(), "deletion left unstaged");
    let repo = git2::Repository::open(dir.path()).unwrap();
    let tree = repo.head().unwrap().peel_to_tree().unwrap();
    assert!(
        tree.get_name("a.yml").is_none(),
        "deleted file still in the commit"
    );
    assert!(tree.get_name("b.yml").is_some());
}

#[test]
fn stage_of_a_deleted_path_records_removal_instead_of_failing() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::init(dir.path()).unwrap();
    fs::write(dir.path().join("a.yml"), "a\n").unwrap();
    vault.stage(&["a.yml"]).unwrap();
    vault.commit("add", &identity()).unwrap();
    fs::remove_file(dir.path().join("a.yml")).unwrap();
    vault.stage(&["a.yml"]).unwrap();
    assert_eq!(vault.status().unwrap().staged, vec!["a.yml"]);
}

/// A push the server refuses (non-fast-forward) must be an error, not a silent success.
#[test]
fn push_rejected_by_remote_is_an_error() {
    let remote_dir = tempfile::tempdir().unwrap();
    git2::Repository::init_bare(remote_dir.path()).unwrap();
    // A plain path, not `file://` + path: on Windows the latter is not a valid libgit2 URL.
    let url = remote_dir.path().to_str().unwrap().to_string();

    let first = tempfile::tempdir().unwrap();
    let v1 = Vault::init(first.path()).unwrap();
    fs::write(first.path().join("a.yml"), "1\n").unwrap();
    v1.stage_all().unwrap();
    v1.commit("one", &identity()).unwrap();
    v1.set_remote("origin", &url).unwrap();
    v1.push_github("origin", "tok")
        .expect("first push should succeed");

    // A second, independent history pushed to the same branch is non-fast-forward.
    let second = tempfile::tempdir().unwrap();
    let v2 = Vault::init(second.path()).unwrap();
    fs::write(second.path().join("b.yml"), "2\n").unwrap();
    v2.stage_all().unwrap();
    v2.commit("other", &identity()).unwrap();
    v2.set_remote("origin", &url).unwrap();
    // libgit2 itself refuses a non-fast-forward before contacting the server.
    v2.push_github("origin", "tok")
        .expect_err("non-fast-forward push reported as success");
}
