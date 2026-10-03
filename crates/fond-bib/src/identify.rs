//! Work out what someone pasted into the "Add" box: a DOI, an arXiv paper, an ISBN, a web link,
//! a BibTeX record, or just words (a title to search for).
//!
//! Pure text analysis — no network, no UI types — so every front end (Kartoteka's Add box, the
//! CLI, Zerkalo) classifies input the same way. The network half lives in [`crate::acquire`].
//!
//! The rule that keeps this trustworthy: **never guess when a wrong guess would add the wrong
//! thing.** An ISBN must have a valid check digit, a bare arXiv id must be exactly an id, and a
//! DOI inside a sentence is found only at a word boundary. Anything unsure falls through to
//! [`Identified::Text`], which only ever *searches* and never adds on its own.

/// What an input string is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identified {
    /// Nothing but whitespace.
    Empty,
    /// A DOI, bare (`10.1000/xyz`) — pulled out of a `doi.org` link, a publisher URL, a
    /// `doi:` prefix, or a sentence that contains one.
    Doi(String),
    /// An arXiv identifier, bare (`1706.03762`, `math/0211159v2`).
    Arxiv(String),
    /// An ISBN with a valid check digit, digits only (a final `X` kept as `X`).
    Isbn(String),
    /// Any other `http(s)` link: the page itself is read for citation metadata.
    Url(String),
    /// A BibTeX/BibLaTeX record (`@article{…}`).
    BibTeX(String),
    /// Free text: treated as a title/citation to search for.
    Text(String),
}

impl Identified {
    /// A short plain-language description for the UI ("DOI", "Web page", …); `None` for
    /// [`Identified::Empty`].
    pub fn label(&self) -> Option<&'static str> {
        Some(match self {
            Identified::Empty => return None,
            Identified::Doi(_) => "DOI",
            Identified::Arxiv(_) => "arXiv paper",
            Identified::Isbn(_) => "ISBN",
            Identified::Url(_) => "Web page",
            Identified::BibTeX(_) => "BibTeX record",
            Identified::Text(_) => "Title or citation",
        })
    }
}

/// Classify `input`. See the module docs for the guiding rule.
pub fn identify(input: &str) -> Identified {
    let text = input.trim();
    if text.is_empty() {
        return Identified::Empty;
    }
    if is_bibtex(text) {
        return Identified::BibTeX(text.to_string());
    }
    let single_token = !text.chars().any(char::is_whitespace);

    // A link: arXiv and DOI links are recognised before being treated as plain pages.
    if single_token && (text.starts_with("http://") || text.starts_with("https://")) {
        if let Some(id) = arxiv_from_url(text) {
            return Identified::Arxiv(id);
        }
        if let Some(doi) = find_doi(text) {
            return Identified::Doi(doi);
        }
        return Identified::Url(text.to_string());
    }

    // A link pasted without its scheme.
    if single_token && text.starts_with("www.") && text[4..].contains('.') {
        return Identified::Url(format!("https://{text}"));
    }

    if let Some(id) = arxiv_prefixed(text) {
        return Identified::Arxiv(id);
    }
    if let Some(doi) = find_doi(text) {
        return Identified::Doi(doi);
    }
    if let Some(isbn) = parse_isbn(text) {
        return Identified::Isbn(isbn);
    }
    if single_token && is_bare_arxiv_id(text) {
        return Identified::Arxiv(strip_pdf_ext(text).to_string());
    }
    Identified::Text(collapse_spaces(text))
}

fn collapse_spaces(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// --- BibTeX -------------------------------------------------------------------------------

/// `@type{` … — an `@` immediately followed by a word and an opening brace or parenthesis.
fn is_bibtex(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('@') else {
        return false;
    };
    let word: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    if word.is_empty() {
        return false;
    }
    matches!(
        rest[word.len()..].trim_start().chars().next(),
        Some('{' | '(')
    )
}

// --- DOI ----------------------------------------------------------------------------------

/// Find a DOI (`10.NNNN/suffix`) in `text` and return it bare. It must start at a word
/// boundary — not mid-number, so `v110.1234/x` is not one — and have a 4–9 digit registrant.
/// A link's query string and fragment are dropped, as is trailing sentence punctuation.
fn find_doi(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(rel) = text[from..].find("10.") {
        let start = from + rel;
        from = start + 3;
        let boundary_ok =
            start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'.');
        if !boundary_ok {
            continue;
        }
        let after = &text[start + 3..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if !(4..=9).contains(&digits) || after.as_bytes().get(digits) != Some(&b'/') {
            continue;
        }
        let suffix = &after[digits + 1..];
        // The DOI runs to the first whitespace; in a link, also to the query or fragment.
        let in_link = text.contains("://");
        let end = suffix
            .find(|c: char| c.is_whitespace() || (in_link && matches!(c, '?' | '#')))
            .unwrap_or(suffix.len());
        let suffix = trim_trailing_punctuation(&suffix[..end]);
        if suffix.is_empty() {
            continue;
        }
        return Some(format!("10.{}/{}", &after[..digits], suffix));
    }
    None
}

/// Drop sentence punctuation after a DOI (`.`, `,`, `;`, `:`, quotes), and a closing bracket
/// that has no opening one — DOIs such as `10.1016/S0140-6736(20)30183-5` keep theirs.
fn trim_trailing_punctuation(s: &str) -> &str {
    let mut s = s;
    loop {
        let Some(last) = s.chars().next_back() else {
            return s;
        };
        let strip = match last {
            '.' | ',' | ';' | ':' | '"' | '\'' | '>' | '’' | '”' => true,
            ')' => s.matches('(').count() < s.matches(')').count(),
            ']' => s.matches('[').count() < s.matches(']').count(),
            _ => false,
        };
        if !strip {
            return s;
        }
        s = &s[..s.len() - last.len_utf8()];
    }
}

// --- arXiv --------------------------------------------------------------------------------

fn strip_pdf_ext(s: &str) -> &str {
    s.strip_suffix(".pdf").unwrap_or(s)
}

/// `https://arxiv.org/abs/1706.03762v2`, `/pdf/1706.03762.pdf`, `/abs/math/0211159`.
fn arxiv_from_url(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let rest = rest
        .strip_prefix("arxiv.org/")
        .or_else(|| rest.strip_prefix("export.arxiv.org/"))?;
    let id = rest
        .strip_prefix("abs/")
        .or_else(|| rest.strip_prefix("pdf/"))?;
    let id = id.split(['?', '#']).next()?.trim_end_matches('/');
    let id = strip_pdf_ext(id);
    is_bare_arxiv_id(id).then(|| id.to_string())
}

/// `arXiv:1706.03762` (any case, optional space after the colon).
fn arxiv_prefixed(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let rest = lower.strip_prefix("arxiv:")?;
    let id = text[text.len() - rest.len()..].trim();
    let id = id.split_whitespace().next()?;
    is_bare_arxiv_id(id).then(|| id.to_string())
}

/// A new-style (`1706.03762`, `0704.0001v3`) or old-style (`math/0211159`, `hep-th/9901001`)
/// arXiv identifier, optionally with a `vN` version — and nothing else.
fn is_bare_arxiv_id(s: &str) -> bool {
    let s = strip_pdf_ext(s);
    let base = match s.rfind('v') {
        Some(i) if i > 0 && s[i + 1..].bytes().all(|b| b.is_ascii_digit()) && i + 1 < s.len() => {
            &s[..i]
        }
        _ => s,
    };
    if let Some((year_month, number)) = base.split_once('.') {
        // YYMM.NNNN or YYMM.NNNNN
        return year_month.len() == 4
            && year_month.bytes().all(|b| b.is_ascii_digit())
            && (4..=5).contains(&number.len())
            && number.bytes().all(|b| b.is_ascii_digit());
    }
    if let Some((archive, number)) = base.split_once('/') {
        return !archive.is_empty()
            && archive
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'-' || b == b'.')
            && number.len() == 7
            && number.bytes().all(|b| b.is_ascii_digit());
    }
    false
}

// --- ISBN ---------------------------------------------------------------------------------

/// An ISBN-10 or ISBN-13, with or without hyphens, spaces or an `ISBN` prefix, whose check digit
/// is valid. Returns the digits only. The *whole* input must be the ISBN, so a number inside a
/// sentence is never taken for one.
fn parse_isbn(text: &str) -> Option<String> {
    let mut t = text.trim();
    if t.len() >= 4 && t[..4].eq_ignore_ascii_case("isbn") {
        t = t[4..].trim_start_matches(|c: char| c == ':' || c == '-' || c.is_whitespace());
        // "ISBN-13: …" / "ISBN-10 …"
        if let Some(r) = t.strip_prefix("13").or_else(|| t.strip_prefix("10")) {
            if r.starts_with([':', ' ']) {
                t = r.trim_start_matches([':', ' ']);
            }
        }
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '-' | ' ' | 'x' | 'X'))
    {
        return None;
    }
    let digits: String = t.chars().filter(|c| !matches!(c, '-' | ' ')).collect();
    let valid = match digits.len() {
        10 => isbn10_valid(&digits),
        13 => isbn13_valid(&digits),
        _ => false,
    };
    valid.then(|| digits.to_ascii_uppercase())
}

/// The ISBN-13 form of `isbn` (digits, hyphens and spaces allowed), so that the same book
/// matches whether it was entered as an ISBN-10 or an ISBN-13. `None` if it isn't a valid ISBN.
pub fn isbn13(isbn: &str) -> Option<String> {
    let digits: String = isbn
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .collect::<String>()
        .to_ascii_uppercase();
    match digits.len() {
        13 if isbn13_valid(&digits) => Some(digits),
        10 if isbn10_valid(&digits) => {
            let body = format!("978{}", &digits[..9]);
            let sum: u32 = body
                .chars()
                .enumerate()
                .map(|(i, c)| c.to_digit(10).unwrap_or(0) * if i % 2 == 0 { 1 } else { 3 })
                .sum();
            Some(format!("{body}{}", (10 - sum % 10) % 10))
        }
        _ => None,
    }
}

fn isbn10_valid(d: &str) -> bool {
    let mut sum = 0;
    for (i, c) in d.chars().enumerate() {
        let v = match c {
            '0'..='9' => c as u32 - '0' as u32,
            'X' | 'x' if i == 9 => 10,
            _ => return false,
        };
        sum += v * (10 - i as u32);
    }
    sum % 11 == 0
}

fn isbn13_valid(d: &str) -> bool {
    if !(d.starts_with("978") || d.starts_with("979")) {
        return false;
    }
    let mut sum = 0;
    for (i, c) in d.chars().enumerate() {
        let Some(v) = c.to_digit(10) else {
            return false;
        };
        sum += v * if i % 2 == 0 { 1 } else { 3 };
    }
    sum % 10 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doi(s: &str) -> Identified {
        Identified::Doi(s.to_string())
    }

    #[test]
    fn empty_and_blank() {
        assert_eq!(identify(""), Identified::Empty);
        assert_eq!(identify("  \n\t "), Identified::Empty);
        assert_eq!(Identified::Empty.label(), None);
    }

    #[test]
    fn dois_in_every_common_shape() {
        for input in [
            "10.1038/171737a0",
            "doi:10.1038/171737a0",
            "DOI: 10.1038/171737a0",
            "https://doi.org/10.1038/171737a0",
            "http://dx.doi.org/10.1038/171737a0",
            "https://doi.org/10.1038/171737a0?utm_source=x#frag",
            "  10.1038/171737a0.  ",
            "see doi 10.1038/171737a0, which was the first",
            "https://www.nature.com/articles/10.1038/171737a0",
        ] {
            assert_eq!(identify(input), doi("10.1038/171737a0"), "{input:?}");
        }
    }

    #[test]
    fn doi_with_brackets_keeps_its_own_but_not_a_stray_closer() {
        assert_eq!(
            identify("10.1016/S0140-6736(20)30183-5"),
            doi("10.1016/S0140-6736(20)30183-5")
        );
        assert_eq!(
            identify("(see 10.1016/S0140-6736(20)30183-5)"),
            doi("10.1016/S0140-6736(20)30183-5")
        );
    }

    #[test]
    fn publisher_links_with_an_embedded_doi_are_dois_not_pages() {
        assert_eq!(
            identify("https://onlinelibrary.wiley.com/doi/10.1111/j.1467-9809.2012.00001.x"),
            doi("10.1111/j.1467-9809.2012.00001.x")
        );
        assert_eq!(
            identify("https://link.springer.com/article/10.1007/s11098-020-01500-1"),
            doi("10.1007/s11098-020-01500-1")
        );
    }

    #[test]
    fn things_that_only_look_like_dois_are_not() {
        // Version numbers and decimals are not DOIs.
        assert!(matches!(
            identify("version 110.1234/x"),
            Identified::Text(_)
        ));
        assert!(matches!(identify("10.5 percent"), Identified::Text(_)));
        // Registrant too short.
        assert!(matches!(identify("10.12/abc"), Identified::Text(_)));
    }

    #[test]
    fn arxiv_in_every_common_shape() {
        for input in [
            "1706.03762",
            "1706.03762v5",
            "arXiv:1706.03762",
            "arxiv: 1706.03762",
            "https://arxiv.org/abs/1706.03762",
            "https://arxiv.org/abs/1706.03762v5",
            "https://arxiv.org/pdf/1706.03762.pdf",
            "https://export.arxiv.org/abs/1706.03762?context=cs",
        ] {
            match identify(input) {
                Identified::Arxiv(id) => assert!(id.starts_with("1706.03762"), "{input:?} → {id}"),
                other => panic!("{input:?} → {other:?}"),
            }
        }
        assert_eq!(
            identify("https://arxiv.org/abs/math/0211159"),
            Identified::Arxiv("math/0211159".into())
        );
        assert_eq!(
            identify("hep-th/9901001"),
            Identified::Arxiv("hep-th/9901001".into())
        );
    }

    #[test]
    fn numbers_that_are_not_arxiv_ids() {
        assert!(matches!(identify("2021.5"), Identified::Text(_)));
        assert!(matches!(identify("12345.12345"), Identified::Text(_)));
    }

    #[test]
    fn isbns_with_a_valid_check_digit() {
        for input in [
            "9780140449136",
            "978-0-14-044913-6",
            "ISBN 978-0-14-044913-6",
            "ISBN: 9780140449136",
            "ISBN-13: 978-0-14-044913-6",
            "0140449132",
            "0-14-044913-2",
            "ISBN-10: 0-14-044913-2",
        ] {
            let got = identify(input);
            assert!(matches!(got, Identified::Isbn(_)), "{input:?} → {got:?}");
        }
        assert_eq!(
            identify("978-0-14-044913-6"),
            Identified::Isbn("9780140449136".into())
        );
        // An ISBN-10 ending in X, either case.
        assert_eq!(
            identify("080442957X"),
            Identified::Isbn("080442957X".into())
        );
        assert_eq!(
            identify("0-8044-2957-x"),
            Identified::Isbn("080442957X".into())
        );
    }

    #[test]
    fn a_bad_check_digit_is_not_an_isbn() {
        assert!(matches!(identify("9780140449137"), Identified::Text(_)));
        assert!(matches!(identify("0140449133"), Identified::Text(_)));
        // 13 digits that are not an ISBN prefix.
        assert!(matches!(identify("1234567890123"), Identified::Text(_)));
        // A phone number / year-ish number.
        assert!(matches!(identify("1937"), Identified::Text(_)));
        assert!(matches!(identify("902 555 0199"), Identified::Text(_)));
    }

    #[test]
    fn other_links_are_pages() {
        assert_eq!(
            identify("https://plato.stanford.edu/entries/process-theism/"),
            Identified::Url("https://plato.stanford.edu/entries/process-theism/".into())
        );
        // A link pasted without its scheme.
        assert_eq!(
            identify("www.example.org/page"),
            Identified::Url("https://www.example.org/page".into())
        );
        assert!(matches!(identify("www.example"), Identified::Text(_)));
        // A link with spaces is not a link.
        assert!(matches!(
            identify("https://example.org/a b"),
            Identified::Text(_)
        ));
    }

    #[test]
    fn bibtex_records() {
        let rec = "@book{cone1970,\n  title = {A Black Theology of Liberation},\n}";
        assert_eq!(identify(rec), Identified::BibTeX(rec.into()));
        assert!(matches!(
            identify("@article {x, title={T}}"),
            Identified::BibTeX(_)
        ));
        // An @-mention or handle is not BibTeX.
        assert!(matches!(identify("@cone1970black"), Identified::Text(_)));
        assert!(matches!(identify("@ home"), Identified::Text(_)));
    }

    #[test]
    fn words_are_a_search_with_whitespace_tidied() {
        assert_eq!(
            identify("  Black   Theology\nand Black Power "),
            Identified::Text("Black Theology and Black Power".into())
        );
        assert_eq!(identify("x").label(), Some("Title or citation"));
    }

    #[test]
    fn a_pasted_citation_with_a_doi_is_found_by_its_doi() {
        assert_eq!(
            identify("Cone, James H. 1970. Black Theology and Black Power. doi:10.1234/abc.def."),
            doi("10.1234/abc.def")
        );
    }

    #[test]
    fn isbn10_and_isbn13_of_one_book_compare_equal() {
        assert_eq!(isbn13("0-14-044913-2").as_deref(), Some("9780140449136"));
        assert_eq!(
            isbn13("978-0-14-044913-6").as_deref(),
            Some("9780140449136")
        );
        assert_eq!(isbn13("080442957X").as_deref(), Some("9780804429573"));
        assert_eq!(isbn13("0140449133"), None);
        assert_eq!(isbn13("hello"), None);
    }

    #[test]
    fn labels() {
        assert_eq!(doi("10.1/x").label(), Some("DOI"));
        assert_eq!(
            Identified::Isbn("9780140449136".into()).label(),
            Some("ISBN")
        );
        assert_eq!(Identified::Url("http://x".into()).label(), Some("Web page"));
    }
}
