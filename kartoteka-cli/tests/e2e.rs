//! End-to-end tests driving the actual `kartoteka` binary, plus a clone-and-read-back
//! that spans fond-vault (git) and fond-bib (model).

use std::path::Path;
use std::process::{Command, Output};

use fond_bib::library::Library;

fn kartoteka(lib: &Path, args: &[&str]) -> Output {
    let bin = env!("CARGO_BIN_EXE_kartoteka");
    Command::new(bin)
        .arg("-L")
        .arg(lib)
        .args(args)
        .output()
        .expect("failed to run kartoteka")
}

const SNIPPET: &str =
    "_:\n  type: book\n  title: The Destiny of Man\n  author: Berdyaev, Nikolai\n  date: 1937\n";

#[test]
fn full_vault_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path();

    // init
    let out = kartoteka(lib, &["init"]);
    assert!(out.status.success(), "init failed: {out:?}");
    assert!(lib.join("entries").is_dir());
    assert!(lib.join(".git").is_dir());

    // `commit` below reads user.name/user.email via git2::Config::open_default(), which
    // checks repo-local config before global — set it here (local only, not --global) so
    // this doesn't depend on the machine/CI runner having a git identity configured at all.
    // A fresh GitHub Actions runner never does, which is what actually broke this in CI.
    for (key, value) in [
        ("user.name", "Test User"),
        ("user.email", "test@example.com"),
    ] {
        let out = Command::new("git")
            .args(["-C", lib.to_str().unwrap(), "config", "--local", key, value])
            .output()
            .expect("failed to run git config");
        assert!(out.status.success(), "git config {key} failed: {out:?}");
    }

    // add from a file
    let snippet = lib.join("snippet.yml");
    std::fs::write(&snippet, SNIPPET).unwrap();
    let out = kartoteka(lib, &["add", "--file", snippet.to_str().unwrap()]);
    assert!(out.status.success(), "add failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("berdyaev1937destiny"), "got: {stdout}");
    assert!(lib.join("entries/berdyaev1937destiny.yml").exists());
    std::fs::remove_file(&snippet).unwrap();

    // list
    let out = kartoteka(lib, &["list"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("berdyaev1937destiny"));

    // reindex is byte-stable
    kartoteka(lib, &["reindex"]);
    let first = std::fs::read(lib.join("library.yml")).unwrap();
    kartoteka(lib, &["reindex"]);
    let second = std::fs::read(lib.join("library.yml")).unwrap();
    assert_eq!(first, second, "library.yml not stable across reindex");

    // fsck clean → exit 0
    let out = kartoteka(lib, &["fsck"]);
    assert!(out.status.success(), "fsck should be clean: {out:?}");

    // commit (uses the repo-local identity set above)
    let out = kartoteka(lib, &["commit", "-m", "seed library"]);
    assert!(out.status.success(), "commit failed: {out:?}");

    // clone and read the entry back through fond-bib
    let dst = tempfile::tempdir().unwrap();
    let clone_path = dst.path().join("clone");
    fond_vault::git2::Repository::clone(lib.to_str().unwrap(), &clone_path).unwrap();
    let cloned_lib = Library::open(&clone_path).unwrap();
    let parsed = cloned_lib.load_entry("berdyaev1937destiny").unwrap();
    assert_eq!(parsed.key, "berdyaev1937destiny");
}

#[test]
fn fsck_exits_nonzero_on_problems() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path();
    kartoteka(lib, &["init"]);

    // Dangling collection reference.
    std::fs::write(
        lib.join("collections/broken.yml"),
        "name: Broken\nkeys:\n- nope\n",
    )
    .unwrap();

    let out = kartoteka(lib, &["fsck"]);
    assert!(!out.status.success(), "fsck should fail on a dangling ref");
    assert!(String::from_utf8_lossy(&out.stdout).contains("dangling"));
}
