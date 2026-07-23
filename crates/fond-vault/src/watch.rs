//! Filesystem watching over the library directory. This is the mechanism the tight
//! Zerkalo coupling uses to see Kartoteka's writes live (`docs/ARCHITECTURE.md` §5). No
//! consumer exists yet — the API lands here in Milestone 1 so the seam is in place.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::error::{Result, VaultError};

/// A change observed under the watched directory.
#[derive(Debug)]
pub enum VaultEvent {
    /// One or more paths changed (created/modified/removed).
    Changed(Vec<PathBuf>),
    /// The watcher reported an error.
    Error(String),
}

/// A live watch. Dropping this stops watching, so callers must keep it alive for as long
/// as they want events on the paired [`Receiver`].
pub struct VaultWatch {
    _watcher: RecommendedWatcher,
}

/// Begin watching `dir` recursively. Returns the watch handle (keep it alive) and a
/// receiver of [`VaultEvent`]s. Blocking/threaded by design — `notify` runs its own
/// thread; there is no async runtime here (`docs/ARCHITECTURE.md` §3).
pub fn watch(dir: impl AsRef<Path>) -> Result<(VaultWatch, Receiver<VaultEvent>)> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| match res {
        Ok(event) => {
            let _ = tx.send(VaultEvent::Changed(event.paths));
        }
        Err(e) => {
            let _ = tx.send(VaultEvent::Error(e.to_string()));
        }
    })
    .map_err(|e| VaultError::Watch(e.to_string()))?;

    watcher
        .watch(dir.as_ref(), RecursiveMode::Recursive)
        .map_err(|e| VaultError::Watch(e.to_string()))?;

    Ok((VaultWatch { _watcher: watcher }, rx))
}
