//! Minimal HTML citation-metadata scraper for "Add from URL". Reads the `<meta>` tags that
//! publishers, repositories, and Google Scholar rely on — Highwire Press (`citation_*`),
//! Dublin Core (`DC.*`), and Open Graph (`og:*`) — into a small struct the New-item YAML
//! builder can consume. Deliberately dependency-free (no full HTML parser): it scans for
//! `<meta>` tags and pulls `name`/`property` + `content` attributes.

/// Citation fields distilled from a page's `<meta>` tags.
#[derive(Debug, Default, PartialEq)]
pub struct WebMeta {
    pub title: String,
    /// Author names, ideally "Family, Given" (as Highwire emits them).
    pub authors: Vec<String>,
    /// Whatever precision the source date actually has — `YYYY`, `YYYY-MM`, or
    /// `YYYY-MM-DD` — all of which Hayagriva's date parser accepts directly.
    pub date: String,
    /// Journal / book / site title.
    pub container: String,
    pub publisher: String,
    pub doi: String,
    pub isbn: String,
    pub volume: String,
    pub issue: String,
    /// `firstpage-lastpage`, or just `firstpage` if there's no last page.
    pub pages: String,
    /// An ISO 639 language code (`citation_language`/`dc.language` are usually already
    /// this shape, e.g. `en`), which is what Hayagriva's `language` field expects.
    pub language: String,
    /// A direct PDF link advertised by the page, if any (may be relative).
    pub pdf_url: String,
    /// Hayagriva entry type inferred from the tags.
    pub entry_type: String,
}

impl WebMeta {
    /// Build from a page's HTML. `page_url` is the URL fetched, used as the fallback `url`
    /// isn't stored here (the caller supplies it) but drives relative-PDF resolution.
    pub fn from_html(html: &str) -> WebMeta {
        let tags = meta_tags(html);
        let first = |keys: &[&str]| -> String {
            for (name, content) in &tags {
                if keys.contains(&name.as_str()) && !content.trim().is_empty() {
                    return content.trim().to_string();
                }
            }
            String::new()
        };
        let all = |keys: &[&str]| -> Vec<String> {
            tags.iter()
                .filter(|(n, c)| keys.contains(&n.as_str()) && !c.trim().is_empty())
                .map(|(_, c)| c.trim().to_string())
                .collect()
        };

        let first_page = first(&["citation_firstpage"]);
        let last_page = first(&["citation_lastpage"]);
        let pages = match (first_page.is_empty(), last_page.is_empty()) {
            (false, false) => format!("{first_page}-{last_page}"),
            (false, true) => first_page,
            _ => first(&["citation_pages"]),
        };

        let mut m = WebMeta {
            title: first(&["citation_title", "dc.title", "og:title", "twitter:title"]),
            authors: all(&["citation_author", "dc.creator", "citation_authors"]),
            date: String::new(),
            container: first(&[
                "citation_journal_title",
                "citation_conference_title",
                "citation_inbook_title",
                "og:site_name",
            ]),
            publisher: first(&["citation_publisher", "dc.publisher"]),
            doi: first(&["citation_doi", "dc.identifier.doi"]),
            isbn: first(&["citation_isbn"]),
            volume: first(&["citation_volume"]),
            issue: first(&["citation_issue"]),
            pages,
            language: first(&["citation_language", "dc.language"]),
            pdf_url: first(&["citation_pdf_url"]),
            entry_type: String::new(),
        };

        // A single "A; B" or "A, B and C" authors field → split it.
        if m.authors.len() == 1 && m.authors[0].contains(';') {
            m.authors = m.authors[0]
                .split(';')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }

        // Whatever precision the source date has, not just its year.
        let date = first(&[
            "citation_publication_date",
            "citation_date",
            "citation_online_date",
            "dc.date",
            "article:published_time",
        ]);
        m.date = best_effort_date(&date);

        // A bare DOI identifier sometimes hides in dc.identifier.
        if m.doi.is_empty() {
            let id = first(&["dc.identifier", "citation_id"]);
            if let Some(rest) = id.strip_prefix("doi:") {
                m.doi = rest.trim().to_string();
            } else if id.starts_with("10.") && id.contains('/') {
                m.doi = id;
            }
        }

        m.entry_type = if !m.container.is_empty() || !m.doi.is_empty() {
            "article".to_string()
        } else if !m.isbn.is_empty() {
            "book".to_string()
        } else {
            "web".to_string()
        };
        m
    }

    /// True when there is enough to make a meaningful entry.
    pub fn is_usable(&self) -> bool {
        !self.title.is_empty()
    }
}

/// Turn a citation-metadata date (`2021-03-01`, `2021/3/1`, `2021`, …) into whatever
/// precision it actually carries, in the form Hayagriva's date parser accepts:
/// `YYYY-MM-DD`, `YYYY-MM`, or bare `YYYY`. Citation date metadata is near-universally
/// already numeric/ISO-shaped (unlike OpenLibrary's freeform `publish_date`), so this only
/// needs to normalize separators and validate ranges, not recognize month names.
fn best_effort_date(s: &str) -> String {
    let year = four_digit_year(s);
    if year.is_empty() {
        return String::new();
    }
    let Some(year_pos) = s.find(&year) else {
        return year;
    };
    let rest = &s[year_pos + 4..];
    let parts: Vec<&str> = rest
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .collect();

    let month = parts.first().and_then(|p| p.parse::<u8>().ok()).filter(|m| (1..=12).contains(m));
    let Some(month) = month else {
        return year;
    };
    let day = parts.get(1).and_then(|p| p.parse::<u8>().ok()).filter(|d| (1..=31).contains(d));
    match day {
        Some(day) => format!("{year}-{month:02}-{day:02}"),
        None => format!("{year}-{month:02}"),
    }
}

/// Extract the first four consecutive ASCII digits as a year (handles `2021-03-01`,
/// `2021/3/1`, `March 2021`, `2021`).
fn four_digit_year(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if bytes[i..i + 4].iter().all(|b| b.is_ascii_digit()) {
            return s[i..i + 4].to_string();
        }
        i += 1;
    }
    String::new()
}

/// All `<meta>` tags as `(name-or-property lowercased, content)`.
fn meta_tags(html: &str) -> Vec<(String, String)> {
    let bytes = html.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 5 <= bytes.len() {
        if html.is_char_boundary(i)
            && html[i..].len() >= 5
            && html[i..i + 5].eq_ignore_ascii_case("<meta")
        {
            let mut j = i + 5;
            while j < bytes.len() && bytes[j] != b'>' {
                j += 1;
            }
            let end = j.min(bytes.len());
            if html.is_char_boundary(end) {
                let tag = &html[i..end];
                let name = meta_attr(tag, "name").or_else(|| meta_attr(tag, "property"));
                let content = meta_attr(tag, "content");
                if let (Some(n), Some(c)) = (name, content) {
                    out.push((n.to_lowercase(), c));
                }
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// The value of attribute `key` (ASCII case-insensitive) inside one tag string.
fn meta_attr(tag: &str, key: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    let klen = key.len();
    let mut i = 0;
    while i + klen <= bytes.len() {
        if tag.is_char_boundary(i)
            && tag.is_char_boundary(i + klen)
            && tag[i..i + klen].eq_ignore_ascii_case(key)
            && (i == 0 || bytes[i - 1].is_ascii_whitespace() || bytes[i - 1] == b'<')
        {
            let mut j = i + klen;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'=' {
                j += 1;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
                    let quote = bytes[j];
                    j += 1;
                    let start = j;
                    while j < bytes.len() && bytes[j] != quote {
                        j += 1;
                    }
                    if j <= bytes.len() {
                        return Some(html_unescape(&tag[start..j]));
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// Decode the handful of HTML entities that show up in meta content.
fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

/// Resolve a possibly-relative PDF URL against the page URL.
pub fn resolve_url(base: &str, link: &str) -> String {
    if link.is_empty() {
        return String::new();
    }
    if link.starts_with("http://") || link.starts_with("https://") {
        return link.to_string();
    }
    // scheme + authority of the base URL, e.g. "https://host.tld".
    let scheme_end = base.find("://").map(|p| p + 3).unwrap_or(0);
    let authority_end = base[scheme_end..]
        .find('/')
        .map(|p| scheme_end + p)
        .unwrap_or(base.len());
    let origin = &base[..authority_end];
    if let Some(rest) = link.strip_prefix("//") {
        let scheme = base.split("://").next().unwrap_or("https");
        return format!("{scheme}://{rest}");
    }
    if link.starts_with('/') {
        return format!("{origin}{link}");
    }
    // Relative to the base's directory.
    let dir_end = base.rfind('/').map(|p| p + 1).unwrap_or(base.len());
    format!("{}{}", &base[..dir_end], link)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_highwire_article() {
        let html = r#"
            <html><head>
            <meta name="citation_title" content="Black Theology &amp; Power">
            <meta name="citation_author" content="Cone, James">
            <meta name="citation_author" content="Smith, Jane">
            <meta name="citation_journal_title" content="Journal of Theology">
            <meta name="citation_publication_date" content="1970/03/01">
            <meta name="citation_doi" content="10.1000/xyz">
            <meta name="citation_volume" content="12">
            <meta name="citation_issue" content="3">
            <meta name="citation_firstpage" content="45">
            <meta name="citation_lastpage" content="67">
            <meta name="citation_language" content="en">
            <meta name="citation_pdf_url" content="/content/1/1.full.pdf">
            </head></html>
        "#;
        let m = WebMeta::from_html(html);
        assert_eq!(m.title, "Black Theology & Power");
        assert_eq!(m.authors, vec!["Cone, James", "Smith, Jane"]);
        assert_eq!(m.container, "Journal of Theology");
        // Month and day are preserved, not truncated down to the bare year.
        assert_eq!(m.date, "1970-03-01");
        assert_eq!(m.doi, "10.1000/xyz");
        assert_eq!(m.volume, "12");
        assert_eq!(m.issue, "3");
        assert_eq!(m.pages, "45-67");
        assert_eq!(m.language, "en");
        assert_eq!(m.entry_type, "article");
        assert!(m.is_usable());
    }

    #[test]
    fn best_effort_date_captures_available_precision() {
        assert_eq!(best_effort_date("1970/03/01"), "1970-03-01");
        assert_eq!(best_effort_date("1970-03"), "1970-03");
        assert_eq!(best_effort_date("1970"), "1970");
        assert_eq!(best_effort_date("no date here"), "");
        // An out-of-range "month" (not actually a date) degrades to year-only rather than
        // producing an invalid Hayagriva date.
        assert_eq!(best_effort_date("1970/99/01"), "1970");
    }

    #[test]
    fn falls_back_to_opengraph_web() {
        let html = r#"<meta property="og:title" content="A Blog Post"/>"#;
        let m = WebMeta::from_html(html);
        assert_eq!(m.title, "A Blog Post");
        assert_eq!(m.entry_type, "web");
    }

    #[test]
    fn resolves_relative_pdf_urls() {
        let base = "https://example.org/articles/1/full";
        assert_eq!(
            resolve_url(base, "/content/1.pdf"),
            "https://example.org/content/1.pdf"
        );
        assert_eq!(
            resolve_url(base, "https://cdn.example.org/x.pdf"),
            "https://cdn.example.org/x.pdf"
        );
        assert_eq!(
            resolve_url(base, "1.pdf"),
            "https://example.org/articles/1/1.pdf"
        );
    }
}
