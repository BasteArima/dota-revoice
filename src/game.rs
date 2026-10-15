//! Поиск установленной Доты и пака озвучки.
//!
//! Steam → `libraryfolders.vdf` → библиотека с `dota 2 beta`.
//! Папку языка выбирает `AudioLanguage` из `game/dota/cfg/boot.vcfg`
//! (с июльского патча 2026 года — именно он, а не язык интерфейса).

use std::path::{Path, PathBuf};
use crate::tr;

#[derive(Debug, Clone)]
pub struct DotaInstall {
    /// `.../dota 2 beta/game`
    pub game_dir: PathBuf,
    /// Какая озвучка включена в игре, например `russian`. `None` — английская.
    pub audio_language: Option<String>,
    /// Озвучку какого языка переделываем (выбор в настройках, по умолчанию — как в игре).
    /// `None` — английская: своей папки языка у неё нет.
    pub voice_language: Option<String>,
}

impl DotaInstall {
    /// Папка, где лежат паки озвучки и куда кладутся моды: `game/dota_russian`.
    pub fn language_dir(&self) -> Option<PathBuf> {
        self.voice_language.as_ref().map(|l| self.game_dir.join(format!("dota_{l}")))
    }

    /// В игре включена не та озвучка, для которой собираем мод.
    pub fn audio_mismatch(&self) -> bool {
        self.audio_language != self.voice_language
    }

    /// Языки озвучки, скачанные в Steam: папки `dota_<язык>` с паком.
    /// Steam создаёт папки всех языков, но пак кладёт только в выбранные.
    pub fn voice_languages(&self) -> Vec<String> {
        let mut langs: Vec<String> = std::fs::read_dir(&self.game_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_owned();
                let lang = name.strip_prefix("dota_")?;
                let dir = e.path();
                (dir.join("pak01_dir.vpk").is_file() || dir.join("pak01_dir.vpk.off").is_file()).then(|| lang.to_owned())
            })
            .collect();
        langs.sort();
        langs
    }

    /// Пак озвучки Valve: включённый `pak01_dir.vpk` или уже отключённый `.off`.
    pub fn voice_pak(&self) -> Option<VoicePak> {
        let dir = self.language_dir()?;
        let on = dir.join("pak01_dir.vpk");
        let off = dir.join("pak01_dir.vpk.off");
        if on.is_file() {
            Some(VoicePak { path: on, enabled: true })
        } else if off.is_file() {
            Some(VoicePak { path: off, enabled: false })
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct VoicePak {
    pub path: PathBuf,
    /// `true` — пак Valve включён и перекрывает наш `pak02`.
    pub enabled: bool,
}

/// Ищет Доту: в папке, которую указали в настройках, иначе — в библиотеках Steam.
/// `voice` — язык озвучки для мода из настроек; `None` — как в игре.
pub fn find_dota(dota_dir: Option<&Path>, voice: Option<&str>) -> Option<DotaInstall> {
    let game_dir = match dota_dir {
        Some(dir) => game_dir_of(dir)?,
        // По частям, а не строкой со слешами: путь показывается в настройках и должен быть ровным.
        None => steam_libraries().into_iter().find_map(|lib| game_dir_of(&lib.join("steamapps").join("common").join("dota 2 beta")))?,
    };
    let audio_language = read_audio_language(&game_dir);
    let voice_language = voice.map(str::to_owned).or_else(|| audio_language.clone());
    Some(DotaInstall { game_dir, audio_language, voice_language })
}

/// Папка `game` внутри выбранной: годится и `dota 2 beta`, и сама `game`.
pub fn game_dir_of(dir: &Path) -> Option<PathBuf> {
    [dir.join("game"), dir.to_path_buf()].into_iter().find(|g| g.join("dota").is_dir())
}

/// `russian` → «русская». Названия папок Valve, а не коды языков.
pub fn language_name(lang: Option<&str>) -> String {
    match lang {
        None | Some("english") => tr!("английская", "English"),
        Some("russian") => tr!("русская", "Russian"),
        Some("schinese") => tr!("китайская", "Chinese"),
        Some("koreana") => tr!("корейская", "Korean"),
        Some("japanese") => tr!("японская", "Japanese"),
        Some(other) => return other.to_owned(),
    }
    .to_owned()
}

fn read_audio_language(game_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(game_dir.join("dota/cfg/boot.vcfg")).ok()?;
    let lang = vdf_value(&text, "AudioLanguage")?;
    (!lang.eq_ignore_ascii_case("english")).then(|| lang.to_ascii_lowercase())
}

/// Значение первого ключа `"key" "value"` в тексте KeyValues.
fn vdf_value(text: &str, key: &str) -> Option<String> {
    vdf_values(text, key).into_iter().next()
}

fn vdf_values(text: &str, key: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split('"').skip(1).step_by(2);
            (parts.next()? == key).then(|| parts.next().map(|v| v.replace("\\\\", "\\")))?
        })
        .collect()
}

fn steam_libraries() -> Vec<PathBuf> {
    let Some(steam) = steam_root() else {
        return Vec::new();
    };
    let vdf = std::fs::read_to_string(steam.join("steamapps/libraryfolders.vdf")).unwrap_or_default();
    let mut libs: Vec<PathBuf> = vdf_values(&vdf, "path").into_iter().map(PathBuf::from).collect();
    if !libs.iter().any(|l| same_path(l, &steam)) {
        libs.insert(0, steam);
    }
    libs
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(windows)]
fn steam_root() -> Option<PathBuf> {
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Valve\Steam").ok()?;
    let path: String = key.get_value("SteamPath").ok()?;
    Some(PathBuf::from(path))
}

#[cfg(target_os = "macos")]
fn steam_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/Steam"))
}

/// Linux: обычный Steam, его символическая ссылка и Steam из Flatpak.
#[cfg(all(not(windows), not(target_os = "macos")))]
fn steam_root() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    [".steam/steam", ".local/share/Steam", ".var/app/com.valvesoftware.Steam/.local/share/Steam"]
        .into_iter()
        .map(|p| home.join(p))
        .find(|p| p.join("steamapps").is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_library_paths_and_language() {
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\Games\\\\Steam\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"K:\\\\SteamLibrary\"\n\t}\n}";
        assert_eq!(vdf_values(vdf, "path"), ["D:\\Games\\Steam", "K:\\SteamLibrary"]);

        let boot = "\"boot\"\n{\n\t\"UILanguage\"\t\t\"russian\"\n\t\"AudioLanguage\"\t\t\"russian\"\n}";
        assert_eq!(vdf_value(boot, "AudioLanguage").as_deref(), Some("russian"));
    }

    #[test]
    fn finds_game_in_chosen_folder_and_voice_languages() {
        let root = std::env::temp_dir().join(format!("dota_revoice_game_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let game = root.join("dota 2 beta/game");
        std::fs::create_dir_all(game.join("dota/cfg")).unwrap();
        std::fs::write(game.join("dota/cfg/boot.vcfg"), "\"boot\"\n{\n\t\"AudioLanguage\"\t\t\"russian\"\n}").unwrap();
        // Скачанная озвучка — папка с паком (в том числе отключённым); пустые папки Steam не в счёт.
        for (lang, pak) in [("russian", Some("pak01_dir.vpk.off")), ("schinese", Some("pak01_dir.vpk")), ("koreana", None)] {
            std::fs::create_dir_all(game.join(format!("dota_{lang}"))).unwrap();
            if let Some(pak) = pak {
                std::fs::write(game.join(format!("dota_{lang}/{pak}")), b"x").unwrap();
            }
        }

        for picked in [root.join("dota 2 beta"), game.clone()] {
            let dota = find_dota(Some(&picked), None).unwrap();
            assert_eq!(dota.game_dir, game);
            assert_eq!(dota.voice_language.as_deref(), Some("russian"));
            assert!(!dota.audio_mismatch());
            assert_eq!(dota.voice_languages(), ["russian", "schinese"]);
        }
        let other = find_dota(Some(&game), Some("schinese")).unwrap();
        assert_eq!(other.language_dir().unwrap(), game.join("dota_schinese"));
        assert!(other.audio_mismatch());
        assert!(find_dota(Some(&root), None).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
