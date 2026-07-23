//! The on-disk library layout: the authoritative source of truth. Everything here reads
//! and writes plain files under a library root (see `docs/DATA-MODEL.md`). Derived state
//! (`library.yml` aside, which is regenerated here) is not this module's concern.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use hayagriva::Library as HLibrary;

use crate::collection::Collection;
use crate::entry::{self, ParsedEntry};
use crate::error::{BibError, Result};
use crate::key;
use crate::note::Note;

pub const ENTRIES_DIR: &str = "entries";
pub const NOTES_DIR: &str = "notes";
pub const ANNOTS_DIR: &str = "annots";
pub const COLLECTIONS_DIR: &str = "collections";
pub const ATTACHMENTS_DIR: &str = "attachments";
pub const DERIVED_DIR: &str = ".kartoteka";
pub const LIBRARY_YML: &str = "library.yml";

const GITIGNORE_BODY: &str = "attachments/\n.kartoteka/\n";

/// A handle to a library rooted at a directory. Cheap to clone; holds only the root path.
#[derive(Debug, Clone)]
pub struct Library {
    root: PathBuf,
}

impl Library {
    /// Open an existing library. The root must exist and be a directory; individual
    /// subdirectories are tolerated as absent (an empty library is valid).
    pub fn open(root: impl Into<PathBuf>) -> Result<Library> {
        let root = root.into();
        if !root.is_dir() {
            return Err(BibError::Layout {
                path: root,
                message: "library root does not exist or is not a directory".into(),
            });
        }
        Ok(Library { root })
    }

    /// Create the layout at `root` (making the root if needed): the subdirectories and a
    /// `.gitignore` excluding `attachments/` and `.kartoteka/`. Idempotent.
    pub fn init(root: impl Into<PathBuf>) -> Result<Library> {
        let root = root.into();
        for dir in [ENTRIES_DIR, NOTES_DIR, ANNOTS_DIR, COLLECTIONS_DIR] {
            let path = root.join(dir);
            fs::create_dir_all(&path).map_err(|e| BibError::io(&path, e))?;
        }
        let gitignore = root.join(".gitignore");
        if !gitignore.exists() {
            fs::write(&gitignore, GITIGNORE_BODY).map_err(|e| BibError::io(&gitignore, e))?;
        }
        Library::open(root)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn entry_path(&self, key: &str) -> PathBuf {
        self.root.join(ENTRIES_DIR).join(format!("{key}.yml"))
    }

    pub fn note_path(&self, key: &str) -> PathBuf {
        self.root.join(NOTES_DIR).join(format!("{key}.md"))
    }

    pub fn collection_path(&self, slug: &str) -> PathBuf {
        self.root.join(COLLECTIONS_DIR).join(format!("{slug}.yml"))
    }

    pub fn library_yml_path(&self) -> PathBuf {
        self.root.join(LIBRARY_YML)
    }

    fn dir_stems(&self, dir: &str, ext: &str) -> Result<Vec<String>> {
        let path = self.root.join(dir);
        if !path.is_dir() {
            return Ok(Vec::new());
        }
        let mut stems = Vec::new();
        for entry in fs::read_dir(&path).map_err(|e| BibError::io(&path, e))? {
            let entry = entry.map_err(|e| BibError::io(&path, e))?;
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some(ext) {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    stems.push(stem.to_string());
                }
            }
        }
        stems.sort();
        Ok(stems)
    }

    /// Every citation key currently on disk (the `entries/*.yml` filenames). The filename
    /// is the authority for collision detection — no database needed.
    pub fn existing_keys(&self) -> Result<HashSet<String>> {
        Ok(self.dir_stems(ENTRIES_DIR, "yml")?.into_iter().collect())
    }

    /// Keys in ascending order.
    pub fn keys_sorted(&self) -> Result<Vec<String>> {
        self.dir_stems(ENTRIES_DIR, "yml")
    }

    pub fn collection_slugs(&self) -> Result<Vec<String>> {
        self.dir_stems(COLLECTIONS_DIR, "yml")
    }

    /// Read an entry file's raw text.
    pub fn read_entry_raw(&self, key: &str) -> Result<String> {
        let path = self.entry_path(key);
        fs::read_to_string(&path).map_err(|e| BibError::io(&path, e))
    }

    /// Load and validate a single entry, checking the filename/inner-key invariant.
    pub fn load_entry(&self, key: &str) -> Result<ParsedEntry> {
        let path = self.entry_path(key);
        let text = fs::read_to_string(&path).map_err(|e| BibError::io(&path, e))?;
        let parsed = entry::parse_single(&text, &path)?;
        if parsed.key != key {
            return Err(BibError::KeyMismatch {
                path,
                inner: parsed.key,
                file: key.to_string(),
            });
        }
        Ok(parsed)
    }

    /// Load a note if one exists for the key.
    pub fn load_note(&self, key: &str) -> Result<Option<Note>> {
        let path = self.note_path(key);
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(Note::parse(&text, &path)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(BibError::io(&path, e)),
        }
    }

    pub fn load_collection(&self, slug: &str) -> Result<Collection> {
        let path = self.collection_path(slug);
        let text = fs::read_to_string(&path).map_err(|e| BibError::io(&path, e))?;
        Collection::parse(&text, &path)
    }

    /// Add one or more entries from a Hayagriva YAML snippet. Keys in the snippet are
    /// treated as placeholders: each entry gets a freshly generated, collision-free key.
    /// Writes `entries/<key>.yml` for each, regenerates `library.yml`, and returns the
    /// assigned keys in input order.
    pub fn add_from_yaml(&self, input: &str) -> Result<Vec<String>> {
        let entries = entry::parse_all(input, Path::new("<input>"))?;
        let mut existing = self.existing_keys()?;
        let mut assigned = Vec::with_capacity(entries.len());

        for e in &entries {
            let family = entry::family_name(e);
            let year = entry::year(e);
            let title = entry::title_string(e);
            let base = key::generate_base_key(family.as_deref(), year, title.as_deref())?;
            let assigned_key = key::assign_key(&base, &existing);
            existing.insert(assigned_key.clone());

            let text = entry::serialize_entry_as(e, &assigned_key)?;
            let path = self.entry_path(&assigned_key);
            fs::write(&path, text).map_err(|e| BibError::io(&path, e))?;
            assigned.push(assigned_key);
        }

        self.regenerate_library_yml()?;
        Ok(assigned)
    }

    /// Regenerate the committed, Typst-consumable `library.yml` from `entries/`. Entries
    /// are concatenated in ascending-key order into one canonical Hayagriva document.
    /// Deterministic: same inputs always yield byte-identical output. `entries/` always
    /// wins — this never reads `library.yml` back.
    pub fn regenerate_library_yml(&self) -> Result<()> {
        let keys = self.keys_sorted()?;
        let mut lib = HLibrary::new();
        for key in &keys {
            let parsed = self.load_entry(key)?;
            lib.push(&parsed.entry);
        }
        let yaml = if lib.is_empty() {
            String::new()
        } else {
            hayagriva::io::to_yaml_str(&lib).map_err(|e| BibError::Yaml {
                path: self.library_yml_path(),
                message: e.to_string(),
            })?
        };
        let path = self.library_yml_path();
        fs::write(&path, yaml).map_err(|e| BibError::io(&path, e))?;
        Ok(())
    }

    /// Check the library for structural problems. Never mutates; returns everything found.
    pub fn fsck(&self) -> Result<FsckReport> {
        let mut report = FsckReport::default();
        let keys = self.keys_sorted()?;
        let key_set: HashSet<&String> = keys.iter().collect();

        // Entry files: parse + filename/inner-key agreement.
        for key in &keys {
            match self.load_entry(key) {
                Ok(_) => {}
                Err(BibError::KeyMismatch { inner, file, .. }) => {
                    report.key_filename_mismatches.push((file, inner));
                }
                Err(e) => {
                    report
                        .unparseable_entries
                        .push((self.entry_path(key).display().to_string(), e.to_string()));
                }
            }
        }

        // Collections: dangling key references.
        for slug in self.collection_slugs()? {
            let collection = self.load_collection(&slug)?;
            for key in &collection.keys {
                if !key_set.contains(key) {
                    report
                        .dangling_collection_refs
                        .push((slug.clone(), key.clone()));
                }
            }
        }

        // Attachments: gather every record (from notes), then reconcile with blobs.
        let mut recorded: HashSet<String> = HashSet::new();
        for key in &keys {
            if let Some(note) = self.load_note(key)? {
                for att in &note.frontmatter.attachments {
                    let hex = strip_hash_prefix(&att.hash);
                    recorded.insert(hex.to_string());
                    if !self.attachment_blob_path(hex).exists() {
                        report
                            .missing_attachments
                            .push((key.clone(), att.hash.clone()));
                    }
                }
            }
        }

        let attach_dir = self.root.join(ATTACHMENTS_DIR);
        if attach_dir.is_dir() {
            for entry in fs::read_dir(&attach_dir).map_err(|e| BibError::io(&attach_dir, e))? {
                let entry = entry.map_err(|e| BibError::io(&attach_dir, e))?;
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };
                if !recorded.contains(&name) {
                    report.orphaned_attachments.push(name.clone());
                }
                // Hash-mismatch: the blob's bytes must hash to its own filename.
                let bytes = fs::read(&path).map_err(|e| BibError::io(&path, e))?;
                let actual = blake3::hash(&bytes).to_hex().to_string();
                if actual != name {
                    report
                        .hash_mismatched_attachments
                        .push((name, format!("content hashes to {actual}")));
                }
            }
        }

        Ok(report)
    }

    fn attachment_blob_path(&self, hex: &str) -> PathBuf {
        self.root.join(ATTACHMENTS_DIR).join(hex)
    }
}

/// Strip a `blake3:` (or any `algo:`) prefix from a content hash, leaving the bare hex
/// used as the on-disk blob filename.
fn strip_hash_prefix(hash: &str) -> &str {
    hash.split_once(':').map(|(_, hex)| hex).unwrap_or(hash)
}

/// The result of [`Library::fsck`]. Empty means the library is structurally clean.
#[derive(Debug, Default)]
pub struct FsckReport {
    /// `(collection slug, referenced key that has no entry)`.
    pub dangling_collection_refs: Vec<(String, String)>,
    /// `(filename key, disagreeing inner key)`.
    pub key_filename_mismatches: Vec<(String, String)>,
    /// `(entry path, parse error)`.
    pub unparseable_entries: Vec<(String, String)>,
    /// `(entry key, attachment hash with no blob present)`.
    pub missing_attachments: Vec<(String, String)>,
    /// Blob filenames in `attachments/` with no record referencing them.
    pub orphaned_attachments: Vec<String>,
    /// `(blob filename, detail)` where the blob's bytes do not hash to its name.
    pub hash_mismatched_attachments: Vec<(String, String)>,
}

impl FsckReport {
    pub fn is_clean(&self) -> bool {
        self.dangling_collection_refs.is_empty()
            && self.key_filename_mismatches.is_empty()
            && self.unparseable_entries.is_empty()
            && self.missing_attachments.is_empty()
            && self.orphaned_attachments.is_empty()
            && self.hash_mismatched_attachments.is_empty()
    }

    pub fn problem_count(&self) -> usize {
        self.dangling_collection_refs.len()
            + self.key_filename_mismatches.len()
            + self.unparseable_entries.len()
            + self.missing_attachments.len()
            + self.orphaned_attachments.len()
            + self.hash_mismatched_attachments.len()
    }
}
