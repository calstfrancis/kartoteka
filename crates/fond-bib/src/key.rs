//! Citation-key generation and collision handling.
//!
//! Scheme (see `docs/DATA-MODEL.md`): `<lastname><year><titleword>`, all lowercase ASCII.
//! First author's family name (ASCII-folded), 4-digit year (`nodate` if absent), first
//! significant title word. With no author the title word takes the lastname slot. On
//! collision the first claimant keeps the bare key; later ones get a `b`/`c`/… suffix.

use std::collections::HashSet;

use crate::error::{BibError, Result};

/// Leading articles skipped when picking the first "significant" title word. A pragmatic
/// multi-language set; extend as needed.
const ARTICLES: &[&str] = &[
    "a", "an", "the", "le", "la", "les", "un", "une", "der", "die", "das", "el", "los", "las",
    "il", "lo", "gli", "i", "os", "as", "de", "het", "een",
];

/// Fold a string to lowercase ASCII alphanumerics, mapping common Latin diacritics to
/// their base letter and dropping everything else (spaces, punctuation, unmapped
/// non-ASCII). `"Gutiérrez"` → `"gutierrez"`, `"The Destiny"` → `"thedestiny"`.
pub fn ascii_fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            continue;
        }
        if ch.is_ascii() {
            // ASCII punctuation / whitespace: drop.
            continue;
        }
        let lc = ch.to_lowercase().next().unwrap_or(ch);
        match lc {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => out.push('a'),
            'æ' => out.push_str("ae"),
            'ç' | 'ć' | 'č' | 'ċ' => out.push('c'),
            'ď' | 'đ' | 'ð' => out.push('d'),
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => out.push('e'),
            'ĝ' | 'ğ' | 'ġ' | 'ģ' => out.push('g'),
            'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ĭ' | 'į' | 'ı' => out.push('i'),
            'ĵ' => out.push('j'),
            'ķ' => out.push('k'),
            'ĺ' | 'ļ' | 'ľ' | 'ł' => out.push('l'),
            'ñ' | 'ń' | 'ņ' | 'ň' => out.push('n'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => out.push('o'),
            'œ' => out.push_str("oe"),
            'ŕ' | 'ř' | 'ŗ' => out.push('r'),
            'ś' | 'š' | 'ş' | 'ș' => out.push('s'),
            'ß' => out.push_str("ss"),
            'ţ' | 'ť' | 'ț' => out.push('t'),
            'þ' => out.push_str("th"),
            'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => out.push('u'),
            'ý' | 'ÿ' => out.push('y'),
            'ź' | 'ž' | 'ż' => out.push('z'),
            other => {
                // Unmapped non-ASCII: keep it only if it happens to be alphanumeric in
                // its own right (e.g. digits from other scripts are rare here); else drop.
                if other.is_alphanumeric() && other.is_ascii() {
                    out.push(other);
                }
            }
        }
    }
    out
}

/// The first significant word of a title, already ASCII-folded. Skips a leading article.
fn first_significant_word(title: &str) -> Option<String> {
    let words: Vec<&str> = title.split_whitespace().collect();
    for w in &words {
        let folded = ascii_fold(w);
        if folded.is_empty() || ARTICLES.contains(&folded.as_str()) {
            continue;
        }
        return Some(folded);
    }
    // Title was all articles / unfoldable: fall back to the first foldable word.
    words
        .iter()
        .filter_map(|w| {
            let f = ascii_fold(w);
            if f.is_empty() {
                None
            } else {
                Some(f)
            }
        })
        .next()
}

/// Build the base (unsuffixed) citation key from an entry's parts.
///
/// - `family` — first author's family name, if any.
/// - `year` — publication year, if any (`nodate` otherwise).
/// - `title` — the title, if any.
///
/// With an author, the key is `<family><year><titleword>`. With no author, the first
/// significant title word takes the lastname slot: `<titleword><year>`. With neither
/// author nor title, the entry is unkeyable.
pub fn generate_base_key(
    family: Option<&str>,
    year: Option<i32>,
    title: Option<&str>,
) -> Result<String> {
    let year_part = year
        .map(|y| y.to_string())
        .unwrap_or_else(|| "nodate".to_string());

    let family_folded = family.map(ascii_fold).filter(|s| !s.is_empty());
    let title_word = title.and_then(first_significant_word);

    match (family_folded, title_word) {
        (Some(fam), Some(word)) => Ok(format!("{fam}{year_part}{word}")),
        (Some(fam), None) => Ok(format!("{fam}{year_part}")),
        (None, Some(word)) => Ok(format!("{word}{year_part}")),
        (None, None) => Err(BibError::UnkeyableEntry),
    }
}

/// Bijective base-25 suffix over `b..z`: 1→"b", 2→"c", … 25→"z", 26→"bb", …
/// Starts at `b` so a suffixed key is always visually distinct from the bare one.
fn nth_suffix(mut n: usize) -> String {
    const ALPHABET: &[u8] = b"bcdefghijklmnopqrstuvwxyz"; // 25 letters, 'a' excluded
    let mut bytes = Vec::new();
    while n > 0 {
        let rem = (n - 1) % 25;
        bytes.push(ALPHABET[rem]);
        n = (n - 1) / 25;
    }
    bytes.reverse();
    String::from_utf8(bytes).expect("ascii only")
}

/// Assign a free key given the base and the set of already-taken keys. The first claimant
/// of a base keeps it bare; each later collision gets the next free `b`/`c`/… suffix.
pub fn assign_key(base: &str, existing: &HashSet<String>) -> String {
    if !existing.contains(base) {
        return base.to_string();
    }
    let mut n = 1;
    loop {
        let candidate = format!("{base}{}", nth_suffix(n));
        if !existing.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_diacritics() {
        assert_eq!(ascii_fold("Gutiérrez"), "gutierrez");
        assert_eq!(ascii_fold("Berdyaev"), "berdyaev");
        assert_eq!(ascii_fold("Þórð"), "thord");
        assert_eq!(ascii_fold("O'Brien"), "obrien");
        assert_eq!(ascii_fold("von Balthasar"), "vonbalthasar");
    }

    #[test]
    fn base_key_with_author_and_title() {
        let k =
            generate_base_key(Some("Berdyaev"), Some(1937), Some("The Destiny of Man")).unwrap();
        assert_eq!(k, "berdyaev1937destiny");
    }

    #[test]
    fn base_key_skips_leading_article() {
        let k = generate_base_key(
            Some("Cone"),
            Some(1970),
            Some("Black Theology and Black Power"),
        )
        .unwrap();
        assert_eq!(k, "cone1970black");
    }

    #[test]
    fn base_key_no_author_uses_title_word() {
        let k = generate_base_key(None, Some(1500), Some("The Cloud of Unknowing")).unwrap();
        assert_eq!(k, "cloud1500");
    }

    #[test]
    fn base_key_no_date() {
        let k = generate_base_key(Some("Anon"), None, Some("Fragment")).unwrap();
        assert_eq!(k, "anonnodatefragment");
    }

    #[test]
    fn unkeyable_without_author_or_title() {
        assert!(matches!(
            generate_base_key(None, Some(2020), None),
            Err(BibError::UnkeyableEntry)
        ));
    }

    #[test]
    fn collision_suffixes_are_stable_and_start_at_b() {
        let mut existing = HashSet::new();
        let base = "smith2020faith";

        let k1 = assign_key(base, &existing);
        assert_eq!(k1, "smith2020faith");
        existing.insert(k1);

        let k2 = assign_key(base, &existing);
        assert_eq!(k2, "smith2020faithb");
        existing.insert(k2);

        let k3 = assign_key(base, &existing);
        assert_eq!(k3, "smith2020faithc");
        existing.insert(k3);
    }

    #[test]
    fn suffix_rolls_over_past_z() {
        assert_eq!(nth_suffix(1), "b");
        assert_eq!(nth_suffix(25), "z");
        assert_eq!(nth_suffix(26), "bb");
    }
}
