//! `kartoteka` — the headless command surface. Thin orchestration over the `fond-*`
//! crates; reusable logic lives in those crates, not here (`docs/ARCHITECTURE.md` §2).

use std::error::Error;
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use fond_bib::entry;
use fond_bib::{FsckReport, Library};
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
    /// Regenerate library.yml (and, later, the search index) fully from the files.
    Reindex,
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

        Command::Reindex => {
            let library = Library::open(&cli.library)?;
            library.regenerate_library_yml()?;
            let n = library.keys_sorted()?.len();
            println!("Regenerated library.yml ({n} entries)");
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
    println!("\n{} problem(s) found", report.problem_count());
}
