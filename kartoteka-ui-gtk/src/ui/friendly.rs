//! Plain-language translations of the errors `fond-bib`/`fond-vault` hand back, for use in
//! toasts. Those crates stay UI-agnostic (`docs/ARCHITECTURE.md` §4) and report concrete,
//! technical error enums on purpose, precisely so a frontend can phrase them however it
//! wants — this is that phrasing for Kartoteka's GTK UI. Anything not specifically handled
//! falls back to the original message, prefixed so it's still recognisable as an error.

use fond_bib::BibError;
use fond_vault::VaultError;

/// Translate a `fond-bib` error into a sentence a non-technical user can act on. Falls back
/// to the raw message (still useful to *someone*, e.g. when reporting a bug) for the rarer
/// cases below that aren't worth a bespoke sentence.
pub fn bib_error(e: &BibError) -> String {
    match e {
        BibError::Io { path, source } => io_error("reading or writing a file", path, source),
        BibError::Yaml { path, .. } | BibError::PlainYaml { path, .. } => format!(
            "The file for \"{}\" looks like it was edited by hand and isn't valid anymore. \
             Open it in a text editor to fix the formatting, or ask for help if you're not \
             sure what changed.",
            file_label(path)
        ),
        BibError::NotSingleEntry { path, .. } => format!(
            "The file for \"{}\" should contain exactly one reference, but doesn't.",
            file_label(path)
        ),
        BibError::KeyMismatch { path, .. } => format!(
            "The file for \"{}\" doesn't match its own file name — it may have been renamed \
             outside Kartoteka.",
            file_label(path)
        ),
        BibError::Frontmatter { path, .. } => format!(
            "The note for \"{}\" has a formatting problem and couldn't be read.",
            file_label(path)
        ),
        BibError::UnkeyableEntry => {
            "This reference needs at least an author or a title before it can be saved.".to_string()
        }
        BibError::Import { message } => format!("Import failed: {message}"),
        BibError::Zotero { .. } => {
            "Couldn't read that Zotero database — it may be open in Zotero right now, or in \
             an unsupported version."
                .to_string()
        }
        BibError::Layout { path, .. } => format!(
            "\"{}\" doesn't look like a Kartoteka library folder.",
            path.display()
        ),
    }
}

/// Translate a `fond-vault` (git-backed backup) error. Git's own errors are usually terse
/// and technical (`"user.name" not found`, etc.) — the one case worth a plain sentence is a
/// missing git identity, since it's the single most likely first-backup failure for someone
/// who has never used git before; everything else keeps a short technical detail, since
/// backup is already an opt-in, more technical feature.
pub fn vault_error(e: &VaultError) -> String {
    match e {
        VaultError::Git(git_err) => {
            let msg = git_err.message();
            if msg.contains("user.name") || msg.contains("user.email") {
                "Backups need a name and email to record who made each change — the same \
                 one-time setup git itself needs. Run this in a terminal, then try backing \
                 up again:\n\n\
                 git config --global user.name \"Your Name\"\n\
                 git config --global user.email \"you@example.com\""
                    .to_string()
            } else {
                format!("Backup couldn't complete ({msg}).")
            }
        }
        VaultError::Io { path, source } => io_error("backing up your library", path, source),
        VaultError::Watch(msg) => format!("Couldn't watch the library folder for changes ({msg})."),
    }
}

/// Translate a plain `std::io::Error` encountered for `path` while doing `doing_what` (a
/// short lowercase gerund phrase, e.g. `"opening that library"`).
pub fn io_error(doing_what: &str, path: &std::path::Path, source: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    let where_ = file_label(path);
    match source.kind() {
        NotFound => format!("\"{where_}\" couldn't be found — it may have been moved or deleted."),
        PermissionDenied => {
            format!("Kartoteka doesn't have permission to access \"{where_}\".")
        }
        _ => format!("Something went wrong {doing_what} (\"{where_}\": {source})."),
    }
}

/// The file/folder name a path is best identified by in a sentence — just the last
/// component, not the full (often long, always technical-looking) path.
fn file_label(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
