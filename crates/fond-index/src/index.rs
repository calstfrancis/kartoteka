//! The tantivy full-text search index over a library. Derived, disposable state: it lives
//! under `.kartoteka/index/` and is fully rebuildable from the authoritative files
//! (`docs/ARCHITECTURE.md` §1). It only ever *caches* what the files already say.
//!
//! Indexed fields: `kind`, `key`, `type`, `author`, `title`, `year`, `tag`, `facet`, plus the
//! un-stored bulk text (`alias`, `identifier`, `note`, `annotation`, `pdftext`, `ai`). A bare
//! query searches the text fields; field scoping works via the field names, e.g.
//! `author:cone tag:christology facet:discipline year:1970`. AI-generated text is a
//! separate `ai:` field so results from it can be filtered/scoped apart from curated text.
//!
//! Both `entries/` and `nodes/` are indexed into one schema, discriminated by `kind`
//! (`entry` or `node`), so a query can scope to/from nodes (`kind:node augustine`). A node
//! reuses `title` for its `label`, `type` for its `node-type`, and `note` for its body, and
//! adds `alias`/`identifier` text; nodes have no author/year (`docs/M3-SPEC.md` §5).

use std::path::Path;

use fond_bib::{entry, Library, NodeType};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, Schema, Value, STORED, STRING, TEXT};
use tantivy::{doc, Index, IndexWriter, TantivyDocument};

use crate::error::{IndexError, Result};

/// A plain, UI-agnostic search result. `kind` is `"entry"` or `"node"`; for a node hit
/// `title` carries the node label and `author`/`year` are empty.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub kind: String,
    pub key: String,
    pub score: f32,
    pub title: String,
    pub author: String,
    pub year: String,
}

#[derive(Clone, Copy)]
struct Fields {
    kind: Field,
    key: Field,
    type_: Field,
    author: Field,
    title: Field,
    year: Field,
    tag: Field,
    facet: Field,
    alias: Field,
    identifier: Field,
    note: Field,
    annotation: Field,
    pdftext: Field,
    ai: Field,
}

fn build_schema() -> Schema {
    let mut b = Schema::builder();
    b.add_text_field("kind", STRING | STORED);
    b.add_text_field("key", STRING | STORED);
    b.add_text_field("type", STRING | STORED);
    b.add_text_field("author", TEXT | STORED);
    b.add_text_field("title", TEXT | STORED);
    b.add_text_field("year", STRING | STORED);
    b.add_text_field("tag", TEXT | STORED);
    b.add_text_field("facet", TEXT | STORED);
    b.add_text_field("alias", TEXT);
    b.add_text_field("identifier", TEXT);
    b.add_text_field("note", TEXT);
    b.add_text_field("annotation", TEXT);
    b.add_text_field("pdftext", TEXT);
    b.add_text_field("ai", TEXT);
    b.build()
}

impl Fields {
    fn of(schema: &Schema) -> Fields {
        let f = |name: &str| schema.get_field(name).expect("known field");
        Fields {
            kind: f("kind"),
            key: f("key"),
            type_: f("type"),
            author: f("author"),
            title: f("title"),
            year: f("year"),
            tag: f("tag"),
            facet: f("facet"),
            alias: f("alias"),
            identifier: f("identifier"),
            note: f("note"),
            annotation: f("annotation"),
            pdftext: f("pdftext"),
            ai: f("ai"),
        }
    }
}

/// The kebab-case string for a node type, matching the on-disk `node-type:` value.
fn node_type_str(t: NodeType) -> &'static str {
    match t {
        NodeType::Person => "person",
        NodeType::Concept => "concept",
        NodeType::School => "school",
        NodeType::Event => "event",
        NodeType::Place => "place",
        NodeType::WorkUncataloged => "work-uncataloged",
    }
}

/// An opened search index, ready to query.
pub struct SearchIndex {
    index: Index,
    fields: Fields,
}

impl SearchIndex {
    fn from_index(index: Index) -> SearchIndex {
        let fields = Fields::of(&index.schema());
        SearchIndex { index, fields }
    }

    /// Open an existing index directory.
    pub fn open(dir: &Path) -> Result<SearchIndex> {
        let index = Index::open_in_dir(dir)?;
        Ok(Self::from_index(index))
    }

    /// Rebuild the index from scratch out of the library's authoritative files. `pdf_text`
    /// supplies extracted PDF text per key (so PDFium stays out of this crate); return
    /// `None` to index no PDF text for a key.
    pub fn rebuild<F>(library: &Library, dir: &Path, mut pdf_text: F) -> Result<SearchIndex>
    where
        F: FnMut(&str) -> Option<String>,
    {
        if dir.exists() {
            std::fs::remove_dir_all(dir).map_err(|e| IndexError::Io {
                path: dir.to_path_buf(),
                source: e,
            })?;
        }
        std::fs::create_dir_all(dir).map_err(|e| IndexError::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;

        let schema = build_schema();
        let fields = Fields::of(&schema);
        let index = Index::create_in_dir(dir, schema)?;
        let mut writer: IndexWriter = index.writer(50_000_000)?;

        for key in library.keys_sorted()? {
            let parsed = library.load_entry(&key)?;
            let e = &parsed.entry;

            let title = entry::title_string(e).unwrap_or_default();
            let author = entry::author_names(e);
            let year = entry::year(e).map(|y| y.to_string()).unwrap_or_default();
            let type_ = format!("{:?}", e.entry_type()).to_lowercase();

            let (tags, facets, note_body) = match library.load_note(&key)? {
                Some(note) => {
                    // Facets: index each faceted tag's name plus its full `name:value` so
                    // `facet:discipline` scoping finds items that carry that facet.
                    let facets = note
                        .frontmatter
                        .tags
                        .iter()
                        .filter_map(|t| {
                            fond_bib::split_facet(t)
                                .0
                                .map(|facet| format!("{facet} {t}"))
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    (note.frontmatter.tags.join(" "), facets, note.body)
                }
                None => (String::new(), String::new(), String::new()),
            };

            // AI-generated text, indexed into its own field so it can be scoped/filtered
            // apart from curated text (`ai:` in a query).
            let ai_text = library
                .load_ai(&key)?
                .map(|ai| {
                    let mut parts: Vec<String> = Vec::new();
                    parts.extend(ai.summary);
                    parts.extend(ai.keywords);
                    parts.extend(ai.concepts);
                    parts.extend(ai.claims);
                    parts.extend(ai.counterarguments);
                    parts.join(" ")
                })
                .unwrap_or_default();

            let annotation = library
                .load_annotations(&key)?
                .map(|s| {
                    s.annotations
                        .iter()
                        .flat_map(|a| a.snippet.iter().chain(a.note.iter()))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();

            let pdf = pdf_text(&key).unwrap_or_default();

            writer.add_document(doc!(
                fields.kind => "entry",
                fields.key => key.clone(),
                fields.type_ => type_,
                fields.author => author,
                fields.title => title,
                fields.year => year,
                fields.tag => tags,
                fields.facet => facets,
                fields.note => note_body,
                fields.annotation => annotation,
                fields.pdftext => pdf,
                fields.ai => ai_text,
            ))?;
        }

        // Nodes: indexed into the same schema, discriminated by `kind:node`. A node reuses
        // `title` (label), `type` (node-type), and `note` (body), plus `alias`/`identifier`.
        for slug in library.node_slugs()? {
            // An unparseable node is skipped (fsck reports it) rather than failing the build.
            let Ok(node) = library.load_node(&slug) else {
                continue;
            };
            let fm = &node.frontmatter;
            let aliases = fm.aliases.join(" ");
            // Index both each identifier's scheme and value ("wikidata Q8018"), so a query
            // can find a node by either.
            let identifiers = fm
                .identifiers
                .iter()
                .map(|(scheme, value)| format!("{scheme} {value}"))
                .collect::<Vec<_>>()
                .join(" ");

            writer.add_document(doc!(
                fields.kind => "node",
                fields.key => slug.clone(),
                fields.type_ => node_type_str(fm.node_type),
                fields.title => fm.label.clone(),
                fields.alias => aliases,
                fields.identifier => identifiers,
                fields.note => node.body.clone(),
            ))?;
        }

        writer.commit()?;
        Ok(SearchIndex { index, fields })
    }

    /// Run a query. Bare terms search the text fields; field scoping (`author:`, `tag:`,
    /// `type:`, `year:`, `title:`, `kind:`) works via field names — e.g. `kind:node augustine`
    /// to restrict to nodes, or `type:person` for person nodes.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();

        let default_fields = vec![
            self.fields.title,
            self.fields.author,
            self.fields.tag,
            self.fields.facet,
            self.fields.alias,
            self.fields.identifier,
            self.fields.note,
            self.fields.annotation,
            self.fields.pdftext,
            self.fields.ai,
        ];
        let parser = QueryParser::for_index(&self.index, default_fields);
        let query = parser
            .parse_query(query)
            .map_err(|e| IndexError::Query(e.to_string()))?;

        let top = searcher.search(&query, &TopDocs::with_limit(limit))?;
        let mut hits = Vec::with_capacity(top.len());
        for (score, address) in top {
            let doc: TantivyDocument = searcher.doc(address)?;
            let get = |field: Field| {
                doc.get_first(field)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            hits.push(SearchHit {
                kind: get(self.fields.kind),
                key: get(self.fields.key),
                score,
                title: get(self.fields.title),
                author: get(self.fields.author),
                year: get(self.fields.year),
            });
        }
        Ok(hits)
    }
}
