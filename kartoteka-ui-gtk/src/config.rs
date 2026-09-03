//! Persisted GUI configuration (the last-opened library path). Stored per Fond convention
//! under `~/.config/kartoteka/`.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// The library directory last opened, restored on launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_path: Option<PathBuf>,
    /// Colour scheme: "system" (default), "light", or "dark".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// WebDAV backup target (base collection URL), e.g. `https://host/remote.php/dav/…`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webdav_url: Option<String>,
    /// WebDAV username (the password is kept in the system keyring).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webdav_username: Option<String>,
    /// Whether automatic periodic backups are on. Off by default — an explicit opt-in.
    #[serde(default)]
    pub auto_backup_enabled: bool,
    /// Minutes between automatic backups, while a library is open.
    #[serde(default = "default_auto_backup_interval")]
    pub auto_backup_interval_mins: u32,
    /// Window size/maximized state, restored on launch — the "internal window sizing" the
    /// UI standard calls for remembering across sessions, alongside the pane positions
    /// below. Unset (a first run) falls back to the window's own built-in defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_width: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_height: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_maximized: Option<bool>,
    /// Collections-sidebar/rest split (the outer `Paned`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collections_pane_position: Option<i32>,
    /// Entries-spreadsheet/detail-pane split (the inner `Paned`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_pane_position: Option<i32>,
    /// Optional-column visibility in the entries spreadsheet, keyed by column id
    /// ("tags", "status", or "custom:<field name>"). Absent means the built-in default —
    /// off for every optional column, so a freshly-defined custom field doesn't clutter the
    /// sheet until asked for. The always-on columns (key/title/author/year/files) aren't
    /// stored here; they can't be hidden.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub column_visible: HashMap<String, bool>,
    /// Entries-spreadsheet column display order, by id, left-to-right. Restored on next
    /// launch — GTK4's native drag-to-reorder (`ColumnView::set_reorderable`) doesn't persist
    /// on its own. Missing/unknown ids are appended in their built-in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub column_order: Vec<String>,
    /// Whether the entries pane is showing the Bookshelf cover grid instead of the
    /// spreadsheet, restored on next launch.
    #[serde(default)]
    pub bookshelf_view: bool,
}

fn default_auto_backup_interval() -> u32 {
    30
}

fn config_dir() -> PathBuf {
    glib::user_config_dir().join("kartoteka")
}

fn config_file() -> PathBuf {
    config_dir().join("gui.json")
}

impl Config {
    pub fn load() -> Config {
        match fs::read_to_string(config_file()) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self) {
        let _ = fs::create_dir_all(config_dir());
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = fs::write(config_file(), text);
        }
    }
}
