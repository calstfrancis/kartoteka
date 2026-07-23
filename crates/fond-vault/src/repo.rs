//! In-process git operations via libgit2 (vendored). We never shell out to `git` — it is
//! not assumed present on the Windows machines (`docs/ARCHITECTURE.md` §7).

use std::path::{Path, PathBuf};

use git2::{IndexAddOption, Repository, Status, StatusOptions};

use crate::error::{Result, VaultError};

/// The commit author/committer identity.
#[derive(Debug, Clone)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

impl Identity {
    /// Read `user.name`/`user.email` from the effective git configuration (repo, then
    /// global). Errors if either is unset.
    pub fn from_git_config() -> Result<Identity> {
        let cfg = git2::Config::open_default()?;
        Ok(Identity {
            name: cfg.get_string("user.name")?,
            email: cfg.get_string("user.email")?,
        })
    }
}

/// A structured working-tree status. Paths are relative to the work directory. A path can
/// appear in more than one bucket (e.g. staged *and* unstaged when edited after staging).
#[derive(Debug, Default, Clone)]
pub struct VaultStatus {
    pub staged: Vec<String>,
    pub unstaged: Vec<String>,
    pub untracked: Vec<String>,
    pub conflicted: Vec<String>,
}

impl VaultStatus {
    pub fn is_clean(&self) -> bool {
        self.staged.is_empty()
            && self.unstaged.is_empty()
            && self.untracked.is_empty()
            && self.conflicted.is_empty()
    }
}

/// A git-backed vault at a work directory.
pub struct Vault {
    repo: Repository,
    workdir: PathBuf,
}

impl Vault {
    /// Initialise a new repository at `path` (creating it if needed).
    pub fn init(path: impl AsRef<Path>) -> Result<Vault> {
        let repo = Repository::init(path.as_ref())?;
        Self::from_repo(repo)
    }

    /// Open an existing repository, discovering it from `path` upward.
    pub fn open(path: impl AsRef<Path>) -> Result<Vault> {
        let repo = Repository::discover(path.as_ref())?;
        Self::from_repo(repo)
    }

    fn from_repo(repo: Repository) -> Result<Vault> {
        let workdir = repo
            .workdir()
            .ok_or_else(|| VaultError::Watch("bare repositories are not supported".into()))?
            .to_path_buf();
        Ok(Vault { repo, workdir })
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// Stage specific paths (relative to the work directory).
    pub fn stage(&self, paths: &[impl AsRef<Path>]) -> Result<()> {
        let mut index = self.repo.index()?;
        for p in paths {
            index.add_path(p.as_ref())?;
        }
        index.write()?;
        Ok(())
    }

    /// Stage everything, honouring `.gitignore` (so `attachments/` and `.kartoteka/` are
    /// never staged).
    pub fn stage_all(&self) -> Result<()> {
        let mut index = self.repo.index()?;
        index.add_all(["*"].iter(), IndexAddOption::DEFAULT, None)?;
        index.write()?;
        Ok(())
    }

    /// Commit the current index. Handles the first (unborn HEAD) commit. Returns the new
    /// commit's id as a hex string.
    pub fn commit(&self, message: &str, who: &Identity) -> Result<String> {
        let sig = git2::Signature::now(&who.name, &who.email)?;
        let mut index = self.repo.index()?;
        let tree_oid = index.write_tree()?;
        let tree = self.repo.find_tree(tree_oid)?;

        let parents: Vec<git2::Commit> = match self.repo.head() {
            Ok(head) => vec![head.peel_to_commit()?],
            Err(_) => Vec::new(), // unborn HEAD: this is the first commit
        };
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();

        let oid = self
            .repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)?;
        Ok(oid.to_string())
    }

    /// Structured working-tree status.
    pub fn status(&self) -> Result<VaultStatus> {
        let mut opts = StatusOptions::new();
        opts.include_untracked(true).recurse_untracked_dirs(true);
        let statuses = self.repo.statuses(Some(&mut opts))?;

        let staged_mask = Status::INDEX_NEW
            | Status::INDEX_MODIFIED
            | Status::INDEX_DELETED
            | Status::INDEX_RENAMED
            | Status::INDEX_TYPECHANGE;
        let unstaged_mask =
            Status::WT_MODIFIED | Status::WT_DELETED | Status::WT_RENAMED | Status::WT_TYPECHANGE;

        let mut out = VaultStatus::default();
        for entry in statuses.iter() {
            let path = entry.path().unwrap_or_default().to_string();
            let st = entry.status();
            if st.is_conflicted() {
                out.conflicted.push(path);
                continue;
            }
            if st.intersects(staged_mask) {
                out.staged.push(path.clone());
            }
            if st.intersects(unstaged_mask) {
                out.unstaged.push(path.clone());
            }
            if st.contains(Status::WT_NEW) {
                out.untracked.push(path);
            }
        }
        Ok(out)
    }
}
