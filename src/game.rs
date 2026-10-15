//! Поиск установленной Доты и пака озвучки.
//!
//! Steam → `libraryfolders.vdf` → библиотека с `dota 2 beta`.
//! Папку языка выбирает `AudioLanguage` из `game/dota/cfg/boot.vcfg`
//! (с июльского патча 2026 года — именно он, а не язык интерфейса).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DotaInstall {
    /// `.../dota 2 beta/game`
    pub game_dir: PathBuf,
    /// Например, `russian`. `None` — английская озвучка, папки языка нет.
    pub audio_language: Option<String>,
}

impl DotaInstall {
    /// Папка, где лежат паки озвучки и куда кладутся моды: `game/dota_russian`.
    pub fn language_dir(&self) -> Option<PathBuf> {
        self.audio_language.as_ref().map(|l| self.game_dir.join(format!("dota_{l}")))
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

pub fn find_dota() -> Option<DotaInstall> {
    steam_libraries().into_iter().find_map(|lib| {
        let game_dir = lib.join("steamapps/common/dota 2 beta/game");
        game_dir.join("dota").is_dir().then(|| DotaInstall {
            audio_language: read_audio_language(&game_dir),
            game_dir,
        })
    })
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
}
