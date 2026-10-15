//! Настройки программы между запусками: папки, язык озвучки, микрофон, автообрезка.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Мастер первого запуска пройден (или настройки перенесены со старой версии).
    pub setup_done: bool,
    pub last_folder: Option<PathBuf>,
    /// Папка `dota 2 beta`, если Дота стоит не там, где её находит Steam.
    pub dota_dir: Option<PathBuf>,
    /// Озвучку какого языка переделываем (`russian`); `None` — ту, что включена в игре.
    pub voice_language: Option<String>,
    /// Рабочая папка: по умолчанию в ней лежат мои фразы и собранный пак.
    pub library: Option<PathBuf>,
    /// Своя папка для моих фраз вместо `<рабочая папка>/export`.
    pub export_dir: Option<PathBuf>,
    /// Своя папка для собранного пака вместо `<мои фразы>/build`.
    pub build_dir: Option<PathBuf>,
    pub ui_language: UiLanguage,
    pub check_updates: bool,
    pub input_device: Option<String>,
    /// Сразу после записи или импорта обрезать тишину по краям.
    pub auto_trim: bool,
    /// Сразу после записи подгонять громкость под оригинал.
    pub auto_level: bool,
    /// Включать шумодав для новых записей.
    pub denoise_new: bool,
    pub pre_roll: PreRoll,
}

/// Язык интерфейса.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiLanguage {
    Russian,
    English,
}

/// Что происходит между нажатием «Запись» и началом записи.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreRoll {
    /// Запись начинается сразу.
    Off,
    /// Три сигнала с интервалом в секунду.
    Countdown,
    /// Сначала звучит оригинал, запись — сразу после него.
    Original,
}

impl PreRoll {
    pub fn label(self) -> &'static str {
        match self {
            PreRoll::Off => "сразу",
            PreRoll::Countdown => "после отсчёта 3-2-1",
            PreRoll::Original => "после прослушивания оригинала",
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            setup_done: false,
            last_folder: None,
            dota_dir: None,
            voice_language: None,
            library: None,
            export_dir: None,
            build_dir: None,
            ui_language: UiLanguage::Russian,
            check_updates: true,
            input_device: None,
            auto_trim: true,
            auto_level: true,
            denoise_new: false,
            pre_roll: PreRoll::Off,
        }
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

    pub fn library_dir(&self) -> PathBuf {
        self.library.clone().unwrap_or_else(default_library)
    }

    /// Папка «Мои фразы»: свои mp3, исходники, дубли, корзина.
    pub fn export_root(&self) -> PathBuf {
        self.export_dir.clone().unwrap_or_else(|| self.library_dir().join("export"))
    }

    /// Куда собирается `pak02_dir.vpk`.
    pub fn build_root(&self) -> PathBuf {
        self.build_dir.clone().unwrap_or_else(|| self.export_root().join("build"))
    }
}

/// Рабочая папка по умолчанию. На Windows программа переносная — папка рядом с exe,
/// если туда можно писать (не Program Files). На macOS `.app` запускается из места,
/// куда писать нельзя, поэтому там и на Linux — «Документы».
pub fn default_library() -> PathBuf {
    #[cfg(windows)]
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf))
        && writable(&dir)
    {
        return dir.join("library");
    }
    let docs = dirs::document_dir().or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from("."));
    docs.join("Dota Revoice")
}

/// Можно ли создать файл в папке.
pub fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".dota_revoice_probe_{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}
