//! Установка собранного пака в игру, выключение и откат.
//!
//! Установка: положить `pak02_dir.vpk` в папку языка и отключить пак озвучки
//! Valve (`pak01_dir.vpk` → `pak01_dir.vpk.off`), иначе он перекрывает наш.
//! Выключение: наш пак → `pak02_dir.vpk.off`, пак Valve — обратно; включение — наоборот,
//! без копирования. Откат: убрать наш пак совсем и вернуть `pak01_dir.vpk` на место.
//! Тома `pak01_00N.vpk` и чужие моды не трогаем.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};

use crate::game::DotaInstall;

pub const PAK_NAME: &str = "pak02_dir.vpk";
/// Наш пак, выключенный переключателем: лежит в игре, но не грузится.
pub const PAK_OFF: &str = "pak02_dir.vpk.off";
const VALVE_ON: &str = "pak01_dir.vpk";
const VALVE_OFF: &str = "pak01_dir.vpk.off";

/// Наш пак в папке языка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModState {
    Absent,
    Enabled,
    /// Выключен: игра звучит озвучкой Valve, включается обратно мгновенно.
    Disabled,
}

pub fn mod_state(dota: &DotaInstall) -> ModState {
    match dota.language_dir() {
        Some(d) if d.join(PAK_NAME).is_file() => ModState::Enabled,
        Some(d) if d.join(PAK_OFF).is_file() => ModState::Disabled,
        _ => ModState::Absent,
    }
}

/// Сам файл нашего пака в игре — включённый или выключенный.
pub fn installed_pak(dota: &DotaInstall) -> Option<PathBuf> {
    let dir = dota.language_dir()?;
    [dir.join(PAK_NAME), dir.join(PAK_OFF)].into_iter().find(|p| p.is_file())
}

/// Что именно изменится в папке игры — показываем пользователю до подтверждения.
#[derive(Debug, Clone)]
pub struct Plan {
    pub language_dir: PathBuf,
    /// В папке уже есть `pak02_dir.vpk` — он будет заменён.
    pub replaces_existing: bool,
    /// Пак Valve включён и будет переименован в `.off`.
    pub disables_valve_pak: bool,
}

impl Plan {
    pub fn for_install(dota: &DotaInstall) -> Result<Self> {
        let language_dir = dota.language_dir().context(
            "у Доты английская озвучка — моды кладутся в папку языка озвучки (например, dota_russian)",
        )?;
        ensure!(language_dir.is_dir(), "нет папки {}", language_dir.display());
        Ok(Self {
            replaces_existing: language_dir.join(PAK_NAME).is_file() || language_dir.join(PAK_OFF).is_file(),
            disables_valve_pak: language_dir.join(VALVE_ON).is_file(),
            language_dir,
        })
    }

    pub fn describe(&self) -> Vec<String> {
        let mut steps = vec![format!(
            "{} {PAK_NAME} в {}",
            if self.replaces_existing { "Заменить" } else { "Положить" },
            self.language_dir.display()
        )];
        if self.disables_valve_pak {
            steps.push("Переименовать pak01_dir.vpk в pak01_dir.vpk.off (отключить озвучку Valve)".into());
        }
        steps
    }
}

pub fn install(plan: &Plan, built_pak: &Path) -> Result<()> {
    ensure!(!is_dota_running(), "Дота запущена — закрой её, она держит паки открытыми");
    ensure!(built_pak.is_file(), "сначала собери пак: нет {}", built_pak.display());

    // Сначала копируем во временный файл рядом, потом подменяем одним переименованием,
    // чтобы при ошибке посреди копирования в игре не остался обрезанный пак.
    let target = plan.language_dir.join(PAK_NAME);
    let tmp = plan.language_dir.join(format!("{PAK_NAME}.tmp"));
    std::fs::copy(built_pak, &tmp).with_context(|| format!("не скопировался пак в {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("не заменился {} — файл занят?", target.display()));
    }
    // Выключенная старая версия больше не нужна — установили свежую.
    let old = plan.language_dir.join(PAK_OFF);
    if old.is_file() {
        std::fs::remove_file(&old).with_context(|| format!("не удаляется старый {}", old.display()))?;
    }

    if plan.disables_valve_pak {
        disable_valve(&plan.language_dir)?;
    }
    Ok(())
}

/// `pak01_dir.vpk` → `.off`. Старый `.off` от прошлой установки (Steam вернул pak01) лишний.
fn disable_valve(dir: &Path) -> Result<()> {
    let (on, off) = (dir.join(VALVE_ON), dir.join(VALVE_OFF));
    if !on.is_file() {
        return Ok(());
    }
    if off.exists() {
        std::fs::remove_file(&off).with_context(|| format!("не удаляется старый {}", off.display()))?;
    }
    std::fs::rename(&on, &off).with_context(|| format!("не переименовался {} — закрой Доту и Source 2 Viewer", on.display()))
}

/// `pak01_dir.vpk.off` → обратно. Если Steam уже вернул pak01, `.off` лишний.
fn enable_valve(dir: &Path, done: &mut Vec<String>) -> Result<()> {
    let (on, off) = (dir.join(VALVE_ON), dir.join(VALVE_OFF));
    if !off.is_file() {
        return Ok(());
    }
    if on.exists() {
        std::fs::remove_file(&off).with_context(|| format!("не удаляется {}", off.display()))?;
        done.push("pak01_dir.vpk уже на месте, лишний .off удалён".into());
    } else {
        std::fs::rename(&off, &on).with_context(|| format!("не переименовался {}", off.display()))?;
        done.push("pak01_dir.vpk включён обратно".into());
    }
    Ok(())
}

/// Включает или выключает установленный пак переименованием — без копирования.
/// Возвращает список сделанного.
pub fn set_enabled(dota: &DotaInstall, enabled: bool) -> Result<Vec<String>> {
    ensure!(!is_dota_running(), "Дота запущена — закрой её, она держит паки открытыми");
    let dir = dota.language_dir().context("у Доты нет папки языка озвучки")?;
    let (on, off) = (dir.join(PAK_NAME), dir.join(PAK_OFF));
    let mut done = Vec::new();
    if enabled {
        ensure!(on.is_file() || off.is_file(), "в {} нет нашего пака — установи его", dir.display());
        if off.is_file() {
            std::fs::rename(&off, &on).with_context(|| format!("не переименовался {}", off.display()))?;
            done.push(format!("{PAK_OFF} → {PAK_NAME}"));
        }
        if dir.join(VALVE_ON).is_file() {
            disable_valve(&dir)?;
            done.push(format!("{VALVE_ON} → {VALVE_OFF}"));
        }
    } else {
        ensure!(on.is_file() || off.is_file(), "в {} нет нашего пака", dir.display());
        if on.is_file() {
            if off.exists() {
                std::fs::remove_file(&off).with_context(|| format!("не удаляется {}", off.display()))?;
            }
            std::fs::rename(&on, &off).with_context(|| format!("не переименовался {}", on.display()))?;
            done.push(format!("{PAK_NAME} → {PAK_OFF}"));
        }
        enable_valve(&dir, &mut done)?;
    }
    Ok(done)
}

/// Убирает наш пак (включённый или выключенный) и возвращает озвучку Valve.
/// Возвращает список сделанного.
pub fn rollback(dota: &DotaInstall) -> Result<Vec<String>> {
    ensure!(!is_dota_running(), "Дота запущена — закрой её, она держит паки открытыми");
    let dir = dota.language_dir().context("у Доты нет папки языка озвучки")?;
    let mut done = Vec::new();

    for name in [PAK_NAME, PAK_OFF] {
        let pak = dir.join(name);
        if pak.is_file() {
            std::fs::remove_file(&pak).with_context(|| format!("не удаляется {}", pak.display()))?;
            done.push(format!("Удалён {name}"));
        }
    }
    enable_valve(&dir, &mut done)?;
    if done.is_empty() {
        bail!("в {} нечего откатывать", dir.display());
    }
    Ok(done)
}

/// Что не так с игрой с точки зрения мода — показываем предупреждением.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    /// Наш пак лежит, но Steam вернул `pak01_dir.vpk` (обновление или проверка файлов) —
    /// озвучка Valve снова перекрывает наши фразы.
    ValveRestored,
    /// В Доте английская озвучка, а другой не выбрано: папки языка нет, моды оттуда не грузятся.
    EnglishAudio,
    /// В игре включена одна озвучка, а мод собирается для другой — его не будет слышно.
    OtherAudio,
}

pub fn check(dota: &DotaInstall) -> Option<Alert> {
    let Some(dir) = dota.language_dir() else { return Some(Alert::EnglishAudio) };
    if dota.audio_mismatch() {
        return Some(Alert::OtherAudio);
    }
    let ours = dir.join(PAK_NAME).is_file();
    let valve_on = dota.voice_pak().is_some_and(|v| v.enabled);
    (ours && valve_on).then_some(Alert::ValveRestored)
}

/// Время изменения пака озвучки Valve: Steam ставит новое при каждом обновлении,
/// а наше переименование в `.off` его сохраняет. Новее собранного пака — пора пересобрать.
pub fn valve_pak_modified(dota: &DotaInstall) -> Option<std::time::SystemTime> {
    std::fs::metadata(dota.voice_pak()?.path).ok()?.modified().ok()
}

/// Установлено ли что-то нашего: есть `pak02` (в том числе выключенный) или отключённый пак Valve.
pub fn has_changes(dota: &DotaInstall) -> bool {
    dota.language_dir().is_some_and(|d| [PAK_NAME, PAK_OFF, VALVE_OFF].iter().any(|n| d.join(n).is_file()))
}

/// Запускает Доту через Steam — так же, как ярлык игры: Steam проверит
/// обновления и применит параметры запуска пользователя.
pub fn launch_dota() -> Result<()> {
    const URL: &str = "steam://rungameid/570";
    #[cfg(windows)]
    let status = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("cmd").args(["/C", "start", "", URL]).creation_flags(CREATE_NO_WINDOW).status()
    };
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("open").arg(URL).status();
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let status = std::process::Command::new("xdg-open").arg(URL).status();
    ensure!(status.context("Steam не запустился")?.success(), "Steam не открыл Доту — он установлен?");
    Ok(())
}

pub fn is_dota_running() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq dota2.exe", "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).to_ascii_lowercase().contains("dota2.exe"))
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("pgrep").args(["-x", "dota2"]).output().is_ok_and(|o| o.status.success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_dota(name: &str, with_valve_pak: bool) -> (DotaInstall, PathBuf) {
        let root = std::env::temp_dir().join(format!("dota_revoice_install_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let lang = root.join("game/dota_russian");
        std::fs::create_dir_all(&lang).unwrap();
        if with_valve_pak {
            std::fs::write(lang.join("pak01_dir.vpk"), b"valve").unwrap();
        }
        std::fs::write(lang.join("pak01_000.vpk"), b"volume").unwrap();
        let russian = Some("russian".to_owned());
        (DotaInstall { game_dir: root.join("game"), audio_language: russian.clone(), voice_language: russian }, root)
    }

    #[test]
    fn disable_and_enable_without_copying() {
        if is_dota_running() {
            return;
        }
        let (dota, root) = fake_dota("toggle", true);
        let lang = dota.language_dir().unwrap();
        let built = root.join("built.vpk");
        std::fs::write(&built, b"our pak").unwrap();
        assert_eq!(mod_state(&dota), ModState::Absent);
        assert!(set_enabled(&dota, true).is_err(), "включать нечего");

        install(&Plan::for_install(&dota).unwrap(), &built).unwrap();
        assert_eq!(mod_state(&dota), ModState::Enabled);

        // Выключили: наш пак лежит как .off, озвучка Valve вернулась.
        set_enabled(&dota, false).unwrap();
        assert_eq!(mod_state(&dota), ModState::Disabled);
        assert_eq!(std::fs::read(lang.join(PAK_OFF)).unwrap(), b"our pak");
        assert_eq!(std::fs::read(lang.join(VALVE_ON)).unwrap(), b"valve");
        assert!(!lang.join(VALVE_OFF).exists());
        assert_eq!(check(&dota), None, "выключенный мод — не повод для тревоги");
        assert_eq!(installed_pak(&dota).unwrap(), lang.join(PAK_OFF));

        // Включили обратно.
        set_enabled(&dota, true).unwrap();
        assert_eq!(mod_state(&dota), ModState::Enabled);
        assert!(lang.join(VALVE_OFF).is_file() && !lang.join(VALVE_ON).exists());

        // Установка поверх выключенного убирает старый .off.
        set_enabled(&dota, false).unwrap();
        std::fs::write(&built, b"new pak").unwrap();
        let plan = Plan::for_install(&dota).unwrap();
        assert!(plan.replaces_existing && plan.disables_valve_pak);
        install(&plan, &built).unwrap();
        assert_eq!(std::fs::read(lang.join(PAK_NAME)).unwrap(), b"new pak");
        assert!(!lang.join(PAK_OFF).exists());

        // Откат убирает и выключенный пак.
        set_enabled(&dota, false).unwrap();
        rollback(&dota).unwrap();
        assert_eq!(mod_state(&dota), ModState::Absent);
        assert!(!has_changes(&dota));
        assert_eq!(std::fs::read(lang.join(VALVE_ON)).unwrap(), b"valve");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn install_then_rollback_restores_everything() {
        if is_dota_running() {
            return; // на машине с запущенной Дотой установка честно откажет
        }
        let (dota, root) = fake_dota("roundtrip", true);
        let lang = dota.language_dir().unwrap();
        let built = root.join("built.vpk");
        std::fs::write(&built, b"our pak").unwrap();

        let plan = Plan::for_install(&dota).unwrap();
        assert!(plan.disables_valve_pak && !plan.replaces_existing);
        install(&plan, &built).unwrap();
        assert_eq!(std::fs::read(lang.join(PAK_NAME)).unwrap(), b"our pak");
        assert!(!lang.join("pak01_dir.vpk").exists());
        assert!(lang.join("pak01_dir.vpk.off").is_file());
        assert!(lang.join("pak01_000.vpk").is_file());
        assert!(has_changes(&dota));

        // Повторная установка просто заменяет pak02.
        let plan = Plan::for_install(&dota).unwrap();
        assert!(plan.replaces_existing && !plan.disables_valve_pak);
        install(&plan, &built).unwrap();

        rollback(&dota).unwrap();
        assert!(!lang.join(PAK_NAME).exists());
        assert_eq!(std::fs::read(lang.join("pak01_dir.vpk")).unwrap(), b"valve");
        assert!(!has_changes(&dota));
        assert!(rollback(&dota).is_err(), "второй откат — нечего откатывать");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn alerts() {
        let (dota, root) = fake_dota("alerts", true);
        let lang = dota.language_dir().unwrap();
        // Только пак Valve, нашего нет — всё штатно.
        assert_eq!(check(&dota), None);
        // Наш пак установлен и Valve отключён — тоже штатно.
        std::fs::write(lang.join(PAK_NAME), b"ours").unwrap();
        std::fs::rename(lang.join("pak01_dir.vpk"), lang.join("pak01_dir.vpk.off")).unwrap();
        assert_eq!(check(&dota), None);
        // Steam вернул pak01_dir.vpk — наш пак снова перекрыт.
        std::fs::write(lang.join("pak01_dir.vpk"), b"valve again").unwrap();
        assert_eq!(check(&dota), Some(Alert::ValveRestored));
        // Английская озвучка, другой не выбрано — папки языка нет.
        let english = DotaInstall { audio_language: None, voice_language: None, ..dota.clone() };
        assert_eq!(check(&english), Some(Alert::EnglishAudio));
        // В игре английская, а мод для русской — его не будет слышно.
        let mismatch = DotaInstall { audio_language: None, ..dota.clone() };
        assert_eq!(check(&mismatch), Some(Alert::OtherAudio));
        assert!(valve_pak_modified(&dota).is_some());
        std::fs::remove_dir_all(root).unwrap();
    }
}
