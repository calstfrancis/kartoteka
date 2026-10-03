//! Schema.org JSON-LD (`<script type="application/ld+json">`) — where most news sites, podcast
//! platforms and video hosts publish who made a thing, when, how long it runs, and what show
//! it belongs to, often far more completely than their `<meta>` tags.

use serde_json::Value;

/// What a page's JSON-LD says about its main work.
#[derive(Debug, Default, PartialEq)]
pub struct LdWork {
    pub media: Option<super::Media>,
    pub title: String,
    /// People credited as author/creator — organizations are left out (a newsroom crediting
    /// itself as author is not a byline).
    pub authors: Vec<String>,
    /// An organization credited as author/creator (a video channel, say), when no person is.
    pub org_author: String,
    pub date: String,
    /// Duration in seconds.
    pub duration: Option<u64>,
    /// The show / series an episode is part of.
    pub series: String,
    pub publisher: String,
    pub description: String,
    /// Schema.org named an episode's guests/hosts outright, so the text needn't be guessed at.
    pub guests: Vec<String>,
    pub hosts: Vec<String>,
}

/// Episode types (audio unless it's a TV episode), then other media, then written works —
/// the first kind present wins, so a podcast page that also describes itself as a WebPage
/// reads as the episode.
const EPISODE_TYPES: &[&str] = &["PodcastEpisode", "RadioEpisode", "TVEpisode", "Episode"];
const MEDIA_TYPES: &[&str] = &["VideoObject", "AudioObject", "Movie", "MusicRecording"];
const WRITTEN_TYPES: &[&str] = &[
    "NewsArticle",
    "ReportageNewsArticle",
    "AnalysisNewsArticle",
    "OpinionNewsArticle",
    "Article",
    "BlogPosting",
    "ScholarlyArticle",
    "Report",
];

/// Read the page's main work from its JSON-LD blocks, if it has any we recognize.
pub fn main_work(html: &str) -> Option<LdWork> {
    let nodes = nodes(html);
    let pick = |types: &[&str]| {
        types
            .iter()
            .find_map(|t| nodes.iter().find(|n| has_type(n, t)))
    };
    let (node, media) = if let Some(n) = pick(EPISODE_TYPES) {
        let video = has_type(n, "TVEpisode") || n.get("video").is_some();
        (
            n,
            Some(if video {
                super::Media::Video
            } else {
                super::Media::Audio
            }),
        )
    } else if let Some(n) = pick(MEDIA_TYPES) {
        let video = has_type(n, "VideoObject") || has_type(n, "Movie");
        (
            n,
            Some(if video {
                super::Media::Video
            } else {
                super::Media::Audio
            }),
        )
    } else {
        (pick(WRITTEN_TYPES)?, None)
    };

    let mut work = LdWork {
        media,
        title: text(node, &["name", "headline"]),
        date: text(node, &["datePublished", "uploadDate", "dateCreated"]),
        duration: node
            .get("duration")
            .or_else(|| node.get("timeRequired"))
            .and_then(Value::as_str)
            .and_then(iso_duration_seconds)
            .or_else(|| {
                // A podcast episode often puts its duration on the attached audio file.
                node.get("associatedMedia")
                    .and_then(|m| m.get("duration"))
                    .and_then(Value::as_str)
                    .and_then(iso_duration_seconds)
            }),
        series: node
            .get("partOfSeries")
            .or_else(|| node.get("partOfSeason").and_then(|s| s.get("partOfSeries")))
            .map(name_of)
            .unwrap_or_default(),
        publisher: node
            .get("publisher")
            .or_else(|| node.get("productionCompany"))
            .map(name_of)
            .unwrap_or_default(),
        description: text(node, &["description"]),
        ..Default::default()
    };
    for key in ["author", "creator"] {
        if let Some(v) = node.get(key) {
            for item in as_list(v) {
                let name = name_of(item);
                if name.is_empty() {
                    continue;
                }
                if is_organization(item) {
                    if work.org_author.is_empty() {
                        work.org_author = name;
                    }
                } else if !work.authors.contains(&name) {
                    work.authors.push(name);
                }
            }
        }
        if !work.authors.is_empty() {
            break;
        }
    }
    for (key, out) in [("guest", &mut work.guests), ("actor", &mut work.hosts)] {
        if let Some(v) = node.get(key) {
            out.extend(
                as_list(v)
                    .into_iter()
                    .map(name_of)
                    .filter(|n| !n.is_empty()),
            );
        }
    }
    // The publisher crediting itself as author is no byline.
    if work.publisher == work.org_author {
        work.org_author.clear();
    }
    Some(work)
}

/// Every JSON-LD object on the page, with `@graph` lists and top-level arrays flattened.
fn nodes(html: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for block in ld_blocks(html) {
        if let Ok(v) = serde_json::from_str::<Value>(block.trim()) {
            flatten(v, &mut out);
        }
    }
    out
}

fn flatten(v: Value, out: &mut Vec<Value>) {
    match v {
        Value::Array(items) => items.into_iter().for_each(|i| flatten(i, out)),
        Value::Object(mut map) => {
            if let Some(graph) = map.remove("@graph") {
                flatten(graph, out);
            }
            out.push(Value::Object(map));
        }
        _ => {}
    }
}

/// The text of each `<script type="application/ld+json">` element.
fn ld_blocks(html: &str) -> Vec<&str> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = lower[from..].find("application/ld+json") {
        let at = from + rel;
        let Some(open_end) = lower[at..].find('>').map(|p| at + p + 1) else {
            break;
        };
        let Some(close) = lower[open_end..].find("</script").map(|p| open_end + p) else {
            break;
        };
        out.push(&html[open_end..close]);
        from = close;
    }
    out
}

fn has_type(node: &Value, wanted: &str) -> bool {
    let matches = |t: &str| t == wanted || t.rsplit('/').next() == Some(wanted);
    match node.get("@type") {
        Some(Value::String(t)) => matches(t),
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).any(matches),
        _ => false,
    }
}

fn is_organization(node: &Value) -> bool {
    [
        "Organization",
        "NewsMediaOrganization",
        "Corporation",
        "Brand",
    ]
    .iter()
    .any(|t| has_type(node, t))
}

fn as_list(v: &Value) -> Vec<&Value> {
    match v {
        Value::Array(items) => items.iter().collect(),
        other => vec![other],
    }
}

/// A thing's name: a bare string, or an object's `name`.
fn name_of(v: &Value) -> String {
    match v {
        Value::String(s) => super::html_unescape(s.trim()),
        Value::Object(_) => v
            .get("name")
            .and_then(Value::as_str)
            .map(|s| super::html_unescape(s.trim()))
            .unwrap_or_default(),
        Value::Array(items) => items.first().map(name_of).unwrap_or_default(),
        _ => String::new(),
    }
}

/// The first of `keys` that holds non-empty text.
fn text(node: &Value, keys: &[&str]) -> String {
    keys.iter()
        .filter_map(|k| node.get(*k))
        .map(name_of)
        .find(|s| !s.is_empty())
        .unwrap_or_default()
}

/// ISO 8601 duration (`PT54M`, `PT1H2M3S`, `P0DT0H19M`) → seconds.
pub fn iso_duration_seconds(s: &str) -> Option<u64> {
    let s = s.trim();
    let rest = s.strip_prefix('P').or_else(|| s.strip_prefix('p'))?;
    let (days, time) = match rest.split_once(['T', 't']) {
        Some((d, t)) => (d, t),
        None => (rest, ""),
    };
    let mut total = 0u64;
    let mut num = String::new();
    for (part, units) in [(days, "D"), (time, "HMS")] {
        for c in part.chars() {
            if c.is_ascii_digit() || c == '.' {
                num.push(c);
                continue;
            }
            let n: f64 = num.parse().ok()?;
            num.clear();
            let factor = match (units, c.to_ascii_uppercase()) {
                ("D", 'D') => 86_400.0,
                ("HMS", 'H') => 3_600.0,
                ("HMS", 'M') => 60.0,
                ("HMS", 'S') => 1.0,
                _ => return None,
            };
            total += (n * factor).round() as u64;
        }
    }
    (total > 0).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_podcast_episode() {
        let html = r#"<script type="application/ld+json">{"@context":"https://schema.org",
            "@type":"PodcastEpisode","name":"On Grief","datePublished":"2024-02-03",
            "timeRequired":"PT1H2M","partOfSeries":{"@type":"PodcastSeries","name":"The Long Talk"},
            "author":{"@type":"Person","name":"Jane Host"},
            "publisher":{"@type":"Organization","name":"Example Audio"}}</script>"#;
        let w = main_work(html).unwrap();
        assert_eq!(w.media, Some(crate::webmeta::Media::Audio));
        assert_eq!(w.title, "On Grief");
        assert_eq!(w.series, "The Long Talk");
        assert_eq!(w.duration, Some(3720));
        assert_eq!(w.authors, vec!["Jane Host"]);
        assert_eq!(w.publisher, "Example Audio");
    }

    #[test]
    fn graph_and_types_by_priority() {
        let html = r#"<script type="application/ld+json">{"@graph":[
            {"@type":"WebPage","name":"Page"},
            {"@type":"VideoObject","name":"A Talk","uploadDate":"2020-01-02T10:00:00Z",
             "duration":"PT12M34S","author":{"@type":"Organization","name":"Some Channel"}}]}
            </script>"#;
        let w = main_work(html).unwrap();
        assert_eq!(w.media, Some(crate::webmeta::Media::Video));
        assert_eq!(w.title, "A Talk");
        assert_eq!(w.duration, Some(754));
        assert!(w.authors.is_empty());
        assert_eq!(w.org_author, "Some Channel");
    }

    #[test]
    fn a_newsroom_crediting_itself_is_not_a_byline() {
        let html = r#"<script type="application/ld+json">{"@type":"ReportageNewsArticle",
            "headline":"Why we should &apos;fight&apos;","datePublished":"2026-07-31T16:00:00.000Z",
            "author":[{"@type":"NewsMediaOrganization","name":"CBC"}],
            "publisher":{"@type":"NewsMediaOrganization","name":"CBC"}}</script>"#;
        let w = main_work(html).unwrap();
        assert_eq!(w.media, None);
        assert_eq!(w.title, "Why we should 'fight'");
        assert!(w.authors.is_empty());
        assert_eq!(w.org_author, "");
    }

    #[test]
    fn iso_durations() {
        assert_eq!(iso_duration_seconds("PT0M19S"), Some(19));
        assert_eq!(iso_duration_seconds("PT54M"), Some(3240));
        assert_eq!(iso_duration_seconds("P1DT1S"), Some(86_401));
        assert_eq!(iso_duration_seconds("54 minutes"), None);
    }
}
