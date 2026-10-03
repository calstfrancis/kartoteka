//! Typst citation text for a key, optionally with a page or other locator — what "Cite" puts
//! on the clipboard. UI-agnostic so Zerkalo and Pereplyot can produce exactly the same text.
//!
//! Typst's reference syntax carries a locator as a supplement in square brackets:
//! `@cone1970black[p. 12]`.

/// A Typst citation for `key`, with `locator` (a page, page range, or free text like
/// `ch. 3`) as its supplement. An empty or blank locator gives the bare `@key`.
///
/// A bare page number or range is given its label — `12` → `p. 12`, `12-14` → `pp. 12–14`,
/// roman numerals included (`xiv` → `p. xiv`) — while anything else is used as typed, so
/// `ch. 3`, `§ 4` and `p. 12` all pass through. Characters that mean something in Typst
/// markup are escaped so a stray `]` or `#` in a locator can't break the document.
pub fn typst_citation(key: &str, locator: Option<&str>) -> String {
    let locator = locator.map(str::trim).filter(|l| !l.is_empty());
    match locator {
        Some(l) => format!("@{key}[{}]", escape_content(&label_locator(l))),
        None => format!("@{key}"),
    }
}

/// Add `p.`/`pp.` to a bare page or page range; leave anything else alone.
fn label_locator(locator: &str) -> String {
    let is_page =
        |s: &str| !s.is_empty() && (s.chars().all(|c| c.is_ascii_digit()) || is_roman_numeral(s));
    if is_page(locator) {
        return format!("p. {locator}");
    }
    // A range, with a hyphen, en dash or em dash (optionally spaced) between two pages.
    if let Some((from, to)) = locator.split_once(['-', '–', '—']) {
        let (from, to) = (from.trim(), to.trim());
        if is_page(from) && is_page(to) {
            return format!("pp. {from}–{to}");
        }
    }
    locator.to_string()
}

/// Whether `s` is a well-formed roman numeral (`xiv`, `XX`) — front-matter page numbers. Checked
/// by converting to a value and back to the canonical spelling, so a word that merely uses
/// roman letters (`vivid`, `civil`, `dim`) is not mistaken for one.
fn is_roman_numeral(s: &str) -> bool {
    let value = |c: char| match c.to_ascii_lowercase() {
        'i' => Some(1i32),
        'v' => Some(5),
        'x' => Some(10),
        'l' => Some(50),
        'c' => Some(100),
        'd' => Some(500),
        'm' => Some(1000),
        _ => None,
    };
    let Some(values) = s.chars().map(value).collect::<Option<Vec<i32>>>() else {
        return false;
    };
    if values.is_empty() || values.len() > 15 {
        return false;
    }
    let mut total = 0i32;
    for (i, v) in values.iter().enumerate() {
        match values.get(i + 1) {
            Some(next) if v < next => total -= v,
            _ => total += v,
        }
    }
    u32::try_from(total).is_ok_and(|n| to_roman(n).eq_ignore_ascii_case(s))
}

fn to_roman(mut n: u32) -> String {
    const TABLE: [(u32, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut out = String::new();
    for (value, text) in TABLE {
        while n >= value {
            out.push_str(text);
            n -= value;
        }
    }
    out
}

/// Escape what Typst markup would otherwise act on inside `[...]`.
pub(crate) fn escape_content(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '[' | ']' | '#' | '$' | '*' | '_' | '`' | '@' | '<' | '>' | '~'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_locator_is_the_bare_reference() {
        assert_eq!(typst_citation("cone1970black", None), "@cone1970black");
        assert_eq!(typst_citation("cone1970black", Some("")), "@cone1970black");
        assert_eq!(
            typst_citation("cone1970black", Some("   ")),
            "@cone1970black"
        );
    }

    #[test]
    fn bare_pages_and_ranges_get_their_label() {
        let c = |l| typst_citation("k", Some(l));
        assert_eq!(c("12"), "@k[p. 12]");
        assert_eq!(c(" 12 "), "@k[p. 12]");
        assert_eq!(c("xiv"), "@k[p. xiv]");
        assert_eq!(c("12-14"), "@k[pp. 12–14]");
        assert_eq!(c("12 – 14"), "@k[pp. 12–14]");
        assert_eq!(c("xiv-xx"), "@k[pp. xiv–xx]");
        assert_eq!(c("XLII"), "@k[p. XLII]");
        // Malformed numerals are free text, not pages.
        assert_eq!(c("iiii"), "@k[iiii]");
        assert_eq!(c("vx"), "@k[vx]");
    }

    #[test]
    fn anything_else_is_used_as_typed() {
        let c = |l| typst_citation("k", Some(l));
        assert_eq!(c("p. 12"), "@k[p. 12]");
        assert_eq!(c("ch. 3"), "@k[ch. 3]");
        assert_eq!(c("§ 4"), "@k[§ 4]");
        // Words that merely use roman-numeral letters are not mistaken for pages.
        assert_eq!(c("vivid"), "@k[vivid]");
        assert_eq!(c("civil"), "@k[civil]");
        assert_eq!(c("dim"), "@k[dim]");
    }

    #[test]
    fn typst_markup_in_a_locator_cannot_break_the_citation() {
        assert_eq!(typst_citation("k", Some("12] #bad")), "@k[12\\] \\#bad]");
        assert_eq!(typst_citation("k", Some("a_b*c")), "@k[a\\_b\\*c]");
    }

    #[test]
    fn the_citation_is_found_again_by_the_usage_scanner() {
        let text = typst_citation("cone1970black", Some("12-14"));
        assert_eq!(
            crate::project::scan_typst_citation_keys(&text),
            vec!["cone1970black"]
        );
    }
}
