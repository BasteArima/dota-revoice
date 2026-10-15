//! Установка собранного пака в игру и откат.
//!
//! Установка: положить `pak02_dir.vpk` в папку языка и отключить пак озвучки
//! Valve (`pak01_dir.vpk` → `pak01_dir.vpk.off`), иначе он перекрывает наш.
//! Откат: убрать `pak02_dir.vpk` и вернуть `pak01_dir.vpk` на место.
//! Тома `pak01_00N.vpk` и чужие моды не трогаем.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};

use crate::game::DotaInstall;

pub const PAK_NAME: &str = "pak02_dir.vpk";

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
            replaces_existing: language_dir.join(PAK_NAME).is_file(),
            disables_valve_pak: language_dir.join("pak01_dir.vpk").is_file(),
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

    if plan.disables_valve_pak {
        let on = plan.language_dir.join("pak01_dir.vpk");
        let off = plan.language_dir.join("pak01_dir.vpk.off");
        if off.exists() {
            // Steam вернул pak01, а старый .off остался — он такой же, лишний.
            std::fs::remove_file(&off).with_context(|| format!("не удаляется старый {}", off.display()))?;
        }
        std::fs::rename(&on, &off)
            .with_context(|| format!("не переименовался {} — закрой Доту и Source 2 Viewer", on.display()))?;
    }
    Ok(())
}

/// Возвращает список сделанного.
pub fn rollback(dota: &DotaInstall) -> Result<Vec<String>> {
    ensure!(!is_dota_running(), "Дота запущена — закрой её, она держит паки открытыми");
    let dir = dota.language_dir().context("у Доты нет папки языка озвучки")?;
    let mut done = Vec::new();

    let pak = dir.join(PAK_NAME);
    if pak.is_file() {
        std::fs::remove_file(&pak).with_context(|| format!("не удаляется {}", pak.display()))?;
        done.push(format!("Удалён {PAK_NAME}"));
    }
    let on = dir.join("pak01_dir.vpk");
    let off = dir.join("pak01_dir.vpk.off");
    if off.is_file() {
        if on.exists() {
            std::fs::remove_file(&off)?;
            done.push("pak01_dir.vpk уже на месте, лишний .off удалён".into());
        } else {
            std::fs::rename(&off, &on).with_context(|| format!("не переименовался {}", off.display()))?;
            done.push("pak01_dir.vpk включён обратно".into());
        }
    }
    if done.is_empty() {
        bail!("в {} нечего откатывать", dir.display());
    }
    Ok(done)
}

/// Установлено ли что-то нашего: есть `pak02` или отключённый пак Valve.
pub fn has_changes(dota: &DotaInstall) -> bool {
    dota.language_dir()
        .is_some_and(|d| d.join(PAK_NAME).is_file() || d.join("pak01_dir.vpk.off").is_file())
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
        (DotaInstall { game_dir: root.join("game"), audio_language: Some("russian".into()) }, root)
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
}
