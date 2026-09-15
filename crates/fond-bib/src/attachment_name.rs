//! Human-readable attachment filenames derived from citation info.
//!
//! Distinct from `key::generate_base_key`'s compact, all-lowercase `smith2020deep` citation
//! keys (meant to be typed as `@key` references) — this is for the file a user actually
//! sees on disk (`Attachment::filename`, shown in the attachment list, reader window title,
//! etc.), so it keeps natural casing and spacing instead of folding to a machine key.

use hayagriva::Entry as HEntry;

use crate::creator;
use crate::entry;

/// Titles are cut at a word boundary within this many characters, so a long title doesn't
/// produce an unwieldy filename.
const MAX_TITLE_CHARS: usize = 60;

/// Build a filename like `Smith 2020 - Attention Is All You Need.pdf` from an entry's
/// author/year/title. `ext` is the source file's extension with no leading dot (empty for
/// none). Returns `None` when the entry has neither a usable creator nor a title yet — e.g.
/// a bare stub attached to before identification — so the caller can fall back to the
/// source file's own name.
pub fn citation_filename(entry: &HEntry, ext: &str) -> Option<String> {
    let author = primary_author_display(entry);
    let year = entry::year(entry).map(|y| y.to_string());
    let title = entry::title_string(entry)
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .map(|t| shorten_title(&t));

    if author.is_none() && year.is_none() && title.is_none() {
        return None;
    }

    let mut stem = String::new();
    if let Some(a) = &author {
        stem.push_str(a);
    }
    if let Some(y) = &year {
        if !stem.is_empty() {
            stem.push(' ');
        }
        stem.push_str(y);
    }
    if let Some(t) = &title {
        if !stem.is_empty() {
            stem.push_str(" - ");
        }
        stem.push_str(t);
    }

    let stem = sanitize(&stem);
    if stem.is_empty() {
        return None;
    }
    Some(if ext.is_empty() {
        stem
    } else {
        format!("{stem}.{ext}")
    })
}

/// The primary creator group's family names as a short display string: a lone author's
/// family name, two authors joined with `&`, or the first plus "et al." for three or
/// more — the same primary-role selection `creator::sort_family_name` uses, just kept
/// short instead of folded to a single name.
fn primary_author_display(entry: &HEntry) -> Option<String> {
    let creators = creator::parse_creators(entry);
    let primary_role = creators.first()?.role;
    let families: Vec<&str> = creators
        .iter()
        .filter(|c| c.role == primary_role)
        .map(|c| c.family.trim())
        .filter(|f| !f.is_empty())
        .collect();

    match families.as_slice() {
        [] => None,
        [a] => Some(a.to_string()),
        [a, b] => Some(format!("{a} & {b}")),
        [a, ..] => Some(format!("{a} et al.")),
    }
}

/// Cut `title` at a word boundary within `MAX_TITLE_CHARS`. A no-op for a title already
/// under budget.
fn shorten_title(title: &str) -> String {
    if title.chars().count() <= MAX_TITLE_CHARS {
        return title.to_string();
    }
    let mut out = String::new();
    for word in title.split_whitespace() {
        let sep = usize::from(!out.is_empty());
        if !out.is_empty() && out.chars().count() + sep + word.chars().count() > MAX_TITLE_CHARS {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.is_empty() {
        // A single word longer than the whole budget: hard-truncate it.
        out = title.chars().take(MAX_TITLE_CHARS).collect();
    }
    out
}

/// Disambiguate `candidate` against filenames already used by other attachments on the same
/// entry (e.g. a preprint and its published PDF would otherwise both render "Smith 2020 -
/// Title.pdf"): appends " (2)", " (3)", … before the extension until the result is unique.
pub fn dedupe(candidate: &str, taken: &[String]) -> String {
    if !taken.iter().any(|t| t == candidate) {
        return candidate.to_string();
    }
    let (stem, ext) = match candidate.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), Some(e.to_string())),
        _ => (candidate.to_string(), None),
    };
    let mut n = 2;
    loop {
        let attempt = match &ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        if !taken.iter().any(|t| t == &attempt) {
            return attempt;
        }
        n += 1;
    }
}

/// Strip characters illegal in filenames and collapse whitespace, keeping the result
/// otherwise readable (letters, digits, spaces, and ordinary punctuation survive).
fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = false;
    for ch in s.chars() {
        if matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || ch.is_control() {
            continue;
        }
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out.trim().trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::parse_single;
    use std::path::Path;

    fn parse(yaml: &str) -> HEntry {
        parse_single(yaml, Path::new("t.yml")).unwrap().entry
    }

    #[test]
    fn single_author_with_year_and_title() {
        let e = parse("k:\n  type: article\n  title: Attention Is All You Need\n  author:\n    - Smith, John\n  date: 2020\n");
        assert_eq!(
            citation_filename(&e, "pdf").as_deref(),
            Some("Smith 2020 - Attention Is All You Need.pdf")
        );
    }

    #[test]
    fn two_authors_joined_with_ampersand() {
        let e = parse("k:\n  type: article\n  title: T\n  author:\n    - Smith, John\n    - Doe, Jane\n  date: 1999\n");
        assert_eq!(
            citation_filename(&e, "pdf").as_deref(),
            Some("Smith & Doe 1999 - T.pdf")
        );
    }

    #[test]
    fn three_or_more_authors_use_et_al() {
        let e = parse("k:\n  type: article\n  title: T\n  author:\n    - Smith, John\n    - Doe, Jane\n    - Roe, Rick\n  date: 1999\n");
        assert_eq!(
            citation_filename(&e, "pdf").as_deref(),
            Some("Smith et al. 1999 - T.pdf")
        );
    }

    #[test]
    fn no_extension_omits_dot() {
        let e =
            parse("k:\n  type: article\n  title: T\n  author:\n    - Smith, John\n  date: 1999\n");
        assert_eq!(citation_filename(&e, "").as_deref(), Some("Smith 1999 - T"));
    }

    #[test]
    fn no_author_falls_back_to_title_and_year() {
        let e = parse("k:\n  type: article\n  title: A Fragment\n  date: 1500\n");
        assert_eq!(
            citation_filename(&e, "pdf").as_deref(),
            Some("1500 - A Fragment.pdf")
        );
    }

    #[test]
    fn bare_stub_entry_has_nothing_to_build_from() {
        let e = parse("k:\n  type: article\n  title: \" \"\n");
        assert_eq!(citation_filename(&e, "pdf"), None);
    }

    #[test]
    fn long_title_is_cut_at_a_word_boundary() {
        let title = "This Is A Very Long Title That Goes On And On Far Past Any Reasonable Filename Length Budget For A Single Attachment";
        let yaml = format!("k:\n  type: article\n  title: \"{title}\"\n  author:\n    - Smith, John\n  date: 2020\n");
        let e = parse(&yaml);
        let name = citation_filename(&e, "pdf").unwrap();
        assert!(name.len() < title.len());
        assert!(name.starts_with("Smith 2020 - This Is A Very Long Title"));
        assert!(!name.contains('\n'));
    }

    #[test]
    fn dedupe_leaves_unique_names_untouched() {
        assert_eq!(dedupe("Smith 2020 - T.pdf", &[]), "Smith 2020 - T.pdf");
    }

    #[test]
    fn dedupe_numbers_a_repeated_name() {
        let taken = vec!["Smith 2020 - T.pdf".to_string()];
        assert_eq!(
            dedupe("Smith 2020 - T.pdf", &taken),
            "Smith 2020 - T (2).pdf"
        );
    }

    #[test]
    fn dedupe_skips_past_multiple_taken_numbers() {
        let taken = vec![
            "Smith 2020 - T.pdf".to_string(),
            "Smith 2020 - T (2).pdf".to_string(),
        ];
        assert_eq!(
            dedupe("Smith 2020 - T.pdf", &taken),
            "Smith 2020 - T (3).pdf"
        );
    }

    #[test]
    fn illegal_filename_characters_are_stripped() {
        let e = parse("k:\n  type: article\n  title: \"Question? / Slash: Test\"\n  author:\n    - Smith, John\n  date: 2020\n");
        let name = citation_filename(&e, "pdf").unwrap();
        assert!(!name.contains(['/', '?', ':']));
    }
}
