//! The on-disk library layout: the authoritative source of truth. Everything here reads
//! and writes plain files under a library root (see `docs/DATA-MODEL.md`). Derived state
//! (`library.yml` aside, which is regenerated here) is not this module's concern.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use hayagriva::Library as HLibrary;

use crate::ai::AiMetadata;
use crate::annotation::AnnotationSidecar;
use crate::collection::Collection;
use crate::entry::{self, ParsedEntry};
use crate::error::{BibError, Result};
use crate::key;
use crate::node::Node;
use crate::note::Attachment;
use crate::note::Note;
use crate::project::{self, Project};
use crate::relation::{Predicate, Relation};

pub const ENTRIES_DIR: &str = "entries";
pub const NOTES_DIR: &str = "notes";
pub const ANNOTS_DIR: &str = "annots";
pub const COLLECTIONS_DIR: &str = "collections";
pub const AI_DIR: &str = "ai";
pub const PROJECTS_DIR: &str = "projects";
pub const NODES_DIR: &str = "nodes";
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
        for dir in [
            ENTRIES_DIR,
            NOTES_DIR,
            ANNOTS_DIR,
            COLLECTIONS_DIR,
            AI_DIR,
            PROJECTS_DIR,
            NODES_DIR,
        ] {
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

    pub fn ai_path(&self, key: &str) -> PathBuf {
        self.root.join(AI_DIR).join(format!("{key}.yml"))
    }

    pub fn node_path(&self, slug: &str) -> PathBuf {
        self.root.join(NODES_DIR).join(format!("{slug}.md"))
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

    /// Every node slug currently on disk (the `nodes/*.md` filenames), ascending. The
    /// filename is the authority for slug identity — no database needed.
    pub fn node_slugs(&self) -> Result<Vec<String>> {
        self.dir_stems(NODES_DIR, "md")
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

    /// Load and parse a single node from `nodes/<slug>.md`. Slug/filename agreement is an
    /// `fsck` concern (a slug is stable even if the `label` is later edited), so this does
    /// not enforce it — it just reads and parses.
    pub fn load_node(&self, slug: &str) -> Result<Node> {
        let path = self.node_path(slug);
        let text = fs::read_to_string(&path).map_err(|e| BibError::io(&path, e))?;
        Node::parse(&text, &path)
    }

    /// Write a node to `nodes/<slug>.md`.
    pub fn write_node(&self, slug: &str, node: &Node) -> Result<PathBuf> {
        let path = self.node_path(slug);
        ensure_parent(&path)?;
        fs::write(&path, node.to_text()?).map_err(|e| BibError::io(&path, e))?;
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

    // ---- Typed relations (see docs/M2-SPEC.md §1) -------------------------------------

    /// All relation edges recorded on `key`'s note (forward and inverse), empty if none.
    pub fn relations(&self, key: &str) -> Result<Vec<Relation>> {
        Ok(self
            .load_note(key)?
            .map(|n| n.frontmatter.relations)
            .unwrap_or_default())
    }

    /// Just the user-authored forward edges on `key` (the ones `key` "owns").
    pub fn forward_relations(&self, key: &str) -> Result<Vec<Relation>> {
        Ok(self
            .relations(key)?
            .into_iter()
            .filter(|r| !r.inverse)
            .collect())
    }

    /// Replace `key`'s forward edges with `forward`, maintaining the matching edge on each
    /// target. `key`'s inverse edges (maintained by other notes) are preserved untouched.
    ///
    /// For an asymmetric predicate the target gains an `inverse: true` edge carrying the
    /// inverse predicate; for the self-inverse `Related` the target gains a plain forward
    /// edge back to `key` (symmetric, matching legacy `set_related`). Writes only the notes
    /// that actually change.
    pub fn set_relations(&self, key: &str, forward: &[Relation]) -> Result<()> {
        let new = normalize_forward(key, forward);
        let old = self.forward_relations(key)?;

        // Rewrite key's note: new forward edges + key's existing inverse edges, re-sorted.
        let mut own = self.load_note(key)?.unwrap_or_default();
        let kept_inverse: Vec<Relation> = own
            .frontmatter
            .relations
            .iter()
            .filter(|r| r.inverse)
            .cloned()
            .collect();
        let mut relations = new.clone();
        relations.extend(kept_inverse);
        sort_relations(&mut relations);
        own.frontmatter.relations = relations;
        self.write_note(key, &own)?;

        let new_ids: HashSet<(Predicate, &str)> = new.iter().map(|r| r.identity()).collect();
        let old_ids: HashSet<(Predicate, &str)> = old.iter().map(|r| r.identity()).collect();

        // Added forward edges → add the counterpart edge on the target.
        for r in &new {
            if !old_ids.contains(&r.identity()) {
                self.add_counterpart_edge(&r.target, counterpart(r.predicate, key))?;
            }
        }
        // Removed forward edges → drop the counterpart edge on the (old) target.
        for r in &old {
            if !new_ids.contains(&r.identity()) {
                let cp = counterpart(r.predicate, key);
                self.remove_edge(&r.target, cp.predicate, key)?;
            }
        }
        Ok(())
    }

    /// Add a single forward edge `key --predicate--> target` (idempotent).
    pub fn add_relation(&self, key: &str, predicate: Predicate, target: &str) -> Result<()> {
        let mut forward = self.forward_relations(key)?;
        let wanted = (predicate, target);
        if !forward.iter().any(|r| r.identity() == wanted) {
            forward.push(Relation::forward(predicate, target));
        }
        self.set_relations(key, &forward)
    }

    /// Remove the forward edge `key --predicate--> target` (idempotent).
    pub fn remove_relation(&self, key: &str, predicate: Predicate, target: &str) -> Result<()> {
        let forward: Vec<Relation> = self
            .forward_relations(key)?
            .into_iter()
            .filter(|r| r.identity() != (predicate, target))
            .collect();
        self.set_relations(key, &forward)
    }

    /// Add `edge` to `target`'s note if an edge with the same identity isn't already there.
    fn add_counterpart_edge(&self, target: &str, edge: Relation) -> Result<()> {
        let mut note = self.load_note(target)?.unwrap_or_default();
        if !note
            .frontmatter
            .relations
            .iter()
            .any(|r| r.identity() == edge.identity())
        {
            note.frontmatter.relations.push(edge);
            sort_relations(&mut note.frontmatter.relations);
            self.write_note(target, &note)?;
        }
        Ok(())
    }

    /// Remove any edge on `target`'s note with identity `(predicate, other)`.
    fn remove_edge(&self, target: &str, predicate: Predicate, other: &str) -> Result<()> {
        if let Some(mut note) = self.load_note(target)? {
            let before = note.frontmatter.relations.len();
            note.frontmatter
                .relations
                .retain(|r| r.identity() != (predicate, other));
            if note.frontmatter.relations.len() != before {
                self.write_note(target, &note)?;
            }
        }
        Ok(())
    }

    /// Reconcile every note's *maintained* relation edges against the forward edges that
    /// are the source of truth (see docs/M2-SPEC.md §1.5). Asymmetric inverse edges are
    /// fully derived (dropped and regenerated); self-inverse `Related` edges are
    /// symmetric-completed (a missing reciprocal is added, never silently removed).
    ///
    /// With `fix == false` it only reports; with `fix == true` it rewrites the notes that
    /// need it. Idempotent: a second run with `fix` finds nothing.
    pub fn reconcile_relations(&self, fix: bool) -> Result<RelationReconcile> {
        let mut report = RelationReconcile::default();
        let key_set = self.existing_keys()?;

        // Expected maintained edges per note, derived from all forward edges in the vault.
        // expected[owner] = set of edges that owner must carry because of others' forwards.
        let mut expected: HashMap<String, Vec<Relation>> = HashMap::new();
        let all_keys = self.keys_sorted()?;
        for key in &all_keys {
            for r in self.forward_relations(key)? {
                if !key_set.contains(&r.target) {
                    report
                        .dangling_targets
                        .push((key.clone(), r.predicate, r.target.clone()));
                    continue;
                }
                expected
                    .entry(r.target.clone())
                    .or_default()
                    .push(counterpart(r.predicate, key));
            }
        }

        for key in &all_keys {
            // Default to an empty note: a key may legitimately have no note yet but still
            // be owed a maintained inverse edge from another note's forward edge.
            let mut note = self.load_note(key)?.unwrap_or_default();
            let want = expected.remove(key).unwrap_or_default();
            let want_ids: HashSet<(Predicate, &str)> = want.iter().map(|r| r.identity()).collect();

            // Partition the note's edges into user-authored forward (kept as-is) and
            // maintained (inverse edges + self-inverse reciprocals we materialized).
            let mut rebuilt: Vec<Relation> = Vec::new();
            for r in &note.frontmatter.relations {
                if r.inverse {
                    // Derived asymmetric inverse: keep only if still expected.
                    if want_ids.contains(&r.identity()) {
                        rebuilt.push(r.clone());
                    } else {
                        report
                            .orphaned
                            .push((key.clone(), r.predicate, r.target.clone()));
                    }
                } else {
                    // Forward edge (asymmetric-authored or self-inverse): always kept.
                    rebuilt.push(r.clone());
                }
            }
            // Add any expected maintained edge that's missing (identity not already present).
            let present: HashSet<(Predicate, &str)> =
                rebuilt.iter().map(|r| r.identity()).collect();
            let present: HashSet<(Predicate, String)> =
                present.iter().map(|(p, t)| (*p, t.to_string())).collect();
            for e in want {
                if !present.contains(&(e.predicate, e.target.clone())) {
                    report
                        .missing
                        .push((key.clone(), e.predicate, e.target.clone()));
                    rebuilt.push(e);
                }
            }

            sort_relations(&mut rebuilt);
            if rebuilt != note.frontmatter.relations && fix {
                note.frontmatter.relations = rebuilt;
                self.write_note(key, &note)?;
            }
        }

        Ok(report)
    }

    /// Lift every note's legacy untyped `related:` list into typed `relations` edges with
    /// `predicate: related`, then clear `related`. Runs [`reconcile_relations`] afterward so
    /// the old two-sided symmetric storage collapses to the canonical form and any missing
    /// reciprocal is materialized. Idempotent: a note with no legacy `related` is untouched.
    /// Returns the number of notes changed.
    ///
    /// [`reconcile_relations`]: Library::reconcile_relations
    pub fn migrate_related_to_relations(&self) -> Result<usize> {
        let mut changed = 0;
        for key in self.keys_sorted()? {
            let Some(mut note) = self.load_note(&key)? else {
                continue;
            };
            if note.frontmatter.related.is_empty() {
                continue;
            }
            let legacy = std::mem::take(&mut note.frontmatter.related);
            for target in legacy {
                if target == key {
                    continue;
                }
                let id = (Predicate::Related, target.as_str());
                if !note.frontmatter.relations.iter().any(|r| r.identity() == id) {
                    note.frontmatter
                        .relations
                        .push(Relation::forward(Predicate::Related, target));
                }
            }
            sort_relations(&mut note.frontmatter.relations);
            self.write_note(&key, &note)?;
            changed += 1;
        }
        // Symmetric-complete any reciprocals the per-note lift didn't reach.
        self.reconcile_relations(true)?;
        Ok(changed)
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

    /// Load an entry's AI-metadata sidecar if one exists.
    pub fn load_ai(&self, key: &str) -> Result<Option<AiMetadata>> {
        let path = self.ai_path(key);
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(AiMetadata::parse(&text, &path)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(BibError::io(&path, e)),
        }
    }

    /// Write an entry's AI-metadata sidecar. This is the *only* path that writes AI data,
    /// and it writes solely to `ai/<key>.yml` — it never touches the note or entry, keeping
    /// the "AI output never overwrites curated fields" boundary structural.
    pub fn write_ai(&self, key: &str, ai: &AiMetadata) -> Result<PathBuf> {
        let path = self.ai_path(key);
        ensure_parent(&path)?;
        fs::write(&path, ai.to_yaml()?).map_err(|e| BibError::io(&path, e))?;
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

    // ---- Projects & usage (see docs/M2-SPEC.md §5) ------------------------------------

    pub fn project_path(&self, slug: &str) -> PathBuf {
        self.root.join(PROJECTS_DIR).join(format!("{slug}.yml"))
    }

    pub fn project_slugs(&self) -> Result<Vec<String>> {
        self.dir_stems(PROJECTS_DIR, "yml")
    }

    pub fn load_project(&self, slug: &str) -> Result<Project> {
        let path = self.project_path(slug);
        let text = fs::read_to_string(&path).map_err(|e| BibError::io(&path, e))?;
        Project::parse(&text, &path)
    }

    pub fn save_project(&self, slug: &str, project: &Project) -> Result<PathBuf> {
        let path = self.project_path(slug);
        ensure_parent(&path)?;
        fs::write(&path, project.to_text()?).map_err(|e| BibError::io(&path, e))?;
        Ok(path)
    }

    /// Scan every project's declared Typst documents for `@key` citations and build the
    /// reverse "used in" map. This is **derived** state: it is persisted under `.kartoteka/`
    /// (never into notes), rebuildable at any time. A document path that can't be read is
    /// skipped and recorded in `unreadable`, not fatal.
    pub fn scan_usage(&self) -> Result<UsageMap> {
        let mut map = UsageMap::default();
        for slug in self.project_slugs()? {
            let project = self.load_project(&slug)?;
            for doc in &project.documents {
                let path = expand_tilde(doc);
                match fs::read_to_string(&path) {
                    Ok(src) => {
                        for key in project::scan_typst_citation_keys(&src) {
                            let uses = map.by_key.entry(key).or_default();
                            let entry = (slug.clone(), path.display().to_string());
                            if !uses.contains(&entry) {
                                uses.push(entry);
                            }
                        }
                    }
                    Err(_) => map.unreadable.push((slug.clone(), path)),
                }
            }
        }
        for uses in map.by_key.values_mut() {
            uses.sort();
        }
        Ok(map)
    }

    /// Scan usage and persist it to `.kartoteka/usage.json` (derived, gitignored). Returns
    /// the map so callers need not re-read it.
    pub fn write_usage(&self) -> Result<UsageMap> {
        let map = self.scan_usage()?;
        let dir = self.root.join(DERIVED_DIR);
        fs::create_dir_all(&dir).map_err(|e| BibError::io(&dir, e))?;
        let path = dir.join("usage.json");
        let json = serde_json::to_string_pretty(&map.by_key).map_err(|e| BibError::Yaml {
            path: path.clone(),
            message: e.to_string(),
        })?;
        fs::write(&path, json).map_err(|e| BibError::io(&path, e))?;
        Ok(map)
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

        // Typed relations: report-only reconciliation (repair is `reconcile_relations(true)`).
        report.relations = self.reconcile_relations(false)?;

        // Projects: document paths that don't resolve to a readable file.
        for slug in self.project_slugs()? {
            let project = self.load_project(&slug)?;
            for doc in &project.documents {
                if !expand_tilde(doc).is_file() {
                    report
                        .dangling_project_docs
                        .push((slug.clone(), doc.display().to_string()));
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

/// Expand a leading `~/` (or bare `~`) in a path using `$HOME`. Any other path is returned
/// unchanged. Kept dependency-free (no `shellexpand`) since this is the only place in the
/// crate that needs it and only the home shorthand matters for project document paths.
fn expand_tilde(path: &Path) -> PathBuf {
    let Some(s) = path.to_str() else {
        return path.to_path_buf();
    };
    let Ok(home) = std::env::var("HOME") else {
        return path.to_path_buf();
    };
    if s == "~" {
        PathBuf::from(home)
    } else if let Some(rest) = s.strip_prefix("~/") {
        PathBuf::from(home).join(rest)
    } else {
        path.to_path_buf()
    }
}

/// The reverse "used in" map produced by [`Library::scan_usage`]: for each citation key, the
/// `(project slug, document path)` pairs that cite it. Derived, never authoritative.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct UsageMap {
    /// `key -> [(project slug, document path), …]`.
    pub by_key: HashMap<String, Vec<(String, String)>>,
    /// `(project slug, path)` documents that could not be read during the scan.
    #[serde(skip)]
    pub unreadable: Vec<(String, PathBuf)>,
}

/// The edge that `subject`'s target must carry pointing back at `subject`. For an
/// asymmetric predicate this is the inverse predicate as an `inverse: true` edge; for the
/// self-inverse `Related` it is a plain forward edge (symmetric).
fn counterpart(predicate: Predicate, subject: &str) -> Relation {
    Relation {
        predicate: predicate.inverse(),
        target: subject.to_string(),
        inverse: !predicate.is_self_inverse(),
    }
}

/// Normalize a requested forward-edge list for `key`: drop self-links, force `inverse`
/// off, and dedup by `(predicate, target)` identity (first occurrence wins).
fn normalize_forward(key: &str, forward: &[Relation]) -> Vec<Relation> {
    let mut seen: HashSet<(Predicate, String)> = HashSet::new();
    let mut out = Vec::new();
    for r in forward {
        if r.target == key {
            continue;
        }
        if seen.insert((r.predicate, r.target.clone())) {
            out.push(Relation::forward(r.predicate, r.target.clone()));
        }
    }
    out
}

/// Stable ordering for a note's relation list: forward edges before inverse, then by
/// predicate name, then target — so diffs stay minimal across rewrites.
fn sort_relations(relations: &mut [Relation]) {
    relations.sort_by(|a, b| {
        a.inverse
            .cmp(&b.inverse)
            .then_with(|| predicate_key(a.predicate).cmp(predicate_key(b.predicate)))
            .then_with(|| a.target.cmp(&b.target))
    });
}

/// A stable sort key for a predicate (its serialized name).
fn predicate_key(p: Predicate) -> &'static str {
    use Predicate::*;
    match p {
        Cites => "cites",
        CitedBy => "cited-by",
        Critiques => "critiques",
        CritiquedBy => "critiqued-by",
        Reviews => "reviews",
        ReviewedBy => "reviewed-by",
        CommentaryOn => "commentary-on",
        HasCommentary => "has-commentary",
        TranslationOf => "translation-of",
        HasTranslation => "has-translation",
        EditionOf => "edition-of",
        HasEdition => "has-edition",
        Supersedes => "supersedes",
        SupersededBy => "superseded-by",
        RepliesTo => "replies-to",
        RepliedToBy => "replied-to-by",
        Expands => "expands",
        ExpandedBy => "expanded-by",
        Related => "related",
    }
}

/// The result of [`Library::reconcile_relations`]. Empty means every note's maintained
/// relation edges already match the forward edges that are the source of truth.
#[derive(Debug, Default)]
pub struct RelationReconcile {
    /// `(note key, predicate, target)` — a maintained edge that should exist but didn't.
    pub missing: Vec<(String, Predicate, String)>,
    /// `(note key, predicate, target)` — an `inverse` edge with no forward edge behind it.
    pub orphaned: Vec<(String, Predicate, String)>,
    /// `(note key, predicate, target)` — a forward edge whose target has no entry.
    pub dangling_targets: Vec<(String, Predicate, String)>,
}

impl RelationReconcile {
    pub fn is_clean(&self) -> bool {
        self.missing.is_empty() && self.orphaned.is_empty() && self.dangling_targets.is_empty()
    }
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
    /// Typed-relation reconciliation findings (desynced inverse edges, dangling targets).
    /// Repaired by [`Library::reconcile_relations`]`(true)`.
    pub relations: RelationReconcile,
    /// `(project slug, document path)` where a project's declared document does not exist.
    pub dangling_project_docs: Vec<(String, String)>,
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
            + self.relations.missing.len()
            + self.relations.orphaned.len()
            + self.relations.dangling_targets.len()
            + self.dangling_project_docs.len()
    }
}
