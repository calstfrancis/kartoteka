//! Persisted GUI configuration (the last-opened library path). Stored per Fond convention
//! under `~/.config/kartoteka/`.

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
