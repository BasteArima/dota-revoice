//! Настройки программы между запусками: последняя папка, микрофон, автообрезка.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub last_folder: Option<PathBuf>,
    pub input_device: Option<String>,
    /// Сразу после записи или импорта обрезать тишину по краям.
    pub auto_trim: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { last_folder: None, input_device: None, auto_trim: true }
    }
}

fn path() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"));
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"));
    base.map(|b| b.join("dota_revoice").join("config.json"))
}

impl Config {
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(p, json);
        }
    }
}
