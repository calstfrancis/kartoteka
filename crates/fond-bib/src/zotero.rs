//! Read-only reader for a Zotero `zotero.sqlite` store. Extracts the item metadata needed
//! to match items against an imported `.bib` (title, DOI, year), plus collections and
//! notes. Purely a reader — it never writes to the Zotero database.
//!
//! The BetterBibTeX *citation* key is not in these tables, so Kartoteka links Zotero items
//! to citation keys by matching DOI (then title+year) against the imported library; see
//! `import.rs`.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::error::BibError;

/// A bibliographic Zotero item (notes/attachments/annotations excluded).
#[derive(Debug, Clone)]
pub struct ZoteroItem {
    pub item_id: i64,
    pub zotero_key: String,
    pub item_type: String,
    pub title: Option<String>,
    pub doi: Option<String>,
    pub year: Option<i32>,
}

/// A Zotero collection and its direct member item ids (in Zotero's order).
#[derive(Debug, Clone)]
pub struct ZoteroCollection {
    pub name: String,
    pub parent_name: Option<String>,
    pub item_ids: Vec<i64>,
}

/// A Zotero note. `parent_item_id` is the item it is attached to, or `None` if standalone.
#[derive(Debug, Clone)]
pub struct ZoteroNote {
    pub parent_item_id: Option<i64>,
    pub html: String,
}

/// Everything read from a Zotero store in one pass.
#[derive(Debug, Default)]
pub struct ZoteroData {
    pub items: Vec<ZoteroItem>,
    pub collections: Vec<ZoteroCollection>,
    pub notes: Vec<ZoteroNote>,
}

fn err(path: &Path, e: impl std::fmt::Display) -> BibError {
    BibError::Zotero {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

/// Open and read a Zotero database read-only.
pub fn read(db_path: &Path) -> Result<ZoteroData, BibError> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| err(db_path, e))?;

    let titles = field_map(&conn, "title").map_err(|e| err(db_path, e))?;
    let dois = field_map(&conn, "DOI").map_err(|e| err(db_path, e))?;
    let dates = field_map(&conn, "date").map_err(|e| err(db_path, e))?;

    let items = read_items(&conn, &titles, &dois, &dates).map_err(|e| err(db_path, e))?;
    let collections = read_collections(&conn).map_err(|e| err(db_path, e))?;
    let notes = read_notes(&conn).map_err(|e| err(db_path, e))?;

    Ok(ZoteroData {
        items,
        collections,
        notes,
    })
}

/// Map itemID → value for a named Zotero field.
fn field_map(conn: &Connection, field: &str) -> rusqlite::Result<HashMap<i64, String>> {
    let mut stmt = conn.prepare(
        "SELECT d.itemID, v.value \
         FROM itemData d \
         JOIN fields f ON f.fieldID = d.fieldID \
         JOIN itemDataValues v ON v.valueID = d.valueID \
         WHERE f.fieldName = ?1",
    )?;
    let rows = stmt.query_map([field], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (id, value) = row?;
        map.insert(id, value);
    }
    Ok(map)
}

fn read_items(
    conn: &Connection,
    titles: &HashMap<i64, String>,
    dois: &HashMap<i64, String>,
    dates: &HashMap<i64, String>,
) -> rusqlite::Result<Vec<ZoteroItem>> {
    let mut stmt = conn.prepare(
        "SELECT i.itemID, i.key, t.typeName \
         FROM items i \
         JOIN itemTypes t ON t.itemTypeID = i.itemTypeID \
         WHERE i.itemID NOT IN (SELECT itemID FROM deletedItems) \
           AND t.typeName NOT IN ('attachment', 'note', 'annotation')",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;

    let mut items = Vec::new();
    for row in rows {
        let (item_id, zotero_key, item_type) = row?;
        items.push(ZoteroItem {
            item_id,
            zotero_key,
            item_type,
            title: titles.get(&item_id).cloned(),
            doi: dois.get(&item_id).cloned(),
            year: dates.get(&item_id).and_then(|d| year_from_zotero_date(d)),
        });
    }
    Ok(items)
}

fn read_collections(conn: &Connection) -> rusqlite::Result<Vec<ZoteroCollection>> {
    // collectionID -> (name, parentID)
    let mut stmt =
        conn.prepare("SELECT collectionID, collectionName, parentCollectionID FROM collections")?;
    let mut names: HashMap<i64, (String, Option<i64>)> = HashMap::new();
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<i64>>(2)?,
        ))
    })?;
    for row in rows {
        let (id, name, parent) = row?;
        names.insert(id, (name, parent));
    }

    // membership in order
    let mut stmt = conn.prepare(
        "SELECT collectionID, itemID FROM collectionItems ORDER BY collectionID, orderIndex",
    )?;
    let mut members: HashMap<i64, Vec<i64>> = HashMap::new();
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    for row in rows {
        let (cid, iid) = row?;
        members.entry(cid).or_default().push(iid);
    }

    let mut collections = Vec::new();
    for (id, (name, parent)) in &names {
        let parent_name = parent.and_then(|p| names.get(&p).map(|(n, _)| n.clone()));
        collections.push(ZoteroCollection {
            name: name.clone(),
            parent_name,
            item_ids: members.get(id).cloned().unwrap_or_default(),
        });
    }
    collections.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(collections)
}

fn read_notes(conn: &Connection) -> rusqlite::Result<Vec<ZoteroNote>> {
    let mut stmt = conn.prepare(
        "SELECT n.parentItemID, n.note \
         FROM itemNotes n \
         WHERE n.itemID NOT IN (SELECT itemID FROM deletedItems)",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut notes = Vec::new();
    for row in rows {
        let (parent_item_id, html) = row?;
        notes.push(ZoteroNote {
            parent_item_id,
            html,
        });
    }
    Ok(notes)
}

/// Extract a 4-digit year from a Zotero date value (stored like `"1970-00-00 1970"` or
/// `"2020-05-01 May 2020"`).
fn year_from_zotero_date(date: &str) -> Option<i32> {
    let bytes = date.as_bytes();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if bytes[i..i + 4].iter().all(|b| b.is_ascii_digit()) {
            // Guard against a longer digit run (don't take the first 4 of 5+ digits).
            let not_longer = i + 4 == bytes.len() || !bytes[i + 4].is_ascii_digit();
            let not_preceded = i == 0 || !bytes[i - 1].is_ascii_digit();
            if not_longer && not_preceded {
                return date[i..i + 4].parse().ok();
            }
        }
        i += 1;
    }
    None
}

/// Very small HTML → Markdown conversion for Zotero note bodies. Handles the tags Zotero
/// actually emits (paragraphs, line breaks, bold/italic, headings, lists) and unescapes
/// entities; unknown tags are stripped. Intentionally dependency-free and lossy-but-legible
/// — the goal is readable note prose, not a faithful HTML renderer.
pub fn html_to_markdown(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut chars = html.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '<' {
            out.push(c);
            continue;
        }
        // Read the tag.
        let mut tag = String::new();
        for tc in chars.by_ref() {
            if tc == '>' {
                break;
            }
            tag.push(tc);
        }
        let name = tag
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let closing = tag.starts_with('/');
        match name.as_str() {
            "p" | "div" => {
                if closing {
                    out.push_str("\n\n");
                }
            }
            "br" => out.push('\n'),
            "b" | "strong" => out.push_str("**"),
            "i" | "em" => out.push('*'),
            "li" => {
                if !closing {
                    out.push_str("- ");
                } else {
                    out.push('\n');
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if !closing {
                    let level = name[1..].parse::<usize>().unwrap_or(1);
                    out.push('\n');
                    for _ in 0..level {
                        out.push('#');
                    }
                    out.push(' ');
                } else {
                    out.push('\n');
                }
            }
            _ => {} // strip other tags
        }
    }

    let unescaped = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'");

    // Collapse runs of 3+ newlines to a clean paragraph break, trim edges.
    let mut collapsed = String::with_capacity(unescaped.len());
    let mut newlines = 0;
    for ch in unescaped.chars() {
        if ch == '\n' {
            newlines += 1;
            if newlines <= 2 {
                collapsed.push('\n');
            }
        } else {
            newlines = 0;
            collapsed.push(ch);
        }
    }
    collapsed.trim().to_string()
}

/// Turn a collection name into a filesystem-safe slug for `collections/<slug>.yml`.
pub fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "collection".to_string()
    } else {
        slug
    }
}

pub(crate) fn normalize_doi(doi: &str) -> String {
    doi.trim()
        .to_ascii_lowercase()
        .trim_start_matches("https://doi.org/")
        .trim_start_matches("http://doi.org/")
        .trim_start_matches("http://dx.doi.org/")
        .trim_start_matches("doi:")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_year_from_zotero_dates() {
        assert_eq!(year_from_zotero_date("1970-00-00 1970"), Some(1970));
        assert_eq!(year_from_zotero_date("2020-05-01 May 2020"), Some(2020));
        assert_eq!(year_from_zotero_date(""), None);
        assert_eq!(year_from_zotero_date("no date here"), None);
    }

    #[test]
    fn html_to_markdown_basics() {
        let html = "<p>First <strong>bold</strong> and <em>italic</em>.</p><p>Second.</p>";
        let md = html_to_markdown(html);
        assert_eq!(md, "First **bold** and *italic*.\n\nSecond.");
    }

    #[test]
    fn html_lists_and_entities() {
        let html = "<ul><li>one</li><li>two &amp; three</li></ul>";
        let md = html_to_markdown(html);
        assert!(md.contains("- one"));
        assert!(md.contains("- two & three"));
    }

    #[test]
    fn slugs_are_filesystem_safe() {
        assert_eq!(slugify("Liberation Theology"), "liberation-theology");
        assert_eq!(slugify("  Café / Notes!! "), "caf-notes");
        assert_eq!(slugify("***"), "collection");
    }

    #[test]
    fn doi_normalization() {
        assert_eq!(normalize_doi("https://doi.org/10.1/ABC"), "10.1/abc");
        assert_eq!(normalize_doi("  doi:10.2/x "), "10.2/x");
    }
}
