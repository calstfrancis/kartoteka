//! Minimal WebDAV upload for backing up a library to a WebDAV server (Nextcloud, pCloud,
//! …). A simple one-way mirror — no history or conflict handling, unlike git.
//!
//! **Only Kartoteka's own library files are uploaded** — the canonical subdirectories and
//! files listed in `LIBRARY_DIRS`/`LIBRARY_FILES`. It never walks arbitrary contents of the
//! library folder, so pointing it at a folder that also holds unrelated files (Downloads,
//! home, …) will not upload those files.

use std::collections::HashSet;
use std::path::Path;

use reqwest::blocking::Client;
use reqwest::Method;

/// The library subdirectories that get backed up (their files, recursively).
const LIBRARY_DIRS: &[&str] = &["entries", "notes", "collections", "annots", "attachments"];
/// The top-level library files that get backed up.
const LIBRARY_FILES: &[&str] = &["library.yml", ".gitignore"];

/// Upload the library's own files under `base_url` (a collection on the WebDAV server).
/// Returns the number of files uploaded. Directories are created with MKCOL as needed.
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

    // Top-level files.
    for name in LIBRARY_FILES {
        let path = root.join(name);
        if path.is_file() {
            upload_file(&client, &base, root, &path, username, password, &mut made)?;
            uploaded += 1;
        }
    }

    // Canonical subdirectories (their files, recursively) — and nothing else.
    for dir_name in LIBRARY_DIRS {
        let dir = root.join(dir_name);
        if !dir.is_dir() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&dir) {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_type().is_file() {
                upload_file(
                    &client,
                    &base,
                    root,
                    entry.path(),
                    username,
                    password,
                    &mut made,
                )?;
                uploaded += 1;
            }
        }
    }

    Ok(uploaded)
}

#[allow(clippy::too_many_arguments)]
fn upload_file(
    client: &Client,
    base: &str,
    root: &Path,
    path: &Path,
    username: &str,
    password: &str,
    made: &mut HashSet<String>,
) -> Result<(), String> {
    let rel = path.strip_prefix(root).map_err(|e| e.to_string())?;

    // Create parent collections top-down.
    let mut cur = base.to_string();
    if let Some(parent) = rel.parent() {
        for comp in parent.components() {
            let seg = comp.as_os_str().to_string_lossy();
            if seg.is_empty() {
                continue;
            }
            cur = format!("{cur}/{}", encode_segment(&seg));
            if made.insert(cur.clone()) {
                mkcol(client, &cur, username, password)?;
            }
        }
    }

    let url = rel_url(base, rel);
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let resp = client
        .put(&url)
        .basic_auth(username, Some(password))
        .body(bytes)
        .send()
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("PUT {} → HTTP {}", rel.display(), resp.status()));
    }
    Ok(())
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
