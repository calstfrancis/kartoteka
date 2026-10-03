//! Import from reference-manager exports other than BibTeX: **RIS** (what Zotero, Mendeley,
//! EndNote, and every publisher's "Export citation" button produce) and **CSL-JSON** (Zotero's
//! and Better BibTeX's JSON export).
//!
//! Both readers turn each source record into a format-neutral [`Work`], and one writer
//! ([`Library::import_records`]) puts works into the library — so keys, tags, duplicate
//! handling and the report behave the same whichever format came in.
//!
//! **Citation keys.** Better BibTeX keys are what a Zotero user's Typst documents already
//! cite, so a CSL-JSON record's `citation-key` (or a plain-looking `id`) is kept as the key.
//! RIS carries no usable key, so those entries get fresh keys.

use std::path::Path;

use serde_json::Value as Json;
use serde_yaml_ng::{Mapping, Value};

use crate::creator::{Creator, CreatorRole};
use crate::error::{BibError, Result};
use crate::import::{ImportOptions, ImportReport};
use crate::library::Library;
use crate::{entry, util};

/// What kind of work a record describes — the small common ground between RIS and CSL types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Article,
    Newspaper,
    Book,
    Chapter,
    Conference,
    Thesis,
    Report,
    Web,
    Blog,
    Manuscript,
    Misc,
}

impl Kind {
    /// `(Hayagriva entry type, type of the parent that holds the container title)`.
    fn hayagriva(self) -> (&'static str, &'static str) {
        match self {
            Kind::Article => ("article", "periodical"),
            Kind::Newspaper => ("article", "newspaper"),
            Kind::Book => ("book", "book"),
            Kind::Chapter => ("chapter", "anthology"),
            Kind::Conference => ("conference", "proceedings"),
            Kind::Thesis => ("thesis", "misc"),
            Kind::Report => ("report", "misc"),
            Kind::Web => ("web", "web"),
            Kind::Blog => ("blog", "web"),
            Kind::Manuscript => ("manuscript", "misc"),
            Kind::Misc => ("misc", "misc"),
        }
    }
}

/// One imported reference, independent of the format it came from.
#[derive(Debug, Clone, Default)]
pub struct Work {
    pub kind: Option<Kind>,
    /// A citation key to keep, if the source supplied a usable one.
    pub key: Option<String>,
    pub title: String,
    pub creators: Vec<Creator>,
    /// `YYYY`, `YYYY-MM` or `YYYY-MM-DD`.
    pub date: String,
    /// Journal, book or proceedings title.
    pub container: String,
    pub volume: String,
    pub issue: String,
    pub pages: String,
    pub publisher: String,
    pub place: String,
    pub doi: String,
    pub isbn: String,
    pub issn: String,
    pub url: String,
    pub language: String,
    pub edition: String,
    pub tags: Vec<String>,
    /// Source fields that had no home in the entry, for the report.
    pub unmapped: Vec<String>,
}

impl Work {
    /// The entry's fields as a Hayagriva mapping (without its key).
    fn fields(&self) -> Result<Mapping> {
        let s = |v: &str| Value::String(v.to_string());
        let (entry_type, parent_type) = self.kind.unwrap_or(Kind::Misc).hayagriva();
        let mut m = Mapping::new();
        m.insert(s("type"), s(entry_type));
        if !self.title.trim().is_empty() {
            m.insert(s("title"), s(self.title.trim()));
        }
        entry::write_creators_into(&mut m, &self.creators)?;
        if !self.date.is_empty() {
            m.insert(s("date"), s(&self.date));
        }
        let set = |m: &mut Mapping, key: &str, value: &str| {
            if !value.trim().is_empty() {
                m.insert(
                    Value::String(key.to_string()),
                    Value::String(value.trim().to_string()),
                );
            }
        };
        set(&mut m, "edition", &self.edition);
        set(&mut m, "language", &self.language);
        set(&mut m, "volume", &self.volume);
        set(&mut m, "issue", &self.issue);
        set(&mut m, "page-range", &self.pages);
        if !self.url.trim().is_empty() {
            m.insert(s("url"), s(self.url.trim()));
        }
        // publisher + place share one key, as everywhere else in the library.
        match (self.publisher.trim(), self.place.trim()) {
            ("", "") => {}
            (name, "") => {
                m.insert(s("publisher"), s(name));
            }
            (name, place) => {
                let mut p = Mapping::new();
                if !name.is_empty() {
                    p.insert(s("name"), s(name));
                }
                p.insert(s("location"), s(place));
                m.insert(s("publisher"), Value::Mapping(p));
            }
        }
        let mut serial = Mapping::new();
        set(&mut serial, "doi", &self.doi);
        set(&mut serial, "isbn", &self.isbn);
        set(&mut serial, "issn", &self.issn);
        if !serial.is_empty() {
            m.insert(s("serial-number"), Value::Mapping(serial));
        }
        if !self.container.trim().is_empty() {
            let mut parent = Mapping::new();
            parent.insert(s("type"), s(parent_type));
            parent.insert(s("title"), s(self.container.trim()));
            m.insert(s("parent"), Value::Mapping(parent));
        }
        Ok(m)
    }

    /// The entry as one-key Hayagriva YAML under `key`.
    fn to_yaml(&self, key: &str) -> Result<String> {
        let mut outer = Mapping::new();
        outer.insert(
            Value::String(key.to_string()),
            Value::Mapping(self.fields()?),
        );
        serde_yaml_ng::to_string(&Value::Mapping(outer)).map_err(|e| BibError::Import {
            message: format!("could not build entry YAML: {e}"),
        })
    }
}

// --- RIS ----------------------------------------------------------------------------------

/// Parse RIS text into works. Tolerates a byte-order mark, CRLF line endings, continuation
/// lines, and records missing their closing `ER` tag.
pub fn parse_ris(text: &str) -> Result<Vec<Work>> {
    let text = text.trim_start_matches('\u{feff}');
    // (tag, value) pairs for the record being read.
    let mut current: Vec<(String, String)> = Vec::new();
    let mut works = Vec::new();

    let finish = |current: &mut Vec<(String, String)>, works: &mut Vec<Work>| {
        if !current.is_empty() {
            works.push(ris_work(current));
            current.clear();
        }
    };

    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some((tag, value)) = ris_line(line) {
            match tag {
                "TY" => {
                    finish(&mut current, &mut works);
                    current.push((tag.to_string(), value.to_string()));
                }
                "ER" => finish(&mut current, &mut works),
                _ => current.push((tag.to_string(), value.to_string())),
            }
        } else if !line.trim().is_empty() {
            // A continuation of the previous value.
            if let Some((_, value)) = current.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        }
    }
    finish(&mut current, &mut works);

    if works.is_empty() {
        return Err(BibError::Import {
            message: "no RIS records found in that file".to_string(),
        });
    }
    Ok(works)
}

/// `TY  - JOUR` → `("TY", "JOUR")`. The tag is two upper-case letters/digits; the separator is
/// a dash with at least one space before it.
fn ris_line(line: &str) -> Option<(&str, &str)> {
    let tag = line.get(..2)?;
    if !tag
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return None;
    }
    let rest = line.get(2..)?;
    if !rest.starts_with(' ') {
        return None;
    }
    let rest = rest.trim_start().strip_prefix('-')?;
    Some((tag, rest.trim()))
}

fn ris_kind(code: &str) -> Kind {
    match code {
        "JOUR" | "JFULL" | "MGZN" | "EJOUR" | "ABST" => Kind::Article,
        "NEWS" => Kind::Newspaper,
        "BOOK" | "EBOOK" | "EDBOOK" | "SER" => Kind::Book,
        "CHAP" | "ECHAP" => Kind::Chapter,
        "CONF" | "CPAPER" => Kind::Conference,
        "THES" => Kind::Thesis,
        "RPRT" => Kind::Report,
        "ELEC" | "WEB" => Kind::Web,
        "BLOG" => Kind::Blog,
        "MANSCPT" | "UNPB" => Kind::Manuscript,
        _ => Kind::Misc,
    }
}

fn ris_work(fields: &[(String, String)]) -> Work {
    let get = |tags: &[&str]| -> String {
        tags.iter()
            .find_map(|t| fields.iter().find(|(k, v)| k == t && !v.is_empty()))
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let all = |tags: &[&str]| -> Vec<String> {
        fields
            .iter()
            .filter(|(k, v)| tags.contains(&k.as_str()) && !v.is_empty())
            .map(|(_, v)| v.clone())
            .collect()
    };

    let kind = ris_kind(&get(&["TY"]));
    let mut work = Work {
        kind: Some(kind),
        title: get(&["TI", "T1"]),
        // Chapters and papers keep their book/proceedings title in T2 (or BT); journals in JO/JF/T2.
        container: get(&["T2", "JO", "JF", "BT", "JA"]),
        volume: get(&["VL"]),
        issue: get(&["IS", "CP"]),
        publisher: get(&["PB"]),
        place: get(&["CY"]),
        doi: clean_doi(&get(&["DO", "DI"])),
        url: get(&["UR", "L1", "L2"]),
        language: get(&["LA"]),
        edition: get(&["ET"]),
        tags: all(&["KW"]),
        date: ris_date(&get(&["DA", "PY", "Y1"])),
        ..Work::default()
    };

    let (start, end) = (get(&["SP"]), get(&["EP"]));
    work.pages = match (start.as_str(), end.as_str()) {
        ("", "") => String::new(),
        (s, "") => s.to_string(),
        (s, e) => format!("{s}-{e}"),
    };

    // SN holds an ISBN for books and an ISSN for serials; tell them apart by the check digit.
    let sn = get(&["SN"]);
    if let Some(isbn) = crate::identify::isbn13(&sn) {
        work.isbn = isbn;
    } else if looks_like_issn(&sn) {
        work.issn = sn.trim().to_string();
    }

    for tag in ["AU", "A1"] {
        for name in all(&[tag]) {
            work.creators.push(person(CreatorRole::Author, &name));
        }
    }
    for name in all(&["A2", "ED"]) {
        work.creators.push(person(CreatorRole::Editor, &name));
    }
    for name in all(&["A4", "TA"]) {
        work.creators.push(person(CreatorRole::Translator, &name));
    }

    const MAPPED: &[&str] = &[
        "TY", "TI", "T1", "T2", "BT", "JO", "JF", "JA", "VL", "IS", "CP", "PB", "CY", "DO", "DI",
        "UR", "L1", "L2", "LA", "ET", "KW", "DA", "PY", "Y1", "SP", "EP", "SN", "AU", "A1", "A2",
        "ED", "A4", "TA", "ID", "ER",
    ];
    let mut unmapped: Vec<String> = fields
        .iter()
        .map(|(k, _)| k.clone())
        .filter(|k| !MAPPED.contains(&k.as_str()))
        .collect();
    unmapped.sort();
    unmapped.dedup();
    work.unmapped = unmapped;
    work
}

/// RIS dates are `YYYY`, `YYYY/MM/DD/other`, `YYYY/MM//` or `YYYY-MM-DD`.
fn ris_date(raw: &str) -> String {
    let parts: Vec<&str> = raw.split(['/', '-']).map(str::trim).collect();
    let number = |i: usize, digits: usize| {
        parts
            .get(i)
            .filter(|p| p.len() <= digits && !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            .and_then(|p| p.parse::<u32>().ok())
    };
    let Some(year) = parts
        .first()
        .filter(|p| p.len() == 4 && p.chars().all(|c| c.is_ascii_digit()))
    else {
        return String::new();
    };
    match (
        number(1, 2).filter(|m| (1..=12).contains(m)),
        number(2, 2).filter(|d| (1..=31).contains(d)),
    ) {
        (Some(m), Some(d)) => format!("{year}-{m:02}-{d:02}"),
        (Some(m), None) => format!("{year}-{m:02}"),
        _ => (*year).to_string(),
    }
}

/// `Cone, James H.` → family/given; a name with no comma is taken as one unit (an organization).
fn person(role: CreatorRole, name: &str) -> Creator {
    match name.split_once(',') {
        Some((family, given)) => Creator::new(role, family.trim(), given.trim()),
        None => Creator::new_single_field(role, name.trim()),
    }
}

fn clean_doi(raw: &str) -> String {
    let d = raw.trim();
    for prefix in [
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
        "doi:",
        "DOI:",
    ] {
        if let Some(rest) = d.strip_prefix(prefix) {
            return rest.trim().to_string();
        }
    }
    d.to_string()
}

fn looks_like_issn(s: &str) -> bool {
    let s = s.trim();
    let digits: Vec<char> = s.chars().filter(|c| *c != '-').collect();
    digits.len() == 8
        && digits[..7].iter().all(char::is_ascii_digit)
        && (digits[7].is_ascii_digit() || matches!(digits[7], 'X' | 'x'))
        && s.chars().filter(|c| *c == '-').count() <= 1
}

// --- CSL-JSON -----------------------------------------------------------------------------

/// Parse CSL-JSON (an array of items, a single item, or `{"items": [...]}`) into works.
pub fn parse_csl_json(text: &str) -> Result<Vec<Work>> {
    let root: Json = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| {
        BibError::Import {
            message: format!("that file isn't valid CSL-JSON: {e}"),
        }
    })?;
    let items: Vec<&Json> = match &root {
        Json::Array(a) => a.iter().collect(),
        Json::Object(o) if o.contains_key("items") => o["items"]
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_default(),
        Json::Object(_) => vec![&root],
        _ => Vec::new(),
    };
    let works: Vec<Work> = items
        .into_iter()
        .filter(|i| i.is_object())
        .map(csl_work)
        .collect();
    if works.is_empty() {
        return Err(BibError::Import {
            message: "no references found in that CSL-JSON file".to_string(),
        });
    }
    Ok(works)
}

fn csl_kind(t: &str) -> Kind {
    match t {
        "article-journal" | "article-magazine" | "article" => Kind::Article,
        "article-newspaper" => Kind::Newspaper,
        "book" => Kind::Book,
        "chapter" | "entry-encyclopedia" | "entry-dictionary" | "entry" => Kind::Chapter,
        "paper-conference" => Kind::Conference,
        "thesis" => Kind::Thesis,
        "report" => Kind::Report,
        "webpage" => Kind::Web,
        "post-weblog" | "post" => Kind::Blog,
        "manuscript" => Kind::Manuscript,
        _ => Kind::Misc,
    }
}

fn csl_work(item: &Json) -> Work {
    let text = |k: &str| -> String {
        match &item[k] {
            Json::String(s) => s.trim().to_string(),
            Json::Number(n) => n.to_string(),
            _ => String::new(),
        }
    };

    let mut work = Work {
        kind: Some(csl_kind(&text("type"))),
        title: text("title"),
        container: text("container-title"),
        volume: text("volume"),
        issue: text("issue"),
        pages: text("page"),
        publisher: text("publisher"),
        place: text("publisher-place"),
        doi: clean_doi(&text("DOI")),
        url: text("URL"),
        language: text("language"),
        edition: text("edition"),
        ..Work::default()
    };
    if let Some(isbn) = crate::identify::isbn13(&text("ISBN")) {
        work.isbn = isbn;
    }
    work.issn = text("ISSN");
    if !looks_like_issn(&work.issn) {
        work.issn.clear();
    }
    work.date = csl_date(&item["issued"]);
    work.key = ["citation-key", "id"]
        .iter()
        .map(|k| text(k))
        .find(|k| plausible_key(k));

    for (field, role) in [
        ("author", CreatorRole::Author),
        ("editor", CreatorRole::Editor),
        ("translator", CreatorRole::Translator),
    ] {
        for p in item[field].as_array().into_iter().flatten() {
            let (family, given, literal) = (
                p["family"].as_str().unwrap_or("").trim(),
                p["given"].as_str().unwrap_or("").trim(),
                p["literal"].as_str().unwrap_or("").trim(),
            );
            if !family.is_empty() {
                work.creators.push(Creator::new(role, family, given));
            } else if !literal.is_empty() {
                work.creators.push(Creator::new_single_field(role, literal));
            }
        }
    }

    // Zotero writes tags as a `keyword` string ("a, b") in some exports.
    work.tags = text("keyword")
        .split([',', ';'])
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();

    const MAPPED: &[&str] = &[
        "id",
        "citation-key",
        "type",
        "title",
        "container-title",
        "volume",
        "issue",
        "page",
        "publisher",
        "publisher-place",
        "DOI",
        "URL",
        "language",
        "edition",
        "ISBN",
        "ISSN",
        "issued",
        "author",
        "editor",
        "translator",
        "keyword",
    ];
    work.unmapped = item
        .as_object()
        .map(|o| {
            o.keys()
                .filter(|k| !MAPPED.contains(&k.as_str()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    work
}

fn csl_date(issued: &Json) -> String {
    let part = |i: usize| issued["date-parts"][0][i].as_i64().filter(|n| *n > 0);
    match (part(0), part(1), part(2)) {
        (Some(y), Some(m), Some(d)) => format!("{y:04}-{m:02}-{d:02}"),
        (Some(y), Some(m), None) => format!("{y:04}-{m:02}"),
        (Some(y), None, _) => format!("{y:04}"),
        _ => {
            // `raw`/`literal` dates: take a leading four-digit year if there is one.
            let raw = issued["raw"]
                .as_str()
                .or(issued["literal"].as_str())
                .unwrap_or("");
            raw.split(|c: char| !c.is_ascii_digit())
                .find(|p| p.len() == 4)
                .map(str::to_string)
                .unwrap_or_default()
        }
    }
}

/// Whether `id` looks like a citation key worth keeping (`cone1970black`, `Cone:1970`) rather
/// than an opaque identifier or URL (`http://zotero.org/users/1/items/AB12`).
fn plausible_key(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty()
        && id.len() <= 80
        && id.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.'))
        && !id.contains("..")
}

// --- Writing them into a library ----------------------------------------------------------

impl Library {
    /// Import RIS text. See [`Library::import_works`].
    pub fn import_ris(&self, text: &str, opts: &ImportOptions) -> Result<ImportReport> {
        self.import_works(parse_ris(text)?, opts)
    }

    /// Import CSL-JSON text. See [`Library::import_works`].
    pub fn import_csl_json(&self, text: &str, opts: &ImportOptions) -> Result<ImportReport> {
        self.import_works(parse_csl_json(text)?, opts)
    }

    /// Write `works` into the library and regenerate `library.yml`.
    ///
    /// A work that carries a usable citation key keeps it; the rest get fresh keys. A work whose
    /// DOI or ISBN is already in the library is left alone and reported (as a collision, with
    /// the existing key) — re-importing the same export adds nothing. A kept key that already
    /// exists is skipped too, unless `opts.overwrite`. Tags go into each entry's note.
    pub fn import_works(&self, works: Vec<Work>, opts: &ImportOptions) -> Result<ImportReport> {
        let mut report = ImportReport::default();
        let today = util::today_iso();
        let mut keyless: Vec<(entry::ParsedEntry, Work)> = Vec::new();
        let mut written: Vec<(String, Work)> = Vec::new();

        for work in works {
            let label = if work.title.is_empty() {
                "(untitled)".to_string()
            } else {
                work.title.clone()
            };

            let existing = if !work.doi.is_empty() {
                self.find_entry_by_doi(&work.doi)?
            } else if !work.isbn.is_empty() {
                self.find_entry_by_isbn(&work.isbn)?
            } else {
                None
            };
            if let Some(existing) = existing {
                if !opts.overwrite || work.key.as_deref() != Some(existing.as_str()) {
                    report.skipped_key_collisions.push(existing);
                    continue;
                }
            }

            match work.key.clone() {
                Some(key) => {
                    if self.entry_path(&key).exists() && !opts.overwrite {
                        report.skipped_key_collisions.push(key);
                        continue;
                    }
                    let parsed = entry::parse_single(&work.to_yaml(&key)?, Path::new("<import>"))?;
                    self.write_entry(&parsed.entry)?;
                    report.imported.push(key.clone());
                    if !work.unmapped.is_empty() {
                        report
                            .unmapped_fields
                            .push((key.clone(), work.unmapped.clone()));
                    }
                    written.push((key, work));
                }
                None => {
                    let yaml = work.to_yaml("_")?;
                    let parsed =
                        entry::parse_single(&yaml, Path::new("<import>")).map_err(|_| {
                            BibError::Import {
                                message: format!("could not read the reference \"{label}\""),
                            }
                        })?;
                    keyless.push((parsed, work));
                }
            }
        }

        if !keyless.is_empty() {
            let entries: Vec<_> = keyless.iter().map(|(p, _)| p.entry.clone()).collect();
            let keys = self.add_entries(&entries)?;
            for (key, (_, work)) in keys.into_iter().zip(keyless) {
                report.imported.push(key.clone());
                if !work.unmapped.is_empty() {
                    report
                        .unmapped_fields
                        .push((key.clone(), work.unmapped.clone()));
                }
                written.push((key, work));
            }
        }

        // Tags (and a date-added) go in each entry's note.
        for (key, work) in &written {
            if work.tags.is_empty() {
                continue;
            }
            let mut note = self.load_note(key)?.unwrap_or_default();
            for tag in &work.tags {
                if !note.frontmatter.tags.contains(tag) {
                    note.frontmatter.tags.push(tag.clone());
                }
            }
            if note.frontmatter.date_added.is_none() {
                note.frontmatter.date_added = Some(today.clone());
            }
            self.write_note(key, &note)?;
            report.tags_written.push((key.clone(), work.tags.len()));
        }

        self.regenerate_library_yml()?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RIS: &str = "\u{feff}TY  - JOUR\r\nAU  - Cone, James H.\r\nAU  - Roberts, J. Deotis\r\nTI  - Black Theology and Black Power:\r\n      a first look\r\nJO  - Christianity and Crisis\r\nVL  - 30\r\nIS  - 1\r\nSP  - 6\r\nEP  - 13\r\nPY  - 1970///\r\nDA  - 1970/03/15/\r\nDO  - https://doi.org/10.2307/abc\r\nSN  - 0009-5281\r\nKW  - christology\r\nKW  - liberation\r\nN1  - a private note\r\nER  - \r\n\r\nTY  - BOOK\r\nAU  - Berdyaev, Nikolai\r\nA4  - Duddington, Natalie\r\nT1  - The Destiny of Man\r\nPB  - Geoffrey Bles\r\nCY  - London\r\nPY  - 1937\r\nSN  - 978-0-14-044913-6\r\nER  - \r\n";

    #[test]
    fn ris_records_are_read_with_their_fields() {
        let works = parse_ris(RIS).unwrap();
        assert_eq!(works.len(), 2);
        let a = &works[0];
        assert_eq!(a.kind, Some(Kind::Article));
        assert_eq!(a.title, "Black Theology and Black Power: a first look");
        assert_eq!(a.container, "Christianity and Crisis");
        assert_eq!(
            (a.volume.as_str(), a.issue.as_str(), a.pages.as_str()),
            ("30", "1", "6-13")
        );
        assert_eq!(a.date, "1970-03-15");
        assert_eq!(a.doi, "10.2307/abc");
        assert_eq!(a.issn, "0009-5281");
        assert_eq!(a.isbn, "");
        assert_eq!(a.tags, ["christology", "liberation"]);
        assert_eq!(a.creators.len(), 2);
        assert_eq!(a.creators[0].family, "Cone");
        assert_eq!(a.unmapped, ["N1"]);

        let b = &works[1];
        assert_eq!(b.kind, Some(Kind::Book));
        assert_eq!(b.isbn, "9780140449136");
        assert_eq!(
            (b.publisher.as_str(), b.place.as_str()),
            ("Geoffrey Bles", "London")
        );
        assert_eq!(b.date, "1937");
        assert!(b
            .creators
            .iter()
            .any(|c| c.role == CreatorRole::Translator && c.family == "Duddington"));
    }

    #[test]
    fn ris_without_records_is_an_error_and_a_missing_er_still_reads() {
        assert!(parse_ris("hello world").is_err());
        let works = parse_ris("TY  - BOOK\nTI  - No End Tag\n").unwrap();
        assert_eq!(works[0].title, "No End Tag");
    }

    #[test]
    fn ris_dates_in_their_odd_shapes() {
        assert_eq!(ris_date("1970"), "1970");
        assert_eq!(ris_date("1970///"), "1970");
        assert_eq!(ris_date("1970/03//"), "1970-03");
        assert_eq!(ris_date("1970/03/15/spring"), "1970-03-15");
        assert_eq!(ris_date("1970-03-15"), "1970-03-15");
        assert_eq!(ris_date("c. 1970"), "");
        assert_eq!(ris_date("1970/13/40/"), "1970");
    }

    const CSL: &str = r#"[
      {"id":"cone1970black","type":"article-journal","title":"Black Theology and Black Power",
       "author":[{"family":"Cone","given":"James H."}],
       "container-title":"Christianity and Crisis","volume":"30","issue":"1","page":"6-13",
       "issued":{"date-parts":[[1970,3]]},"DOI":"10.2307/abc","ISSN":"0009-5281",
       "keyword":"christology, liberation","abstract":"A first look."},
      {"id":"http://zotero.org/users/1/items/AB12CD34","type":"book","title":"The Destiny of Man",
       "author":[{"family":"Berdyaev","given":"Nikolai"}],"translator":[{"family":"Duddington","given":"Natalie"}],
       "publisher":"Geoffrey Bles","publisher-place":"London","issued":{"date-parts":[[1937]]},
       "ISBN":"0-14-044913-2","citation-key":"berdyaev1937destiny"},
      {"id":"who2020","type":"report","title":"A Report","author":[{"literal":"World Council of Churches"}],
       "issued":{"raw":"Spring 2020"}}
    ]"#;

    #[test]
    fn csl_json_items_are_read_and_better_bibtex_keys_kept() {
        let works = parse_csl_json(CSL).unwrap();
        assert_eq!(works.len(), 3);
        assert_eq!(works[0].key.as_deref(), Some("cone1970black"));
        assert_eq!(works[0].date, "1970-03");
        assert_eq!(works[0].tags, ["christology", "liberation"]);
        assert_eq!(works[0].unmapped, ["abstract"]);
        // `citation-key` wins over an opaque Zotero URL id.
        assert_eq!(works[1].key.as_deref(), Some("berdyaev1937destiny"));
        assert_eq!(works[1].isbn, "9780140449136");
        assert_eq!(works[1].creators.len(), 2);
        assert_eq!(works[2].key.as_deref(), Some("who2020"));
        assert_eq!(works[2].date, "2020");
        assert!(works[2].creators[0].single_field);
    }

    #[test]
    fn csl_json_accepts_a_single_item_or_an_items_wrapper_and_rejects_nonsense() {
        assert_eq!(
            parse_csl_json(r#"{"type":"book","title":"T"}"#)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            parse_csl_json(r#"{"items":[{"title":"A"},{"title":"B"}]}"#)
                .unwrap()
                .len(),
            2
        );
        assert!(parse_csl_json("not json").is_err());
        assert!(parse_csl_json("[]").is_err());
    }

    #[test]
    fn only_key_shaped_ids_are_kept_as_keys() {
        for good in ["cone1970black", "Cone:1970", "a-b_c.d"] {
            assert!(plausible_key(good), "{good}");
        }
        for bad in [
            "",
            "1970cone",
            "http://zotero.org/x",
            "a b",
            "a/../b",
            "../x",
        ] {
            assert!(!plausible_key(bad), "{bad}");
        }
    }
}
