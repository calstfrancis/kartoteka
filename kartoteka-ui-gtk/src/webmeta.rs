//! HTML citation-metadata scraper for "Add from URL". Reads what a page says about itself, in
//! order of how much it's worth:
//!
//! 1. The `<meta>` tags publishers, repositories, and Google Scholar rely on — Highwire Press
//!    (`citation_*`), Dublin Core (`DC.*`) — plus Open Graph (`og:*`) and schema.org microdata
//!    (`itemprop`, which is how YouTube describes a video).
//! 2. Schema.org JSON-LD ([`jsonld`]) — authors, durations, and the show an episode is from.
//! 3. Site readers for pages whose real subject isn't in either: a CBC story page
//!    ([`cbc`]) plays a program episode that only its embedded player state describes.
//!
//! Recorded media come out shaped the way [`fond_bib::item_kind`] describes (an episode inside
//! its show, the running time, "Interview by …"), so they cite properly. Who hosted and who was
//! interviewed is guessed from the episode's text ([`people`]) when nothing structured says.
//! Dependency-light: no full HTML parser — tags are scanned for directly.

mod cbc;
mod jsonld;
mod people;

/// Recorded audio or video.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Media {
    #[default]
    Audio,
    Video,
}

/// Citation fields distilled from a page.
#[derive(Debug, Default, PartialEq)]
pub struct WebMeta {
    pub title: String,
    /// Author names, ideally "Family, Given" (as Highwire emits them) or natural order.
    pub authors: Vec<String>,
    /// An organization or channel as sole author (a YouTube channel) — written as one name,
    /// never split into given/family.
    pub org_author: String,
    /// Whatever precision the source date actually has — `YYYY`, `YYYY-MM`, or
    /// `YYYY-MM-DD` — all of which Hayagriva's date parser accepts directly.
    pub date: String,
    /// Journal / book / site title — or, for an episode, its show.
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
    /// Hayagriva entry type inferred from the page.
    pub entry_type: String,
    /// Hayagriva type for the container's `parent:` (empty = the entry type's usual one).
    pub parent_type: String,
    /// Medium label or "Interview by NAME" (see `fond_bib::item_kind`).
    pub genre: String,
    /// Running time, `mm:ss` / `hh:mm:ss`.
    pub runtime: String,
    /// The people (host, guest) were guessed from the page's text, so worth a look.
    pub people_guessed: bool,
}

impl WebMeta {
    /// Build from a page's HTML alone (no site readers that need the URL).
    #[cfg(test)]
    pub fn from_html(html: &str) -> WebMeta {
        WebMeta::from_page(html, "")
    }

    /// Build from a page's HTML; `page_url` is where it was fetched from.
    pub fn from_page(html: &str, page_url: &str) -> WebMeta {
        let mut m = WebMeta::from_meta_tags(html);
        let ld = jsonld::main_work(html);

        // Written works: JSON-LD fills what the meta tags left out (news sites often put
        // their bylines only there).
        if let Some(ld) = ld.as_ref().filter(|ld| ld.media.is_none()) {
            if m.authors.is_empty() {
                m.authors = ld.authors.clone();
            }
            if m.date.is_empty() {
                m.date = best_effort_date(&ld.date);
            }
            if m.title.is_empty() {
                m.title = ld.title.clone();
            }
        }

        let cbc = page_url
            .contains("cbc.ca/")
            .then(|| cbc::media(html))
            .flatten();
        if let Some(c) = cbc {
            m.apply_media(Episode {
                media: c.media,
                title: c.title,
                series: c.show,
                publisher: c.network,
                date: c.date,
                duration: c.duration,
                description: c.description,
                ..Default::default()
            });
        } else if let Some(ld) = ld.filter(|ld| ld.media.is_some()) {
            // Microdata fills what the JSON-LD left out (YouTube's has no channel or length).
            let micro = m.itemprop_video(html).unwrap_or_default();
            let site = m.container.clone();
            m.apply_media(Episode {
                media: ld.media.unwrap_or_default(),
                title: ld.title,
                series: ld.series,
                publisher: if ld.publisher.is_empty() {
                    site
                } else {
                    ld.publisher
                },
                date: ld.date,
                duration: ld.duration.or(micro.duration),
                description: ld.description,
                org_author: if ld.authors.is_empty() && ld.org_author.is_empty() {
                    micro.org_author
                } else {
                    ld.org_author
                },
                authors: ld.authors,
                hosts: ld.hosts,
                guests: ld.guests,
            });
        } else if let Some(video) = m.itemprop_video(html) {
            m.apply_media(video);
        }
        m
    }

    /// The `<meta>`-tag reading: Highwire, Dublin Core, Open Graph.
    fn from_meta_tags(html: &str) -> WebMeta {
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
            ..Default::default()
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

    /// A video described by schema.org microdata (`itemprop`), as YouTube does: the first
    /// `name` is the video's, the channel is the `name` on a `<link>` inside its author.
    fn itemprop_video(&self, html: &str) -> Option<Episode> {
        let tags = meta_tags(html);
        let get = |key: &str| {
            tags.iter()
                .find(|(n, c)| n == key && !c.trim().is_empty())
                .map(|(_, c)| c.trim().to_string())
                .unwrap_or_default()
        };
        let og_type = get("og:type");
        let duration = jsonld::iso_duration_seconds(&get("itemprop:duration"));
        if !og_type.starts_with("video") && duration.is_none() {
            return None;
        }
        let title = get("itemprop:name");
        Some(Episode {
            media: Media::Video,
            title: if title.is_empty() {
                self.title.clone()
            } else {
                title
            },
            publisher: self.container.clone(),
            date: [get("itemprop:datepublished"), get("itemprop:uploaddate")]
                .into_iter()
                .find(|d| !d.is_empty())
                .unwrap_or_default(),
            duration,
            org_author: get("itemprop-link:name"),
            ..Default::default()
        })
    }

    /// Turn the page's reading into recorded media: an episode of a show (`scene` inside an
    /// `audio`/`video` parent), or a standalone recording — with "Interview by …" when it is
    /// one, the guest then being its author.
    fn apply_media(&mut self, e: Episode) {
        if !e.title.is_empty() {
            self.title = e.title.trim().to_string();
        }
        let date = best_effort_date(&e.date);
        if !date.is_empty() {
            self.date = date;
        }
        self.runtime = e
            .duration
            .map(fond_bib::item_kind::format_seconds)
            .unwrap_or_default();
        self.publisher = e.publisher.trim().to_string();
        self.doi.clear();
        self.isbn.clear();
        self.pdf_url.clear();
        let parent = match e.media {
            Media::Audio => "audio",
            Media::Video => "video",
        };
        let series = e.series.trim().to_string();
        if series.is_empty() {
            self.entry_type = parent.to_string();
            self.container.clear();
            self.parent_type.clear();
        } else {
            self.entry_type = "scene".to_string();
            self.container = series.clone();
            self.parent_type = parent.to_string();
        }

        // Who's talking: from structured data when there is any, else guessed from the text.
        let (mut host, mut guests) = (e.hosts.first().cloned().unwrap_or_default(), e.guests);
        if host.is_empty() && guests.is_empty() {
            let guessed = people::guess(
                &self.title,
                &e.description,
                &[series.as_str(), self.publisher.as_str()],
            );
            self.people_guessed = !guessed.host.is_empty() || !guessed.guests.is_empty();
            host = guessed.host;
            guests = guessed.guests;
        }
        let (authors, org_author) = (e.authors, e.org_author);
        if !host.is_empty() && !guests.is_empty() {
            self.genre = fond_bib::item_kind::genre_for_interviewer(&host);
            self.authors = guests;
            self.org_author.clear();
        } else {
            self.genre = match (e.media, series.is_empty()) {
                (Media::Audio, false) => "Podcast episode".to_string(),
                (Media::Video, true) => "Video".to_string(),
                _ => String::new(),
            };
            self.authors = if !authors.is_empty() {
                authors
            } else if !host.is_empty() {
                vec![host]
            } else {
                guests
            };
            self.org_author = if self.authors.is_empty() {
                org_author
            } else {
                String::new()
            };
        }
    }

    /// True when there is enough to make a meaningful entry.
    pub fn is_usable(&self) -> bool {
        !self.title.is_empty()
    }
}

/// One episode or recording, as read from whichever source described it.
#[derive(Debug, Default)]
struct Episode {
    media: Media,
    title: String,
    /// The show / series / channel it belongs to (empty for a standalone recording).
    series: String,
    publisher: String,
    date: String,
    duration: Option<u64>,
    description: String,
    authors: Vec<String>,
    org_author: String,
    hosts: Vec<String>,
    guests: Vec<String>,
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

    let month = parts
        .first()
        .and_then(|p| p.parse::<u8>().ok())
        .filter(|m| (1..=12).contains(m));
    let Some(month) = month else {
        return year;
    };
    let day = parts
        .get(1)
        .and_then(|p| p.parse::<u8>().ok())
        .filter(|d| (1..=31).contains(d));
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

/// All `<meta>` tags as `(name-or-property lowercased, content)`. Schema.org microdata comes
/// through too, keyed apart so it can't collide with the named tags: `<meta itemprop="x">` as
/// `itemprop:x`, and `<link itemprop="x" content=…>` (YouTube's channel name) as
/// `itemprop-link:x`.
fn meta_tags(html: &str) -> Vec<(String, String)> {
    let bytes = html.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 5 <= bytes.len() {
        let opens = |tag: &str| {
            html.is_char_boundary(i)
                && html.is_char_boundary((i + tag.len()).min(html.len()))
                && html[i..].len() >= tag.len()
                && html[i..i + tag.len()].eq_ignore_ascii_case(tag)
        };
        let is_meta = opens("<meta");
        let is_link = !is_meta && opens("<link");
        if is_meta || is_link {
            let mut j = i + 5;
            while j < bytes.len() && bytes[j] != b'>' {
                j += 1;
            }
            let end = j.min(bytes.len());
            if html.is_char_boundary(end) {
                let tag = &html[i..end];
                let content = meta_attr(tag, "content");
                let itemprop = meta_attr(tag, "itemprop").map(|p| p.to_lowercase());
                let name = if is_link {
                    itemprop.map(|p| format!("itemprop-link:{p}"))
                } else {
                    meta_attr(tag, "name")
                        .or_else(|| meta_attr(tag, "property"))
                        .map(|n| n.to_lowercase())
                        .or_else(|| itemprop.map(|p| format!("itemprop:{p}")))
                };
                if let (Some(n), Some(c)) = (name, content) {
                    out.push((n, c));
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
    fn youtube_video_from_jsonld_and_microdata() {
        let html = r#"
            <meta property="og:site_name" content="YouTube">
            <meta property="og:type" content="video.other">
            <meta itemprop="name" content="Me at the zoo">
            <meta itemprop="duration" content="PT0M19S">
            <span itemprop="author"><link itemprop="name" content="jawed"></span>
            <meta itemprop="uploadDate" content="2005-04-23T20:31:52-07:00">
            <script type="application/ld+json">{"@type":"VideoObject","name":"Me at the zoo",
              "uploadDate":"2005-04-23T20:31:52-07:00"}</script>"#;
        let m = WebMeta::from_page(html, "https://www.youtube.com/watch?v=jNQXAC9IVRw");
        assert_eq!(m.entry_type, "video");
        assert_eq!(m.title, "Me at the zoo");
        assert_eq!(m.org_author, "jawed");
        assert!(m.authors.is_empty());
        assert_eq!(m.date, "2005-04-23");
        assert_eq!(m.runtime, "00:19");
        assert_eq!(m.publisher, "YouTube");
        assert_eq!(m.genre, "Video");
        assert!(m.container.is_empty());
    }

    #[test]
    fn cbc_radio_story_is_the_interview_it_plays() {
        // The shape of https://www.cbc.ca/radio/ideas/karen-hao-empire-of-ai-9.7142134,
        // cut down: the page's own metadata describes a written story by "CBC"; the episode
        // is only in the player state.
        let html = r#"
            <meta property="og:title" content="Why we should &#x27;fight like hell&#x27; against Big AI | CBC Radio"/>
            <meta property="og:type" content="article"/>
            <meta property="og:site_name" content="CBC"/>
            <script type="application/ld+json">{"@type":"ReportageNewsArticle",
              "headline":"Why we should &apos;fight like hell&apos; against Big AI",
              "datePublished":"2026-07-31T16:00:00.000Z",
              "author":[{"@type":"NewsMediaOrganization","name":"CBC"}],
              "publisher":{"@type":"NewsMediaOrganization","name":"CBC"}}</script>
            <script>window.__INITIAL_STATE__ = {"detail":{"content":{"body":[{"type":"html",
              "content":[{"type":"polopoly_media","content":{"type":"audio",
              "title":"Empire of AI: Tech journalist Karen Hao ","showName":"Ideas",
              "publishedAt":1785502800000,"creator":undefined,
              "description":"Tech journalist Karen Hao investigated OpenAI for her bestseller, Empire of AI. Hao clearly sets out the urgent stakes in this conversation with Nahlah Ayed, alongside a March 2026 talk.",
              "media":{"duration":3240,"contentArea":"Radio"}}}]}]}}};</script>"#;
        let m = WebMeta::from_page(
            html,
            "https://www.cbc.ca/radio/ideas/karen-hao-empire-of-ai-9.7142134",
        );
        assert_eq!(m.entry_type, "scene");
        assert_eq!(m.parent_type, "audio");
        assert_eq!(m.title, "Empire of AI: Tech journalist Karen Hao");
        assert_eq!(m.container, "Ideas");
        assert_eq!(m.publisher, "CBC Radio");
        assert_eq!(m.authors, vec!["Karen Hao"]);
        assert_eq!(m.genre, "Interview by Nahlah Ayed");
        assert_eq!(m.date, "2026-07-31");
        assert_eq!(m.runtime, "54:00");
        assert!(m.people_guessed);
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
