//! `kartoteka` — the headless command surface. Thin orchestration over the `fond-*`
//! crates; reusable logic lives in those crates, not here (`docs/ARCHITECTURE.md` §2).

use std::error::Error;
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use fond_bib::entry;
use fond_bib::{FsckReport, ImportReport, Library};
use fond_vault::{Identity, Vault};

type CliResult<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Parser)]
#[command(
    name = "kartoteka",
    version,
    about = "Plain-file reference manager and PDF library"
)]
struct Cli {
    /// Path to the library root.
    #[arg(long, short = 'L', global = true, default_value = ".")]
    library: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new library (layout + git repo) at PATH (or --library).
    Init {
        /// Where to create the library; defaults to --library.
        path: Option<PathBuf>,
    },
    /// Add one or more entries from a Hayagriva YAML snippet (--file or stdin). Keys are
    /// generated automatically; the snippet's keys are treated as placeholders.
    Add {
        /// Read the snippet from this file instead of stdin.
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// List entries.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show a single entry (and its note, if any).
    Show {
        key: String,
        #[arg(long)]
        json: bool,
    },
    /// Import entries from a BetterBibTeX / BibLaTeX .bib file. Citation keys are
    /// preserved. Prints a report of anything that did not map cleanly.
    Import {
        /// Path to the .bib file.
        #[arg(long = "from-bibtex")]
        from_bibtex: PathBuf,
        /// Overwrite entries whose key already exists (default: skip and report).
        #[arg(long)]
        overwrite: bool,
        /// Do not hash and copy referenced attachment files.
        #[arg(long)]
        no_attachments: bool,
        /// Resolve relative attachment paths against this directory.
        #[arg(long)]
        attachment_base: Option<PathBuf>,
        /// Augment with a Zotero zotero.sqlite: import collections and notes, matching
        /// items to entries by DOI then title+year.
        #[arg(long = "zotero-db")]
        zotero_db: Option<PathBuf>,
    },
    /// Acquire an entry from a DOI, arXiv id, ISBN, or a BibTeX file. A fresh citation key
    /// is generated. Network is used for --doi/--arxiv/--isbn.
    Acquire {
        /// DOI to look up (e.g. 10.1000/xyz or https://doi.org/10.1000/xyz).
        #[arg(long)]
        doi: Option<String>,
        /// arXiv id to look up (e.g. 2103.12345).
        #[arg(long)]
        arxiv: Option<String>,
        /// ISBN to look up (via OpenLibrary).
        #[arg(long)]
        isbn: Option<String>,
        /// Add from a local BibTeX file instead (offline).
        #[arg(long)]
        bibtex_file: Option<PathBuf>,
    },
    /// Drop a PDF in and get a populated entry: identify it (--doi/--arxiv/--isbn, else
    /// sniff a DOI or metadata from the PDF), then attach the PDF to the resulting entry.
    /// Uses PDFium for sniffing and page count.
    AddPdf {
        /// Path to the PDF.
        path: PathBuf,
        #[arg(long)]
        doi: Option<String>,
        #[arg(long)]
        arxiv: Option<String>,
        #[arg(long)]
        isbn: Option<String>,
        /// Attach to an existing entry with this key instead of acquiring a new one.
        #[arg(long)]
        key: Option<String>,
    },
    /// Render a collection as a formatted reference list.
    Bib {
        /// Collection slug (the collections/<slug>.yml basename).
        collection: String,
        /// Style name: sbl, chicago-notes, chicago-author-date, or a CSL id (e.g. apa).
        #[arg(long, default_value = "sbl")]
        style: String,
        /// Use a CSL style file instead of a built-in / archived style.
        #[arg(long)]
        style_file: Option<PathBuf>,
        /// Emit HTML instead of plain text.
        #[arg(long)]
        html: bool,
    },
    /// Emit a Typst annotated bibliography: each rendered reference paired with its note.
    AnnotatedBib {
        /// Collection slug.
        collection: String,
        /// Style name or CSL id (see `bib`).
        #[arg(long, default_value = "sbl")]
        style: String,
        /// Use a CSL style file instead of a built-in / archived style.
        #[arg(long)]
        style_file: Option<PathBuf>,
        /// Write the .typ to this file instead of stdout.
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
    /// Regenerate library.yml and the search index fully from the files. If PDFium is
    /// available, present PDF attachments are indexed too.
    Reindex,
    /// Full-text search over metadata, notes, annotations, and PDF text. Field scoping:
    /// author: title: tag: type: year:  (e.g. `kartoteka search author:cone tag:christology`).
    Search {
        #[arg(required = true)]
        query: Vec<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Show the annotations recorded for an entry.
    Annots {
        key: String,
        #[arg(long)]
        json: bool,
    },
    /// Extract the text layer from an entry's PDF attachment (needs PDFium; set
    /// PDFIUM_LIB_PATH if it is not on the system library path).
    PdfText { key: String },
    /// Import highlights/underlines/strikeouts embedded in an entry's PDF into its
    /// annotation sidecar (annots/<key>.json). Idempotent; needs PDFium.
    ImportAnnots { key: String },
    /// Export an entry's sidecar highlights into a copy of its PDF. Needs PDFium.
    ExportAnnots {
        key: String,
        /// Where to write the annotated PDF.
        #[arg(long, short)]
        output: PathBuf,
    },
    /// Check the library for structural problems. Exits non-zero if any are found.
    Fsck,
    /// Show git working-tree status.
    Status,
    /// Stage all tracked changes and commit.
    Commit {
        #[arg(short, long)]
        message: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> CliResult<ExitCode> {
    match cli.command {
        Command::Init { path } => {
            let target = path.unwrap_or_else(|| cli.library.clone());
            std::fs::create_dir_all(&target)?;
            Library::init(&target)?;
            Vault::init(&target)?;
            println!("Initialised library at {}", target.display());
            Ok(ExitCode::SUCCESS)
        }

        Command::Add { file } => {
            let input = match file {
                Some(path) => std::fs::read_to_string(&path)?,
                None => {
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf
                }
            };
            let library = Library::open(&cli.library)?;
            let keys = library.add_from_yaml(&input)?;
            for key in &keys {
                println!("added {key}");
            }
            Ok(ExitCode::SUCCESS)
        }

        Command::List { json } => {
            let library = Library::open(&cli.library)?;
            let keys = library.keys_sorted()?;
            if json {
                let mut items = Vec::new();
                for key in &keys {
                    let parsed = library.load_entry(key)?;
                    items.push(serde_json::json!({
                        "key": key,
                        "title": entry::title_string(&parsed.entry),
                        "author": entry::family_name(&parsed.entry),
                        "year": entry::year(&parsed.entry),
                    }));
                }
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else {
                for key in &keys {
                    let parsed = library.load_entry(key)?;
                    let title = entry::title_string(&parsed.entry).unwrap_or_default();
                    let author = entry::family_name(&parsed.entry).unwrap_or_default();
                    let year = entry::year(&parsed.entry)
                        .map(|y| y.to_string())
                        .unwrap_or_default();
                    println!("{key}\t{author}\t{year}\t{title}");
                }
            }
            Ok(ExitCode::SUCCESS)
        }

        Command::Show { key, json } => {
            let library = Library::open(&cli.library)?;
            let raw = library.read_entry_raw(&key)?;
            let note = library.load_note(&key)?;
            if json {
                let value = serde_json::json!({
                    "key": key,
                    "yaml": raw,
                    "note": note.as_ref().map(|n| serde_json::json!({
                        "frontmatter": n.frontmatter,
                        "body": n.body,
                    })),
                });
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                print!("{raw}");
                if !raw.ends_with('\n') {
                    println!();
                }
                if let Some(note) = note {
                    println!("\n--- note ---");
                    print!("{}", note.to_text()?);
                }
            }
            Ok(ExitCode::SUCCESS)
        }

        Command::Import {
            from_bibtex,
            overwrite,
            no_attachments,
            attachment_base,
            zotero_db,
        } => {
            let source = std::fs::read_to_string(&from_bibtex)?;
            let library = Library::open(&cli.library)?;
            let opts = fond_bib::ImportOptions {
                overwrite,
                copy_attachments: !no_attachments,
                attachment_base: attachment_base
                    .or_else(|| from_bibtex.parent().map(|p| p.to_path_buf())),
                zotero_db,
            };
            let report = library.import_bibtex(&source, &opts)?;
            print_import(&report);
            Ok(ExitCode::SUCCESS)
        }

        Command::Acquire {
            doi,
            arxiv,
            isbn,
            bibtex_file,
        } => {
            let library = Library::open(&cli.library)?;
            let keys = acquire_entry(&library, doi, arxiv, isbn, bibtex_file)?;
            if keys.is_empty() {
                return Err("no entries found in the acquired record".into());
            }
            for key in &keys {
                println!("acquired {key}");
            }
            Ok(ExitCode::SUCCESS)
        }

        Command::AddPdf {
            path,
            doi,
            arxiv,
            isbn,
            key,
        } => {
            let library = Library::open(&cli.library)?;

            // Resolve the entry the PDF belongs to.
            let target_key = if let Some(k) = key {
                if !library.entry_path(&k).exists() {
                    return Err(format!("no entry '{k}' to attach to").into());
                }
                k
            } else if doi.is_some() || arxiv.is_some() || isbn.is_some() {
                acquire_one(&library, doi, arxiv, isbn)?
            } else {
                identify_pdf(&library, &path)?
            };

            // Page count via PDFium if available; otherwise the record simply omits it.
            let pages = fond_doc::bind_pdfium().ok().and_then(|pdfium| {
                std::fs::read(&path)
                    .ok()
                    .and_then(|bytes| fond_doc::page_count(&pdfium, &bytes).ok())
                    .map(|n| n as u32)
            });

            let att = library.store_attachment(&target_key, &path, pages)?;
            println!("attached {} to {target_key} ({})", att.filename, att.hash);
            Ok(ExitCode::SUCCESS)
        }

        Command::Bib {
            collection,
            style,
            style_file,
            html,
        } => {
            let library = Library::open(&cli.library)?;
            let csl = load_style(&style, style_file.as_deref())?;
            let format = if html {
                fond_bib::BufWriteFormat::Html
            } else {
                fond_bib::BufWriteFormat::Plain
            };
            let rendered = library.bibliography_for_collection(&collection, &csl, format)?;
            for item in &rendered {
                println!("{}\n", item.text);
            }
            Ok(ExitCode::SUCCESS)
        }

        Command::AnnotatedBib {
            collection,
            style,
            style_file,
            output,
        } => {
            let library = Library::open(&cli.library)?;
            let csl = load_style(&style, style_file.as_deref())?;
            let typ = library.annotated_bibliography_typ(&collection, &csl)?;
            match output {
                Some(path) => {
                    std::fs::write(&path, typ)?;
                    println!("Wrote {}", path.display());
                }
                None => print!("{typ}"),
            }
            Ok(ExitCode::SUCCESS)
        }

        Command::Reindex => {
            let library = Library::open(&cli.library)?;
            library.regenerate_library_yml()?;

            // Extract PDF text only if PDFium is available; otherwise index without it.
            let pdfium = fond_doc::bind_pdfium().ok();
            let pdf_text = |key: &str| -> Option<String> {
                let pdfium = pdfium.as_ref()?;
                let attachments = library
                    .load_note(key)
                    .ok()
                    .flatten()
                    .map(|n| n.frontmatter.attachments)
                    .unwrap_or_default();
                for att in &attachments {
                    let hex = att
                        .hash
                        .split_once(':')
                        .map(|(_, h)| h)
                        .unwrap_or(&att.hash);
                    let path = library.attachment_blob_path(hex);
                    if path.exists() {
                        if let Ok(text) = fond_doc::extract_text_from_file(pdfium, &path) {
                            return Some(text.full_text());
                        }
                    }
                }
                None
            };

            let index_dir = cli.library.join(".kartoteka").join("index");
            fond_index::SearchIndex::rebuild(&library, &index_dir, pdf_text)?;
            let n = library.keys_sorted()?.len();
            let pdf_note = if pdfium.is_some() {
                ""
            } else {
                " (PDFium unavailable — PDF text not indexed)"
            };
            println!("Regenerated library.yml and search index ({n} entries){pdf_note}");
            Ok(ExitCode::SUCCESS)
        }

        Command::Search { query, limit, json } => {
            let index_dir = cli.library.join(".kartoteka").join("index");
            if !index_dir.exists() {
                return Err("no search index yet — run `kartoteka reindex` first".into());
            }
            let index = fond_index::SearchIndex::open(&index_dir)?;
            let hits = index.search(&query.join(" "), limit)?;
            if json {
                let items: Vec<_> = hits
                    .iter()
                    .map(|h| {
                        serde_json::json!({
                            "key": h.key, "score": h.score,
                            "title": h.title, "author": h.author, "year": h.year,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else if hits.is_empty() {
                println!("no matches");
            } else {
                for h in &hits {
                    println!(
                        "{:.2}\t{}\t{}\t{}\t{}",
                        h.score, h.key, h.author, h.year, h.title
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }

        Command::Annots { key, json } => {
            let library = Library::open(&cli.library)?;
            match library.load_annotations(&key)? {
                None => {
                    if json {
                        println!("null");
                    } else {
                        println!("no annotations for {key}");
                    }
                }
                Some(sidecar) => {
                    if json {
                        print!("{}", sidecar.to_json()?);
                    } else {
                        println!(
                            "{} annotation(s) for {key}{}",
                            sidecar.annotations.len(),
                            sidecar
                                .pdf_hash
                                .as_deref()
                                .map(|h| format!("  (pdf {h})"))
                                .unwrap_or_default()
                        );
                        for a in &sidecar.annotations {
                            let label = a.snippet.as_deref().or(a.note.as_deref()).unwrap_or("");
                            println!("  p{:<4} {:?}\t{}", a.page, a.kind, label);
                        }
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }

        Command::PdfText { key } => {
            let library = Library::open(&cli.library)?;
            let attachments = library
                .load_note(&key)?
                .map(|n| n.frontmatter.attachments)
                .unwrap_or_default();
            let blob = attachments.iter().find_map(|att| {
                let hex = att
                    .hash
                    .split_once(':')
                    .map(|(_, h)| h)
                    .unwrap_or(&att.hash);
                let path = library.attachment_blob_path(hex);
                path.exists().then_some(path)
            });
            let Some(path) = blob else {
                return Err(format!("no present PDF attachment for '{key}'").into());
            };
            let pdfium = fond_doc::bind_pdfium()?;
            let text = fond_doc::extract_text_from_file(&pdfium, &path)?;
            print!("{}", text.full_text());
            Ok(ExitCode::SUCCESS)
        }

        Command::ImportAnnots { key } => {
            let library = Library::open(&cli.library)?;
            let (blob, hash) = pdf_attachment(&library, &key)?;
            let pdfium = fond_doc::bind_pdfium()?;
            let bytes = std::fs::read(&blob)?;
            let extracted = fond_doc::extract_annotations(&pdfium, &bytes)?;

            let mut sidecar = library
                .load_annotations(&key)?
                .unwrap_or_else(|| fond_bib::AnnotationSidecar::new(&key));
            sidecar.pdf_hash = Some(hash);
            for a in &extracted {
                let kind = match a.kind {
                    fond_doc::PdfAnnotationKind::Highlight => fond_bib::AnnotationKind::Highlight,
                    fond_doc::PdfAnnotationKind::Underline => fond_bib::AnnotationKind::Underline,
                    fond_doc::PdfAnnotationKind::Strikeout => fond_bib::AnnotationKind::Strikeout,
                };
                let quads = a.quadpoints.iter().map(|q| q.map(|v| v as f64)).collect();
                sidecar.upsert(fond_bib::Annotation::imported(
                    kind,
                    a.page as u32,
                    quads,
                    a.snippet.clone(),
                    a.contents.clone(),
                ));
            }
            library.write_annotations(&sidecar)?;
            println!(
                "imported {} annotation(s) from PDF ({} total in sidecar)",
                extracted.len(),
                sidecar.annotations.len()
            );
            Ok(ExitCode::SUCCESS)
        }

        Command::ExportAnnots { key, output } => {
            let library = Library::open(&cli.library)?;
            let (blob, _hash) = pdf_attachment(&library, &key)?;
            let sidecar = library
                .load_annotations(&key)?
                .ok_or_else(|| format!("no annotations recorded for '{key}'"))?;

            let to_embed: Vec<fond_doc::AnnotationToEmbed> = sidecar
                .annotations
                .iter()
                .filter(|a| {
                    a.kind == fond_bib::AnnotationKind::Highlight && !a.quadpoints.is_empty()
                })
                .map(|a| fond_doc::AnnotationToEmbed {
                    page: a.page as u16,
                    quadpoints: a.quadpoints.iter().map(|q| q.map(|v| v as f32)).collect(),
                    contents: a.note.clone(),
                })
                .collect();
            if to_embed.is_empty() {
                return Err("no highlight annotations to export".into());
            }

            let pdfium = fond_doc::bind_pdfium()?;
            let bytes = std::fs::read(&blob)?;
            let annotated = fond_doc::embed_highlights(&pdfium, &bytes, &to_embed)?;
            std::fs::write(&output, annotated)?;
            println!(
                "wrote {} highlight(s) to {}",
                to_embed.len(),
                output.display()
            );
            Ok(ExitCode::SUCCESS)
        }

        Command::Fsck => {
            let library = Library::open(&cli.library)?;
            let report = library.fsck()?;
            print_fsck(&report);
            if report.is_clean() {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::FAILURE)
            }
        }

        Command::Status => {
            let vault = Vault::open(&cli.library)?;
            let status = vault.status()?;
            if status.is_clean() {
                println!("clean");
            } else {
                for p in &status.conflicted {
                    println!("conflicted  {p}");
                }
                for p in &status.staged {
                    println!("staged      {p}");
                }
                for p in &status.unstaged {
                    println!("unstaged    {p}");
                }
                for p in &status.untracked {
                    println!("untracked   {p}");
                }
            }
            Ok(ExitCode::SUCCESS)
        }

        Command::Commit { message } => {
            let vault = Vault::open(&cli.library)?;
            let identity = Identity::from_git_config()
                .map_err(|_| "no git identity: set user.name and user.email in your git config")?;
            vault.stage_all()?;
            let oid = vault.commit(&message, &identity)?;
            println!("committed {}", &oid[..oid.len().min(10)]);
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Find the first present PDF attachment blob for an entry; returns its path and hash.
fn pdf_attachment(library: &Library, key: &str) -> CliResult<(PathBuf, String)> {
    let attachments = library
        .load_note(key)?
        .map(|n| n.frontmatter.attachments)
        .unwrap_or_default();
    for att in &attachments {
        let hex = att
            .hash
            .split_once(':')
            .map(|(_, h)| h)
            .unwrap_or(&att.hash);
        let path = library.attachment_blob_path(hex);
        if path.exists() {
            return Ok((path, att.hash.clone()));
        }
    }
    Err(format!("no present PDF attachment for '{key}'").into())
}

fn acquire_entry(
    library: &Library,
    doi: Option<String>,
    arxiv: Option<String>,
    isbn: Option<String>,
    bibtex_file: Option<PathBuf>,
) -> CliResult<Vec<String>> {
    if let Some(doi) = doi {
        Ok(library.add_bibtex(&fond_bib::acquire::fetch_doi_bibtex(&doi)?)?)
    } else if let Some(arxiv) = arxiv {
        Ok(library.add_bibtex(&fond_bib::acquire::fetch_arxiv_bibtex(&arxiv)?)?)
    } else if let Some(isbn) = isbn {
        Ok(library.add_from_yaml(&fond_bib::acquire::fetch_isbn_yaml(&isbn)?)?)
    } else if let Some(path) = bibtex_file {
        Ok(library.add_bibtex(&std::fs::read_to_string(&path)?)?)
    } else {
        Err("provide --doi, --arxiv, --isbn, or --bibtex-file".into())
    }
}

fn acquire_one(
    library: &Library,
    doi: Option<String>,
    arxiv: Option<String>,
    isbn: Option<String>,
) -> CliResult<String> {
    acquire_entry(library, doi, arxiv, isbn, None)?
        .into_iter()
        .next()
        .ok_or_else(|| "acquisition produced no entry".into())
}

/// Identify a dropped PDF: sniff a DOI from its text, else build a minimal entry from its
/// embedded metadata. Returns the citation key of the created entry.
fn identify_pdf(library: &Library, path: &std::path::Path) -> CliResult<String> {
    let pdfium = fond_doc::bind_pdfium()
        .map_err(|e| format!("PDFium is needed to identify a PDF without an identifier: {e}"))?;
    let bytes = std::fs::read(path)?;

    if let Ok(text) = fond_doc::extract_text(&pdfium, &bytes) {
        if let Some(doi) = fond_doc::find_doi(&text.full_text()) {
            eprintln!("sniffed DOI {doi}");
            let bibtex = fond_bib::acquire::fetch_doi_bibtex(&doi)?;
            if let Some(key) = library.add_bibtex(&bibtex)?.into_iter().next() {
                return Ok(key);
            }
        }
    }

    let meta = fond_doc::extract_metadata(&pdfium, &bytes)?;
    if let Some(title) = meta.title {
        eprintln!("no DOI found; building an entry from PDF metadata");
        let yaml = fond_bib::acquire::minimal_book_yaml(&title, meta.author.as_deref())?;
        if let Some(key) = library.add_from_yaml(&yaml)?.into_iter().next() {
            return Ok(key);
        }
    }

    Err("could not identify the PDF; pass --doi, --arxiv, --isbn, or --key".into())
}

fn load_style(
    name: &str,
    style_file: Option<&std::path::Path>,
) -> CliResult<fond_bib::IndependentStyle> {
    match style_file {
        Some(path) => {
            let xml = std::fs::read_to_string(path)?;
            Ok(fond_bib::style_from_csl(&xml)?)
        }
        None => Ok(fond_bib::resolve_style(name)?),
    }
}

fn print_import(report: &ImportReport) {
    println!("Imported {} entries.", report.imported.len());

    let total_copied = report.attachments_copied.len();
    if total_copied > 0 {
        println!("Copied {total_copied} attachment(s) into attachments/.");
    }
    let total_tagged: usize = report.tags_written.iter().map(|(_, n)| n).sum();
    if total_tagged > 0 {
        println!(
            "Wrote {total_tagged} tag(s) across {} note(s).",
            report.tags_written.len()
        );
    }
    if !report.collections_created.is_empty() {
        println!(
            "Created {} collection(s) from Zotero.",
            report.collections_created.len()
        );
    }
    if report.notes_imported > 0 {
        println!(
            "Merged {} Zotero note(s) into entry notes.",
            report.notes_imported
        );
    }

    if report.is_clean() {
        println!("Everything mapped cleanly.");
        return;
    }

    println!("\n--- migration report ---");
    for key in &report.skipped_key_collisions {
        println!("skipped (exists)    {key}  (use --overwrite to replace)");
    }
    for (key, reason) in &report.skipped_unsafe_keys {
        println!("skipped (bad key)   {key}: {reason}");
    }
    for (key, path) in &report.attachments_missing {
        println!("attachment missing  {key}: {path}");
    }
    for (key, fields) in &report.unmapped_fields {
        println!("unmapped fields     {key}: {}", fields.join(", "));
    }
    for label in &report.zotero_unmatched {
        println!("zotero unmatched    {label}  (no entry matched by DOI or title+year)");
    }
    if report.standalone_notes_skipped > 0 {
        println!(
            "standalone notes    {} Zotero note(s) had no parent item to attach to",
            report.standalone_notes_skipped
        );
    }
    for warning in &report.parse_warnings {
        println!("warning             {warning}");
    }
}

fn print_fsck(report: &FsckReport) {
    if report.is_clean() {
        println!("clean — no problems found");
        return;
    }
    for (path, msg) in &report.unparseable_entries {
        println!("unparseable entry   {path}: {msg}");
    }
    for (file, inner) in &report.key_filename_mismatches {
        println!("key mismatch        entries/{file}.yml has inner key '{inner}'");
    }
    for (slug, key) in &report.dangling_collection_refs {
        println!("dangling reference  collection '{slug}' references missing key '{key}'");
    }
    for (key, hash) in &report.missing_attachments {
        println!("missing attachment  {key}: {hash}");
    }
    for name in &report.orphaned_attachments {
        println!("orphaned attachment attachments/{name}");
    }
    for (name, detail) in &report.hash_mismatched_attachments {
        println!("hash mismatch       attachments/{name}: {detail}");
    }
    for (path, msg) in &report.unparseable_annotations {
        println!("unparseable annots  {path}: {msg}");
    }
    for (file, inner) in &report.annotation_key_mismatches {
        println!("annot key mismatch  annots/{file}.json has inner key '{inner}'");
    }
    for (key, hash) in &report.annotation_pdf_unmatched {
        println!("annot pdf unmatched {key}: {hash} matches no recorded attachment");
    }
    println!("\n{} problem(s) found", report.problem_count());
}
