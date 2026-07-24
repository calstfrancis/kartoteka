//! The on-disk library layout: the authoritative source of truth. Everything here reads
//! and writes plain files under a library root (see `docs/DATA-MODEL.md`). Derived state
//! (`library.yml` aside, which is regenerated here) is not this module's concern.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use hayagriva::Library as HLibrary;

use crate::annotation::AnnotationSidecar;
use crate::collection::Collection;
use crate::entry::{self, ParsedEntry};
use crate::error::{BibError, Result};
use crate::key;
use crate::note::Attachment;
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

    pub fn annot_path(&self, key: &str) -> PathBuf {
        self.root.join(ANNOTS_DIR).join(format!("{key}.json"))
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

    /// Write an entry to `entries/<entry.key()>.yml`, preserving its own key (no key
    /// generation). Used by import, where the source's citation keys must be kept so
    /// existing `@key` references in the user's documents keep working.
    pub fn write_entry(&self, entry: &hayagriva::Entry) -> Result<PathBuf> {
        let path = self.entry_path(entry.key());
        let text = entry::serialize_entry(entry)?;
        ensure_parent(&path)?;
        fs::write(&path, text).map_err(|e| BibError::io(&path, e))?;
        Ok(path)
    }

    /// Write a note to `notes/<key>.md`.
    pub fn write_note(&self, key: &str, note: &Note) -> Result<PathBuf> {
        let path = self.note_path(key);
        ensure_parent(&path)?;
        fs::write(&path, note.to_text()?).map_err(|e| BibError::io(&path, e))?;
        Ok(path)
    }

    pub fn attachments_dir(&self) -> PathBuf {
        self.root.join(ATTACHMENTS_DIR)
    }

    /// Path where the blob with the given bare hex hash lives.
    pub fn attachment_blob_path(&self, hex: &str) -> PathBuf {
        self.root.join(ATTACHMENTS_DIR).join(hex)
    }

    /// Store a PDF (or any file) as an attachment of `key`: hash it (blake3), copy the blob
    /// into `attachments/`, and record it in the entry's note (creating the note if
    /// needed). `pages` is supplied by the caller (via `fond-doc`) so this crate stays
    /// PDFium-free. Returns the attachment record.
    pub fn store_attachment(
        &self,
        key: &str,
        src: &Path,
        pages: Option<u32>,
    ) -> Result<Attachment> {
        let bytes = fs::read(src).map_err(|e| BibError::io(src, e))?;
        let hex = blake3::hash(&bytes).to_hex().to_string();

        fs::create_dir_all(self.attachments_dir())
            .map_err(|e| BibError::io(self.attachments_dir(), e))?;
        let dest = self.attachment_blob_path(&hex);
        if !dest.exists() {
            fs::write(&dest, &bytes).map_err(|e| BibError::io(&dest, e))?;
        }

        let attachment = Attachment {
            hash: format!("blake3:{hex}"),
            filename: src
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("attachment")
                .to_string(),
            bytes: bytes.len() as u64,
            pages,
        };

        let mut note = self.load_note(key)?.unwrap_or_default();
        if !note
            .frontmatter
            .attachments
            .iter()
            .any(|a| a.hash == attachment.hash)
        {
            note.frontmatter.attachments.push(attachment.clone());
        }
        if note.frontmatter.date_added.is_none() {
            note.frontmatter.date_added = Some(crate::util::today_iso());
        }
        self.write_note(key, &note)?;

        Ok(attachment)
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

    /// The related-entry keys recorded on `key`'s note (empty if none / no note).
    pub fn related(&self, key: &str) -> Result<Vec<String>> {
        Ok(self
            .load_note(key)?
            .map(|n| n.frontmatter.related)
            .unwrap_or_default())
    }

    /// Set `key`'s related list to `targets`, keeping links symmetric: every target gains
    /// `key` in its own note, and any entry previously linked but no longer in `targets`
    /// loses it. Self-links are ignored. Writes only the notes that actually change.
    pub fn set_related(&self, key: &str, targets: &[String]) -> Result<()> {
        let old: HashSet<String> = self.related(key)?.into_iter().collect();
        let new: HashSet<String> = targets
            .iter()
            .filter(|t| t.as_str() != key)
            .cloned()
            .collect();

        // Write key's own list (sorted for stable diffs).
        let mut own = self.load_note(key)?.unwrap_or_default();
        let mut list: Vec<String> = new.iter().cloned().collect();
        list.sort();
        own.frontmatter.related = list;
        self.write_note(key, &own)?;

        // Add key to newly linked targets.
        for t in new.difference(&old) {
            let mut note = self.load_note(t)?.unwrap_or_default();
            if !note.frontmatter.related.iter().any(|k| k == key) {
                note.frontmatter.related.push(key.to_string());
                note.frontmatter.related.sort();
                self.write_note(t, &note)?;
            }
        }
        // Remove key from dropped targets.
        for t in old.difference(&new) {
            if let Some(mut note) = self.load_note(t)? {
                let before = note.frontmatter.related.len();
                note.frontmatter.related.retain(|k| k != key);
                if note.frontmatter.related.len() != before {
                    self.write_note(t, &note)?;
                }
            }
        }
        Ok(())
    }

    /// Load an entry's annotation sidecar if one exists.
    pub fn load_annotations(&self, key: &str) -> Result<Option<AnnotationSidecar>> {
        let path = self.annot_path(key);
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(AnnotationSidecar::parse(&text, &path)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(BibError::io(&path, e)),
        }
    }

    /// Write an entry's annotation sidecar.
    pub fn write_annotations(&self, sidecar: &AnnotationSidecar) -> Result<PathBuf> {
        let path = self.annot_path(&sidecar.key);
        ensure_parent(&path)?;
        fs::write(&path, sidecar.to_json()?).map_err(|e| BibError::io(&path, e))?;
        Ok(path)
    }

    pub fn load_collection(&self, slug: &str) -> Result<Collection> {
        let path = self.collection_path(slug);
        let text = fs::read_to_string(&path).map_err(|e| BibError::io(&path, e))?;
        Collection::parse(&text, &path)
    }

    /// Write a collection to `collections/<slug>.yml`.
    pub fn save_collection(&self, slug: &str, collection: &Collection) -> Result<PathBuf> {
        let path = self.collection_path(slug);
        ensure_parent(&path)?;
        fs::write(&path, collection.to_text()?).map_err(|e| BibError::io(&path, e))?;
        Ok(path)
    }

    /// Delete a collection file. Missing is not an error.
    pub fn delete_collection(&self, slug: &str) -> Result<()> {
        let path = self.collection_path(slug);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(BibError::io(&path, e)),
        }
    }

    /// Add one or more entries from a Hayagriva YAML snippet. Keys in the snippet are
    /// treated as placeholders: each entry gets a freshly generated, collision-free key.
    /// Writes `entries/<key>.yml` for each, regenerates `library.yml`, and returns the
    /// assigned keys in input order.
    pub fn add_from_yaml(&self, input: &str) -> Result<Vec<String>> {
        let entries = entry::parse_all(input, Path::new("<input>"))?;
        self.add_entries(&entries)
    }

    /// Add entries parsed from a BibLaTeX/BibTeX snippet, generating fresh citation keys
    /// (unlike `import_bibtex`, which preserves the source keys). Used by acquisition.
    pub fn add_bibtex(&self, source: &str) -> Result<Vec<String>> {
        let library = hayagriva::io::from_biblatex_str(source).map_err(|errors| {
            let joined = errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            BibError::Import {
                message: format!("could not parse BibLaTeX: {joined}"),
            }
        })?;
        let entries: Vec<_> = library.iter().cloned().collect();
        self.add_entries(&entries)
    }

    /// Every tag used across the library, with how many entries carry it, sorted by name.
    pub fn all_tags(&self) -> Result<Vec<(String, usize)>> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for key in self.keys_sorted()? {
            if let Some(note) = self.load_note(&key)? {
                for tag in note.frontmatter.tags {
                    *counts.entry(tag).or_default() += 1;
                }
            }
        }
        let mut tags: Vec<(String, usize)> = counts.into_iter().collect();
        tags.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(tags)
    }

    /// Rename `old` to `new` across all notes (merging if `new` already exists). An empty
    /// `new` deletes the tag. Returns the number of notes changed.
    pub fn rename_tag(&self, old: &str, new: &str) -> Result<usize> {
        let mut changed = 0;
        for key in self.keys_sorted()? {
            let Some(mut note) = self.load_note(&key)? else {
                continue;
            };
            if !note.frontmatter.tags.iter().any(|t| t == old) {
                continue;
            }
            let mut seen = HashSet::new();
            note.frontmatter.tags = note
                .frontmatter
                .tags
                .into_iter()
                .map(|t| if t == old { new.to_string() } else { t })
                .filter(|t| !t.is_empty() && seen.insert(t.clone()))
                .collect();
            self.write_note(&key, &note)?;
            changed += 1;
        }
        Ok(changed)
    }

    /// Find groups of entries that look like duplicates, matched by DOI, else ISBN, else
    /// folded title + year. Returns each group of 2+ keys (singletons excluded).
    pub fn find_duplicates(&self) -> Result<Vec<Vec<String>>> {
        let mut groups: HashMap<String, Vec<String>> = HashMap::new();
        for key in self.keys_sorted()? {
            let parsed = self.load_entry(&key)?;
            let e = &parsed.entry;
            let bucket = if let Some(doi) = e.doi() {
                format!("doi:{}", crate::zotero::normalize_doi(doi))
            } else if let Some(isbn) = e.isbn() {
                format!("isbn:{}", isbn.replace(['-', ' '], ""))
            } else if let Some(title) = entry::title_string(e) {
                let folded = crate::key::ascii_fold(&title);
                if folded.is_empty() {
                    continue;
                }
                let year = entry::year(e).map(|y| y.to_string()).unwrap_or_default();
                format!("ty:{folded}:{year}")
            } else {
                continue;
            };
            groups.entry(bucket).or_default().push(key);
        }
        let mut dups: Vec<Vec<String>> = groups.into_values().filter(|g| g.len() > 1).collect();
        dups.sort();
        Ok(dups)
    }

    /// Merge a duplicate group into `into`: fold the others' tags, attachments,
    /// annotations, and note prose into the target; delete the others' files; and replace
    /// them in every collection. `into` must be one of `keys`. Regenerates `library.yml`.
    pub fn merge_group(&self, keys: &[String], into: &str) -> Result<()> {
        let others: Vec<&String> = keys.iter().filter(|k| k.as_str() != into).collect();

        let mut note = self.load_note(into)?.unwrap_or_default();
        let mut annots = self
            .load_annotations(into)?
            .unwrap_or_else(|| AnnotationSidecar::new(into));

        for other in &others {
            if let Some(other_note) = self.load_note(other)? {
                for tag in other_note.frontmatter.tags {
                    if !note.frontmatter.tags.contains(&tag) {
                        note.frontmatter.tags.push(tag);
                    }
                }
                for att in other_note.frontmatter.attachments {
                    if !note
                        .frontmatter
                        .attachments
                        .iter()
                        .any(|a| a.hash == att.hash)
                    {
                        note.frontmatter.attachments.push(att);
                    }
                }
                let body = other_note.body.trim();
                if !body.is_empty() {
                    if !note.body.trim().is_empty() {
                        note.body.push_str("\n\n");
                    }
                    note.body.push_str(body);
                    note.body.push('\n');
                }
            }
            if let Some(sidecar) = self.load_annotations(other)? {
                for a in sidecar.annotations {
                    annots.upsert(a);
                }
            }
        }

        self.write_note(into, &note)?;
        if !annots.annotations.is_empty() {
            annots.key = into.to_string();
            self.write_annotations(&annots)?;
        }

        for other in &others {
            let _ = fs::remove_file(self.entry_path(other));
            let _ = fs::remove_file(self.note_path(other));
            let _ = fs::remove_file(self.annot_path(other));
        }

        // Replace merged keys in every collection (deduped, order preserved).
        for slug in self.collection_slugs()? {
            let mut coll = self.load_collection(&slug)?;
            let before = coll.keys.clone();
            let mut seen = HashSet::new();
            coll.keys = coll
                .keys
                .into_iter()
                .map(|k| {
                    if others.iter().any(|o| o.as_str() == k) {
                        into.to_string()
                    } else {
                        k
                    }
                })
                .filter(|k| seen.insert(k.clone()))
                .collect();
            if coll.keys != before {
                self.save_collection(&slug, &coll)?;
            }
        }

        self.regenerate_library_yml()?;
        Ok(())
    }

    /// Add entries with freshly generated, collision-free citation keys. Writes each
    /// `entries/<key>.yml` and regenerates `library.yml`. Returns the assigned keys.
    pub fn add_entries(&self, entries: &[hayagriva::Entry]) -> Result<Vec<String>> {
        let mut existing = self.existing_keys()?;
        let mut assigned = Vec::with_capacity(entries.len());

        for e in entries {
            let family = entry::family_name(e);
            let year = entry::year(e);
            let title = entry::title_string(e);
            let base = key::generate_base_key(family.as_deref(), year, title.as_deref())?;
            let assigned_key = key::assign_key(&base, &existing);
            existing.insert(assigned_key.clone());

            let text = entry::serialize_entry_as(e, &assigned_key)?;
            let path = self.entry_path(&assigned_key);
            ensure_parent(&path)?;
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

        // Annotation sidecars: parse, filename/inner-key agreement, and pdf_hash linkage.
        for stem in self.dir_stems(ANNOTS_DIR, "json")? {
            let path = self.annot_path(&stem);
            let text = fs::read_to_string(&path).map_err(|e| BibError::io(&path, e))?;
            let sidecar = match AnnotationSidecar::parse(&text, &path) {
                Ok(s) => s,
                Err(e) => {
                    report
                        .unparseable_annotations
                        .push((path.display().to_string(), e.to_string()));
                    continue;
                }
            };
            if sidecar.key != stem {
                report
                    .annotation_key_mismatches
                    .push((stem.clone(), sidecar.key.clone()));
            }
            if let Some(pdf_hash) = &sidecar.pdf_hash {
                let hex = strip_hash_prefix(pdf_hash);
                let attached: bool = self
                    .load_note(&stem)?
                    .map(|note| {
                        note.frontmatter
                            .attachments
                            .iter()
                            .any(|a| strip_hash_prefix(&a.hash) == hex)
                    })
                    .unwrap_or(false);
                if !attached {
                    report
                        .annotation_pdf_unmatched
                        .push((stem.clone(), pdf_hash.clone()));
                }
            }
        }

        Ok(report)
    }
}

/// Strip a `blake3:` (or any `algo:`) prefix from a content hash, leaving the bare hex
/// used as the on-disk blob filename.
/// Ensure the directory that will hold `path` exists (so writes into a freshly opened or
/// partially-populated library don't fail with "no such file or directory").
fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| BibError::io(parent, e))?;
    }
    Ok(())
}

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
    /// `(annotation path, parse error)`.
    pub unparseable_annotations: Vec<(String, String)>,
    /// `(filename key, disagreeing inner key)` for annotation sidecars.
    pub annotation_key_mismatches: Vec<(String, String)>,
    /// `(entry key, pdf_hash)` where a sidecar's `pdf_hash` matches no attachment recorded
    /// for that entry.
    pub annotation_pdf_unmatched: Vec<(String, String)>,
}

impl FsckReport {
    pub fn is_clean(&self) -> bool {
        self.problem_count() == 0
    }

    pub fn problem_count(&self) -> usize {
        self.dangling_collection_refs.len()
            + self.key_filename_mismatches.len()
            + self.unparseable_entries.len()
            + self.missing_attachments.len()
            + self.orphaned_attachments.len()
            + self.hash_mismatched_attachments.len()
            + self.unparseable_annotations.len()
            + self.annotation_key_mismatches.len()
            + self.annotation_pdf_unmatched.len()
    }
}
