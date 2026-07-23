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
    /// Regenerate library.yml (and, later, the search index) fully from the files.
    Reindex,
    /// Show the annotations recorded for an entry.
    Annots {
        key: String,
        #[arg(long)]
        json: bool,
    },
    /// Extract the text layer from an entry's PDF attachment (needs PDFium; set
    /// PDFIUM_LIB_PATH if it is not on the system library path).
    PdfText { key: String },
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
            let n = library.keys_sorted()?.len();
            println!("Regenerated library.yml ({n} entries)");
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
