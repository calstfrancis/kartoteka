//! `fond-vault` — git-backed vault operations and filesystem watching, shared across the
//! Fond suite. All git work is in-process via vendored libgit2; the public API exposes no
//! UI-framework types (`docs/ARCHITECTURE.md` §4).

pub mod error;
pub mod repo;
pub mod watch;

pub use error::{Result, VaultError};
pub use repo::{Identity, Vault, VaultStatus};

// Re-export git2 so the rare downstream that needs raw git types can reach them without
// declaring its own version-pinned dependency.
pub use git2;
pub use watch::{watch, VaultEvent, VaultWatch};
