//! Small dependency-free helpers.

use std::io::Write;
use std::path::Path;

/// Write `contents` to `path` atomically: into a temp file next to it, fsynced, then renamed
/// over the target. A crash or full disk mid-write leaves the previous file intact instead of
/// a truncated one — and edits here are read-modify-rewrite of the very file being edited, so
/// with a plain `fs::write` an interrupted save could destroy an entry or note outright. (A
/// truncated attachment blob was worse: `store_attachment` skips writing when the blob path
/// exists, so it was never repaired by re-importing.) The temp name has a `.tmp` extension so
/// directory scans by `.yml`/`.md`/`.json` never mistake it for a record.
pub(crate) fn write_atomic(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_ref())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Split leading `---`-delimited YAML frontmatter from a Markdown body. Returns
/// `(yaml, body)` or `None` when there is no frontmatter block. Handles `\n` and `\r\n`
/// line endings. Shared by the note and node parsers so the split stays byte-identical
/// across both file types (`docs/M3-SPEC.md` §1).
pub(crate) fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    // Find a closing fence line: a line that is exactly "---".
    let mut search_start = 0;
    loop {
        let idx = rest[search_start..].find("---")?;
        let abs = search_start + idx;
        let at_line_start = abs == 0 || rest.as_bytes()[abs - 1] == b'\n';
        let after = &rest[abs + 3..];
        let closes_line = after.is_empty() || after.starts_with('\n') || after.starts_with("\r\n");
        if at_line_start && closes_line {
            let yaml = &rest[..abs];
            let body = after
                .strip_prefix('\n')
                .or_else(|| after.strip_prefix("\r\n"))
                .unwrap_or(after);
            return Some((yaml, body));
        }
        search_start = abs + 3;
    }
}

pub use fond_annot::util::today_iso;
