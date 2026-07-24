//! Minimal WebDAV upload for backing up a library to a WebDAV server (Nextcloud, pCloud,
//! …). A simple one-way mirror — no history or conflict handling, unlike git. Uploads the
//! working files (records, notes, collections, annotations, and attachment blobs),
//! skipping the derived `.kartoteka/` and git's `.git/`.

use std::collections::HashSet;
use std::path::Path;

use reqwest::blocking::Client;
use reqwest::Method;

/// Upload the whole library under `base_url` (a collection on the WebDAV server). Returns
/// the number of files uploaded. Directories are created with MKCOL as needed.
pub fn upload_library(
    base_url: &str,
    username: &str,
    password: &str,
    root: &Path,
) -> Result<usize, String> {
    let base = base_url.trim_end_matches('/').to_string();
    let client = Client::builder().build().map_err(|e| e.to_string())?;

    mkcol(&client, &base, username, password)?;

    let mut uploaded = 0;
    let mut made: HashSet<String> = HashSet::new();

    let walker = walkdir::WalkDir::new(root).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        !(e.file_type().is_dir() && (name == ".kartoteka" || name == ".git"))
    });

    for entry in walker {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(root).map_err(|e| e.to_string())?;

        // Create parent collections top-down.
        let mut cur = base.clone();
        if let Some(parent) = rel.parent() {
            for comp in parent.components() {
                let seg = comp.as_os_str().to_string_lossy();
                if seg.is_empty() {
                    continue;
                }
                cur = format!("{cur}/{}", encode_segment(&seg));
                if made.insert(cur.clone()) {
                    mkcol(&client, &cur, username, password)?;
                }
            }
        }

        let url = rel_url(&base, rel);
        let bytes = std::fs::read(entry.path()).map_err(|e| e.to_string())?;
        let resp = client
            .put(&url)
            .basic_auth(username, Some(password))
            .body(bytes)
            .send()
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("PUT {} → HTTP {}", rel.display(), resp.status()));
        }
        uploaded += 1;
    }

    Ok(uploaded)
}

fn mkcol(client: &Client, url: &str, username: &str, password: &str) -> Result<(), String> {
    let method = Method::from_bytes(b"MKCOL").map_err(|e| e.to_string())?;
    let resp = client
        .request(method, url)
        .basic_auth(username, Some(password))
        .send()
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    // 201 created; 405/301/409 commonly mean "already exists" — tolerate.
    if resp.status().is_success() || matches!(status, 301 | 405 | 409) {
        Ok(())
    } else {
        Err(format!("MKCOL {url} → HTTP {status}"))
    }
}

fn rel_url(base: &str, rel: &Path) -> String {
    let mut url = base.to_string();
    for comp in rel.components() {
        let seg = comp.as_os_str().to_string_lossy();
        if !seg.is_empty() {
            url.push('/');
            url.push_str(&encode_segment(&seg));
        }
    }
    url
}

/// Percent-encode the characters that matter in a path segment. Library filenames are
/// citation keys / hashes / slugs (URL-safe), so this only needs to cover the odd space or
/// reserved character.
fn encode_segment(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for b in seg.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
