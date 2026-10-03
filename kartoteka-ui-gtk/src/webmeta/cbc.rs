//! CBC (cbc.ca) story pages. A CBC Radio story wraps the audio of a program episode, but the
//! page's own metadata describes only the written story ("ReportageNewsArticle" by "CBC") —
//! the episode itself (show, title, running time) lives only in the page's embedded
//! `window.__INITIAL_STATE__`, as the player's media object.

use serde_json::Value;

/// The episode a CBC page plays.
#[derive(Debug, Default, PartialEq)]
pub struct CbcMedia {
    pub media: super::Media,
    pub title: String,
    /// The program: "Ideas", "The Current", …
    pub show: String,
    /// "CBC Radio" for radio programs, else "CBC".
    pub network: String,
    pub description: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    pub duration: Option<u64>,
}

/// The first media object in the page's story body (the episode it is about), else the first
/// anywhere on the page.
pub fn media(html: &str) -> Option<CbcMedia> {
    let state = initial_state(html)?;
    let mut in_body = None;
    let mut anywhere = None;
    find_media(&state, false, &mut in_body, &mut anywhere);
    in_body.or(anywhere)
}

fn find_media(
    v: &Value,
    in_body: bool,
    body_hit: &mut Option<CbcMedia>,
    any_hit: &mut Option<CbcMedia>,
) {
    if body_hit.is_some() {
        return;
    }
    match v {
        Value::Object(map) => {
            if let Some(m) = read_media(v) {
                if in_body {
                    *body_hit = Some(m);
                    return;
                }
                if any_hit.is_none() {
                    *any_hit = Some(m);
                }
            }
            for (k, child) in map {
                find_media(child, in_body || k == "body", body_hit, any_hit);
            }
        }
        Value::Array(items) => {
            for child in items {
                find_media(child, in_body, body_hit, any_hit);
            }
        }
        _ => {}
    }
}

/// A player media object: has a show name, an audio/video type, and a duration.
fn read_media(v: &Value) -> Option<CbcMedia> {
    let show = v.get("showName").and_then(Value::as_str)?.trim();
    let media = match v.get("type").and_then(Value::as_str)? {
        "audio" => super::Media::Audio,
        "video" => super::Media::Video,
        _ => return None,
    };
    let inner = v.get("media");
    let duration = inner
        .and_then(|m| m.get("duration"))
        .or_else(|| v.get("mediaDuration"))
        .and_then(Value::as_u64)
        .filter(|d| *d > 0);
    let area = inner
        .and_then(|m| m.get("contentArea"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let string = |key: &str| {
        v.get(key)
            .and_then(Value::as_str)
            .map(|s| super::html_unescape(s.trim()))
            .unwrap_or_default()
    };
    Some(CbcMedia {
        media,
        title: string("title"),
        show: show.to_string(),
        network: if area.eq_ignore_ascii_case("radio") {
            "CBC Radio".to_string()
        } else {
            "CBC".to_string()
        },
        description: string("description"),
        date: v
            .get("publishedAt")
            .and_then(Value::as_i64)
            .map(epoch_ms_to_date)
            .unwrap_or_default(),
        duration,
    })
}

/// The `window.__INITIAL_STATE__ = {…}` object, parsed. It is a JavaScript literal, not JSON:
/// bare `undefined` values are rewritten to `null` first.
fn initial_state(html: &str) -> Option<Value> {
    let at = html.find("__INITIAL_STATE__")?;
    let open = at + html[at..].find('{')?;
    let close = open + html[open..].find("</script")?;
    let js = html[open..close].trim_end().trim_end_matches(';');
    serde_json::from_str(&undefined_to_null(js)).ok()
}

/// Replace bare `undefined` tokens (outside string literals) with `null`.
fn undefined_to_null(js: &str) -> String {
    let mut out = String::with_capacity(js.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut rest = js;
    while let Some(c) = rest.chars().next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if rest.starts_with("undefined")
            && !out
                .chars()
                .last()
                .is_some_and(|p| p.is_alphanumeric() || p == '_')
        {
            out.push_str("null");
            rest = &rest["undefined".len()..];
            continue;
        } else {
            out.push(c);
        }
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Milliseconds since the Unix epoch → `YYYY-MM-DD` (UTC).
fn epoch_ms_to_date(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_dates() {
        assert_eq!(epoch_ms_to_date(0), "1970-01-01");
        // Jul 31 2026, 09:00 EDT.
        assert_eq!(epoch_ms_to_date(1_785_502_800_000), "2026-07-31");
        assert_eq!(epoch_ms_to_date(951_782_400_000), "2000-02-29");
    }

    #[test]
    fn undefined_only_outside_strings() {
        assert_eq!(
            undefined_to_null(r#"{"a":undefined,"b":"undefined","c":[undefined]}"#),
            r#"{"a":null,"b":"undefined","c":[null]}"#
        );
    }

    #[test]
    fn prefers_the_story_body_media() {
        let html = r#"<script>window.__INITIAL_STATE__ = {"sidebar":{"x":{"type":"audio",
            "showName":"Other Show","title":"Unrelated","media":{"duration":60}}},
            "detail":{"content":{"body":[{"type":"polopoly_media","content":{"type":"audio",
            "title":"Empire of AI ","showName":"Ideas","publishedAt":1785502800000,
            "description":"in this conversation with Nahlah Ayed","creator":undefined,
            "media":{"duration":3240,"contentArea":"Radio"}}}]}}};</script>"#;
        let m = media(html).unwrap();
        assert_eq!(m.title, "Empire of AI");
        assert_eq!(m.show, "Ideas");
        assert_eq!(m.network, "CBC Radio");
        assert_eq!(m.date, "2026-07-31");
        assert_eq!(m.duration, Some(3240));
    }
}
