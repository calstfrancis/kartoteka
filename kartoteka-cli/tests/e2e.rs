//! End-to-end tests driving the actual `kartoteka` binary, plus a clone-and-read-back
//! that spans fond-vault (git) and fond-bib (model).

use std::path::Path;
use std::process::{Command, Output};

use fond_bib::library::Library;

fn kartoteka(lib: &Path, args: &[&str]) -> Output {
    kartoteka_with_home(lib, args, None)
}

/// `commit` reads user.name/user.email via `git2::Config::open_default()`, which — unlike the
/// plain `git` CLI's own config discovery — only ever looks at the *global*/system config
/// (`$HOME/.gitconfig` and friends), never at a repository-local `.git/config`, since it's
/// called with no repository context at all. So a repo-local `git config --local` (tried
/// first here, and it looked right locally since it happened to leave the *real* machine
/// identity reachable as a fallback) never actually reaches it — only `HOME` does. Point
/// `HOME` at a throwaway directory with its own `.gitconfig` so this is isolated from
/// whatever identity (or lack of one) the machine/CI runner actually has.
fn kartoteka_with_home(lib: &Path, args: &[&str], home: Option<&Path>) -> Output {
    let bin = env!("CARGO_BIN_EXE_kartoteka");
    let mut cmd = Command::new(bin);
    cmd.arg("-L").arg(lib).args(args);
    if let Some(home) = home {
        cmd.env("HOME", home);
    }
    cmd.output().expect("failed to run kartoteka")
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

    // A throwaway HOME with its own .gitconfig for `commit` below — see kartoteka_with_home's
    // doc comment for why this, not a repo-local config, is what actually reaches
    // git2::Config::open_default(). Isolated from the machine/CI runner's real identity (or
    // lack of one) either way.
    let fake_home = tempfile::tempdir().unwrap();
    std::fs::write(
        fake_home.path().join(".gitconfig"),
        "[user]\n\tname = Test User\n\temail = test@example.com\n",
    )
    .unwrap();

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

    // commit (needs the throwaway identity from fake_home above)
    let out = kartoteka_with_home(
        lib,
        &["commit", "-m", "seed library"],
        Some(fake_home.path()),
    );
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
