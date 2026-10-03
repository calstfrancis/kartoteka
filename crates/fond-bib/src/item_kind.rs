//! The kinds of work the GUI offers ("Book", "Podcast episode", "Interview", …) and how each is
//! written as Hayagriva YAML. Most kinds are one Hayagriva `type`, but recorded media need more
//! than that to cite properly, because of how Hayagriva maps entries onto CSL:
//!
//! - An `audio`/`video` entry has **no container** in Hayagriva (`Entry::get_container`), so a
//!   podcast episode typed `audio` with its show as `parent:` renders without the show's name
//!   in every style. A `scene` whose parent is `audio`/`video` is the one shape that carries
//!   the show/series as `container-title` — so episodes are `scene` > `audio` (podcast, radio)
//!   or `scene` > `video` (TV, a web series).
//! - Hayagriva never produces CSL's `interview` type, nor the `interviewer`/`host`/`guest`
//!   name variables (they resolve to nothing in `hayagriva::csl::taxonomy`), and its
//!   `PersonRole::Unknown` can't be written to YAML. So an interview is cited the way Chicago
//!   and MLA phrase it — the person interviewed as author, and **"Interview by NAME"** as the
//!   entry's `genre`, which Chicago, MLA and SBL print right after the title and APA prints as
//!   the bracketed description. [`interviewer_from_genre`]/[`genre_for_interviewer`] keep the
//!   interviewer editable as a name rather than as that phrase.
//! - `genre` also carries the medium label APA expects ("Podcast episode", "Video"), and
//!   `runtime` (CSL `dimensions`) the running time.
//!
//! These shapes were chosen by rendering each candidate through Chicago (notes and
//! author-date), APA, MLA and SBL — see `tests` below for the shapes they settled on.

use crate::entry::EntryFields;

/// One kind of work offered in the type dropdowns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemKind {
    /// What the dropdown shows.
    pub label: &'static str,
    /// The Hayagriva `type` written for it.
    pub entry_type: &'static str,
    /// The `type` a newly created `parent:` (its journal, book, show, …) gets.
    pub parent_type: &'static str,
    /// The `genre` written for it by default, if any (the medium label citation styles print).
    pub genre: Option<&'static str>,
    /// Whether this kind sits inside another work (journal, book, show), so the editor offers
    /// its container and numbers up front.
    pub has_container: bool,
    /// Recorded audio/video — the editor offers a running time.
    pub is_media: bool,
}

const fn kind(
    label: &'static str,
    entry_type: &'static str,
    parent_type: &'static str,
    has_container: bool,
) -> ItemKind {
    ItemKind {
        label,
        entry_type,
        parent_type,
        genre: None,
        has_container,
        is_media: false,
    }
}

const fn media(
    label: &'static str,
    entry_type: &'static str,
    parent_type: &'static str,
    genre: Option<&'static str>,
    has_container: bool,
) -> ItemKind {
    ItemKind {
        label,
        entry_type,
        parent_type,
        genre,
        has_container,
        is_media: true,
    }
}

/// Every kind offered, in dropdown order. Where two kinds share a Hayagriva type, [`kind_of`]
/// tells them apart by parent type and genre; otherwise the first one listed wins.
pub const ITEM_KINDS: &[ItemKind] = &[
    kind("Book", "book", "periodical", false),
    kind("Journal article", "article", "periodical", true),
    kind("Book chapter", "chapter", "anthology", true),
    kind("Conference paper", "conference", "proceedings", true),
    kind("Report", "report", "periodical", false),
    kind("Thesis", "thesis", "periodical", false),
    kind("Manuscript", "manuscript", "periodical", false),
    // Hayagriva has no native sermon entry type — "manuscript" (an unpublished written text)
    // is the closest fit and works whether or not the sermon was ever recorded or posted.
    kind("Sermon", "manuscript", "periodical", false),
    kind("Web page", "web", "periodical", true),
    kind("Blog post", "blog", "periodical", true),
    kind("Newspaper article", "newspaper", "periodical", true),
    media(
        "Podcast episode",
        "scene",
        "audio",
        Some("Podcast episode"),
        true,
    ),
    // Default parent `audio`; an interview already inside a video series keeps its `video`
    // parent (see `retype`).
    media("Interview", "scene", "audio", Some(INTERVIEW), true),
    media("TV or video episode", "scene", "video", None, true),
    media("Video", "video", "video", Some("Video"), false),
    media("Audio recording", "audio", "audio", None, false),
    kind("Anthology", "anthology", "anthology", true),
    kind("Periodical", "periodical", "periodical", false),
    kind("Miscellaneous", "misc", "periodical", false),
];

/// The genre an interview with no interviewer named carries.
const INTERVIEW: &str = "Interview";

/// Look a kind up by its label.
pub fn by_label(label: &str) -> Option<&'static ItemKind> {
    ITEM_KINDS.iter().find(|k| k.label == label)
}

/// Index into [`ITEM_KINDS`] of the kind an entry with these fields is, or `None` for a
/// Hayagriva type no kind covers (the caller keeps it as-is rather than retyping it).
pub fn kind_of(fields: &EntryFields) -> Option<usize> {
    let ty = fields.entry_type.as_str();
    if ty == "scene" {
        let label = if interviewer_from_genre(&fields.genre).is_some() {
            "Interview"
        } else if fields.parent_type == "video" {
            "TV or video episode"
        } else {
            "Podcast episode"
        };
        return ITEM_KINDS.iter().position(|k| k.label == label);
    }
    ITEM_KINDS.iter().position(|k| k.entry_type == ty)
}

/// Re-type `fields` as `to`: its Hayagriva type, its parent's type, and its genre — but a
/// genre the person wrote themselves (not the previous kind's default, nor an interview line
/// the new kind doesn't want) is kept.
pub fn retype(fields: &mut EntryFields, to: &ItemKind) {
    let from_genre_is_default = fields.genre.is_empty()
        || ITEM_KINDS
            .iter()
            .any(|k| k.genre == Some(fields.genre.as_str()))
        || interviewer_from_genre(&fields.genre).is_some();
    fields.entry_type = to.entry_type.to_string();
    let keep_media_parent =
        to.label == "Interview" && matches!(fields.parent_type.as_str(), "audio" | "video");
    if !keep_media_parent {
        fields.parent_type = to.parent_type.to_string();
    }
    if from_genre_is_default {
        let keeps_interviewer =
            to.label == "Interview" && interviewer_from_genre(&fields.genre).is_some();
        if !keeps_interviewer {
            fields.genre = to.genre.unwrap_or_default().to_string();
        }
    }
}

/// The interviewer named by an interview's genre — `Some("")` for a bare "Interview", `None`
/// when the genre isn't an interview line at all.
pub fn interviewer_from_genre(genre: &str) -> Option<String> {
    let g = genre.trim();
    let lower = g.to_lowercase();
    if lower == "interview" {
        return Some(String::new());
    }
    lower
        .strip_prefix("interview by ")
        .map(|_| g["interview by ".len()..].trim().to_string())
}

/// The genre that names `interviewer`: "Interview by NAME", or plain "Interview".
pub fn genre_for_interviewer(interviewer: &str) -> String {
    let name = interviewer.trim();
    if name.is_empty() {
        INTERVIEW.to_string()
    } else {
        format!("Interview by {name}")
    }
}

/// Normalize a typed or scraped running time into the `[[hh:]mm:]ss` form Hayagriva's
/// `runtime` field parses (`54:00`, `01:02:03`) (anything else fails the entry's parse). Accepts `54:00`,
/// `1:02:03`, ISO 8601 (`PT54M`, `PT1H2M3S`), plain seconds (`3240`), and spelled-out units
/// (`54 min`, `1h 2m`, `1 hr 30 mins`). `None` when it can't be read.
pub fn normalize_runtime(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let seconds = if s.contains(':') {
        let parts: Vec<u64> = s
            .split(':')
            .map(|p| p.trim().parse::<u64>().ok())
            .collect::<Option<_>>()?;
        if parts.len() > 3 {
            return None;
        }
        parts.iter().fold(0, |acc, p| acc * 60 + p)
    } else if let Ok(n) = s.parse::<u64>() {
        n
    } else {
        unit_seconds(
            s.strip_prefix("PT")
                .or_else(|| s.strip_prefix("pt"))
                .unwrap_or(s),
        )?
    };
    if seconds == 0 {
        return None;
    }
    Some(format_seconds(seconds))
}

/// `1h 2m 3s`, `54 min`, `1H2M3S` → seconds.
fn unit_seconds(s: &str) -> Option<u64> {
    let mut total = 0;
    let mut number = String::new();
    let mut unit = String::new();
    let mut found = false;
    let mut flush = |number: &mut String, unit: &mut String| -> Option<()> {
        if number.is_empty() {
            return if unit.is_empty() { Some(()) } else { None };
        }
        let n: u64 = number.parse().ok()?;
        let u = unit.to_lowercase();
        let factor = match u.as_str() {
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            _ => return None,
        };
        total += n * factor;
        found = true;
        number.clear();
        unit.clear();
        Some(())
    };
    for c in s.chars() {
        if c.is_ascii_digit() {
            if !unit.is_empty() {
                flush(&mut number, &mut unit)?;
            }
            number.push(c);
        } else if c.is_alphabetic() {
            unit.push(c);
        } else if c.is_whitespace() || c == ',' {
            if !unit.is_empty() {
                flush(&mut number, &mut unit)?;
            }
        } else {
            return None;
        }
    }
    flush(&mut number, &mut unit)?;
    found.then_some(total)
}

/// Seconds as `mm:ss` / `hh:mm:ss` (Hayagriva's duration parser wants two digits a part).
pub fn format_seconds(total: u64) -> String {
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(ty: &str, parent: &str, genre: &str) -> EntryFields {
        EntryFields {
            entry_type: ty.into(),
            parent_type: parent.into(),
            genre: genre.into(),
            ..Default::default()
        }
    }

    fn label_of(f: &EntryFields) -> &'static str {
        ITEM_KINDS[kind_of(f).unwrap()].label
    }

    #[test]
    fn media_kinds_are_told_apart() {
        assert_eq!(
            label_of(&fields("scene", "audio", "Podcast episode")),
            "Podcast episode"
        );
        assert_eq!(label_of(&fields("scene", "audio", "")), "Podcast episode");
        assert_eq!(
            label_of(&fields("scene", "audio", "Interview by Nahlah Ayed")),
            "Interview"
        );
        assert_eq!(
            label_of(&fields("scene", "video", "Interview")),
            "Interview"
        );
        assert_eq!(
            label_of(&fields("scene", "video", "")),
            "TV or video episode"
        );
        assert_eq!(label_of(&fields("video", "", "Video")), "Video");
        assert_eq!(label_of(&fields("audio", "", "")), "Audio recording");
        assert_eq!(label_of(&fields("book", "", "")), "Book");
        // Two kinds share `manuscript`; the first listed wins, as before.
        assert_eq!(label_of(&fields("manuscript", "", "")), "Manuscript");
        assert_eq!(kind_of(&fields("patent", "", "")), None);
    }

    #[test]
    fn retype_sets_parent_and_genre_but_keeps_a_custom_genre() {
        let mut f = fields("web", "periodical", "");
        retype(&mut f, by_label("Podcast episode").unwrap());
        assert_eq!(
            (f.entry_type.as_str(), f.parent_type.as_str()),
            ("scene", "audio")
        );
        assert_eq!(f.genre, "Podcast episode");

        retype(&mut f, by_label("Interview").unwrap());
        assert_eq!(f.genre, "Interview");
        f.genre = genre_for_interviewer("Nahlah Ayed");

        // Back to a podcast episode: the interview line goes.
        retype(&mut f, by_label("Podcast episode").unwrap());
        assert_eq!(f.genre, "Podcast episode");

        // An interview inside a video series stays in it.
        let mut f = fields("scene", "video", "");
        retype(&mut f, by_label("Interview").unwrap());
        assert_eq!(f.parent_type, "video");

        // A genre the person typed survives a change of kind.
        let mut f = fields("video", "", "Lecture recording");
        retype(&mut f, by_label("Audio recording").unwrap());
        assert_eq!(f.genre, "Lecture recording");
    }

    #[test]
    fn interviewer_round_trips_through_genre() {
        assert_eq!(
            interviewer_from_genre("Interview by Nahlah Ayed").as_deref(),
            Some("Nahlah Ayed")
        );
        assert_eq!(interviewer_from_genre("interview").as_deref(), Some(""));
        assert_eq!(interviewer_from_genre("Podcast episode"), None);
        assert_eq!(
            genre_for_interviewer(" Nahlah Ayed "),
            "Interview by Nahlah Ayed"
        );
        assert_eq!(genre_for_interviewer(""), "Interview");
    }

    #[test]
    fn runtimes_normalize_to_what_hayagriva_parses() {
        for (input, want) in [
            ("54:00", "54:00"),
            ("3240", "54:00"),
            ("PT54M", "54:00"),
            ("PT1H2M3S", "01:02:03"),
            ("PT0M19S", "00:19"),
            ("1h 2m", "01:02:00"),
            ("54 min", "54:00"),
            ("1 hr 30 mins", "01:30:00"),
            ("90:00", "01:30:00"),
        ] {
            let got = normalize_runtime(input).unwrap_or_else(|| panic!("{input}"));
            assert_eq!(got, want, "{input}");
            assert!(
                got.parse::<hayagriva::types::Duration>().is_ok(),
                "{got} must parse as a Hayagriva duration"
            );
        }
        for bad in ["", "soon", "1:2:3:4", "0", "5 parsecs"] {
            assert_eq!(normalize_runtime(bad), None, "{bad}");
        }
    }
}
