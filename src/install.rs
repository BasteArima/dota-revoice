//! Установка собранного пака в игру, выключение и откат.
//!
//! Наш пак — свой `pak01_dir.vpk`: оглавление Valve, где чужие фразы ссылаются на тома
//! Valve `pak01_00N.vpk`, а мои лежат внутри. Оригинал Valve на это время откладываем
//! как `pak01_dir.vpk.off`. Выключение: наш → `pak01_dir.vpk.revoice`, оригинал — на
//! место; включение — наоборот, без копирования. Откат: наш пак удалить, оригинал вернуть.
//! Свой файл от файла Valve отличаем по заголовку (`vpk::is_signed`), а по метке внутри
//! (`pack::built_on`) видно, на каком оглавлении Valve он собран. Тома и чужие моды не трогаем.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};

use crate::game::DotaInstall;
use crate::{pack, vpk};
use crate::{tr, trf};

/// Наш пак в игре — под именем пака озвучки Valve.
pub const PAK_NAME: &str = "pak01_dir.vpk";
/// Наш пак, выключенный переключателем: лежит в игре, но не грузится.
pub const PAK_OFF: &str = "pak01_dir.vpk.revoice";
/// Оригинал Valve, отложенный на время работы мода.
pub const VALVE_OFF: &str = "pak01_dir.vpk.off";
const VALVE_ON: &str = PAK_NAME;
/// Прежняя схема (до 0.4): полная копия озвучки на ~550 МБ в `pak02`. При установке убираем.
const LEGACY: [&str; 2] = ["pak02_dir.vpk", "pak02_dir.vpk.off"];

/// Наш пак в папке языка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModState {
    Absent,
    Enabled,
    /// Выключен: игра звучит озвучкой Valve, включается обратно мгновенно.
    Disabled,
    /// Пак прежней версии программы (`pak02`): работает, но весит как вся озвучка.
    Legacy,
}

/// `pak01_dir.vpk` в папке — наш (без подписи Valve).
fn ours_on(dir: &Path) -> bool {
    let p = dir.join(PAK_NAME);
    p.is_file() && !vpk::is_signed(&p)
}

/// `pak01_dir.vpk` в папке — оригинал Valve.
fn valve_on(dir: &Path) -> bool {
    let p = dir.join(VALVE_ON);
    p.is_file() && vpk::is_signed(&p)
}

fn legacy_present(dir: &Path) -> bool {
    LEGACY.iter().any(|n| dir.join(n).is_file())
}

pub fn mod_state(dota: &DotaInstall) -> ModState {
    match dota.language_dir() {
        Some(d) if ours_on(&d) => ModState::Enabled,
        Some(d) if d.join(PAK_OFF).is_file() => ModState::Disabled,
        Some(d) if legacy_present(&d) => ModState::Legacy,
        _ => ModState::Absent,
    }
}

/// Сам файл нашего пака в игре — включённый или выключенный.
pub fn installed_pak(dota: &DotaInstall) -> Option<PathBuf> {
    let dir = dota.language_dir()?;
    if ours_on(&dir) {
        return Some(dir.join(PAK_NAME));
    }
    Some(dir.join(PAK_OFF)).filter(|p| p.is_file())
}

/// Оглавление Valve, на котором собирать пак: включённое (Steam мог положить свежее),
/// иначе отложенное `.off`.
pub fn valve_index(dota: &DotaInstall) -> Option<PathBuf> {
    let dir = dota.language_dir()?;
    if valve_on(&dir) {
        return Some(dir.join(VALVE_ON));
    }
    Some(dir.join(VALVE_OFF)).filter(|p| p.is_file())
}

/// Пак собран на другом оглавлении Valve — после обновления Доты его ссылки ведут не туда.
/// Пак без метки (или собранный без озвучки игры) не проверяем.
pub fn built_outdated(dota: &DotaInstall, pak: &Path) -> bool {
    let Some(Some(base)) = pack::built_on(pak) else { return false };
    valve_index(dota).and_then(|v| pack::index_stamp(&v)).is_some_and(|now| now != base)
}

fn ensure_fresh(dota: &DotaInstall, pak: &Path) -> Result<()> {
    ensure!(
        !built_outdated(dota, pak),
        tr!(
            "Озвучка игры обновилась после сборки — пересобери пак и установи заново",
            "The game's voice-over was updated after the build — rebuild the pack and install it again"
        )
    );
    Ok(())
}

/// Что именно изменится в папке игры — показываем пользователю до подтверждения.
#[derive(Debug, Clone)]
pub struct Plan {
    pub language_dir: PathBuf,
    /// Оригинал Valve включён — отложим его в `.off`.
    pub sets_valve_aside: bool,
    /// Рядом лежит старый `.off`, а Steam уже положил свежий оригинал: старый удалим.
    pub drops_stale_valve: bool,
    /// В папке уже есть наш пак — заменим.
    pub replaces_existing: bool,
    /// Лежит выключенный наш пак — после установки он не нужен.
    pub drops_disabled: bool,
    /// Файлы прежней версии (`pak02`), которые удалим.
    pub legacy: Vec<&'static str>,
}

impl Plan {
    pub fn for_install(dota: &DotaInstall, built_pak: &Path) -> Result<Self> {
        let language_dir = dota.language_dir().context(
            tr!("у Доты английская озвучка — моды кладутся в папку языка озвучки (например, dota_russian)", "Dota's voice-over is English — mods go into a voice-over language folder (e.g. dota_russian)"),
        )?;
        ensure!(language_dir.is_dir(), trf!("нет папки {}", "no folder {}", language_dir.display()));
        ensure!(built_pak.is_file(), trf!("сначала собери пак: нет {}", "build the pack first: {} is missing", built_pak.display()));
        ensure_fresh(dota, built_pak)?;
        let valve = valve_on(&language_dir);
        Ok(Self {
            sets_valve_aside: valve,
            drops_stale_valve: valve && language_dir.join(VALVE_OFF).is_file(),
            replaces_existing: ours_on(&language_dir),
            drops_disabled: language_dir.join(PAK_OFF).is_file(),
            legacy: LEGACY.into_iter().filter(|n| language_dir.join(n).is_file()).collect(),
            language_dir,
        })
    }

    pub fn describe(&self) -> Vec<String> {
        let mut steps = Vec::new();
        if self.drops_stale_valve {
            steps.push(trf!(
                "Удалить устаревший {VALVE_OFF} — Steam положил свежий оригинал",
                "Delete the outdated {VALVE_OFF} — Steam put a fresh original"
            ));
        }
        if self.sets_valve_aside {
            steps.push(trf!(
                "Переименовать {VALVE_ON} Valve в {VALVE_OFF} — оригинал остаётся рядом",
                "Rename Valve's {VALVE_ON} to {VALVE_OFF} — the original stays next to it"
            ));
        }
        steps.push(trf!(
            "{} наш {PAK_NAME} в {}",
            "{} our {PAK_NAME} into {}",
            if self.replaces_existing { tr!("Заменить", "Replace") } else { tr!("Положить", "Put") },
            self.language_dir.display()
        ));
        if self.drops_disabled {
            steps.push(trf!("Удалить прежний выключенный {PAK_OFF}", "Delete the previous disabled {PAK_OFF}"));
        }
        for name in &self.legacy {
            steps.push(trf!(
                "Удалить {name} прежней версии программы — копия всей озвучки больше не нужна",
                "Delete {name} from the previous app version — a full copy of the voice-over isn't needed anymore"
            ));
        }
        steps
    }
}

pub fn install(plan: &Plan, built_pak: &Path) -> Result<()> {
    ensure!(!is_dota_running(), tr!("Дота запущена — закрой её, она держит паки открытыми", "Dota is running — close it, it keeps the packs open"));
    ensure!(built_pak.is_file(), trf!("сначала собери пак: нет {}", "build the pack first: {} is missing", built_pak.display()));
    let dir = &plan.language_dir;

    // Сначала копируем во временный файл рядом: если копирование оборвётся,
    // в игре ничего не поменяется.
    let target = dir.join(PAK_NAME);
    let tmp = dir.join(format!("{PAK_NAME}.tmp"));
    std::fs::copy(built_pak, &tmp).with_context(|| trf!("не скопировался пак в {}", "couldn't copy the pack to {}", tmp.display()))?;
    if let Err(e) = set_valve_aside(dir) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| trf!("не заменился {} — файл занят?", "couldn't replace {} — is the file in use?", target.display()));
    }
    for name in std::iter::once(PAK_OFF).chain(LEGACY) {
        let p = dir.join(name);
        if p.is_file() {
            std::fs::remove_file(&p).with_context(|| trf!("не удаляется {}", "can't delete {}", p.display()))?;
        }
    }
    Ok(())
}

/// Оригинал Valve `pak01_dir.vpk` → `.off`. Старый `.off` при этом лишний: раз Steam
/// положил оригинал заново, свежий — он.
fn set_valve_aside(dir: &Path) -> Result<()> {
    if !valve_on(dir) {
        return Ok(());
    }
    let (on, off) = (dir.join(VALVE_ON), dir.join(VALVE_OFF));
    if off.exists() {
        std::fs::remove_file(&off).with_context(|| trf!("не удаляется старый {}", "can't delete the old {}", off.display()))?;
    }
    std::fs::rename(&on, &off).with_context(|| trf!("не переименовался {} — закрой Доту и Source 2 Viewer", "couldn't rename {} — close Dota and Source 2 Viewer", on.display()))
}

/// Оригинал Valve — обратно на место (место должно быть свободно).
fn restore_valve(dir: &Path, done: &mut Vec<String>) -> Result<()> {
    let (on, off) = (dir.join(VALVE_ON), dir.join(VALVE_OFF));
    if !off.is_file() {
        return Ok(());
    }
    if valve_on(dir) {
        std::fs::remove_file(&off).with_context(|| trf!("не удаляется {}", "can't delete {}", off.display()))?;
        done.push(trf!("{VALVE_ON} Valve уже на месте, лишний .off удалён", "Valve's {VALVE_ON} is already in place, the extra .off was deleted"));
    } else {
        std::fs::rename(&off, &on).with_context(|| trf!("не переименовался {}", "couldn't rename {}", off.display()))?;
        done.push(trf!("{VALVE_OFF} → {VALVE_ON}: озвучка Valve на месте", "{VALVE_OFF} → {VALVE_ON}: Valve's voice-over is back"));
    }
    Ok(())
}

/// Включает или выключает установленный пак переименованием — без копирования.
/// Возвращает список сделанного.
pub fn set_enabled(dota: &DotaInstall, enabled: bool) -> Result<Vec<String>> {
    ensure!(!is_dota_running(), tr!("Дота запущена — закрой её, она держит паки открытыми", "Dota is running — close it, it keeps the packs open"));
    let dir = dota.language_dir().context(tr!("у Доты нет папки языка озвучки", "Dota has no voice-over language folder"))?;
    let (on, off) = (dir.join(PAK_NAME), dir.join(PAK_OFF));
    let mut done = Vec::new();
    if enabled {
        if ours_on(&dir) {
            return Ok(done);
        }
        ensure!(off.is_file(), trf!("в {} нет нашего пака — установи его", "our pack isn't in {} — install it", dir.display()));
        ensure_fresh(dota, &off)?;
        if valve_on(&dir) {
            set_valve_aside(&dir)?;
            done.push(trf!("{VALVE_ON} Valve → {VALVE_OFF}", "Valve's {VALVE_ON} → {VALVE_OFF}"));
        }
        std::fs::rename(&off, &on).with_context(|| trf!("не переименовался {}", "couldn't rename {}", off.display()))?;
        done.push(format!("{PAK_OFF} → {PAK_NAME}"));
    } else {
        ensure!(ours_on(&dir), trf!("в {} нет включённого нашего пака", "there's no enabled pack of ours in {}", dir.display()));
        // Без оригинала Valve выключить нечем: игра осталась бы совсем без этой озвучки.
        ensure!(
            dir.join(VALVE_OFF).is_file(),
            trf!(
                "нет оригинала Valve ({VALVE_OFF}) — проверь целостность файлов Доты в Steam",
                "Valve's original ({VALVE_OFF}) is missing — verify Dota's files in Steam"
            )
        );
        if off.exists() {
            std::fs::remove_file(&off).with_context(|| trf!("не удаляется {}", "can't delete {}", off.display()))?;
        }
        std::fs::rename(&on, &off).with_context(|| trf!("не переименовался {}", "couldn't rename {}", on.display()))?;
        done.push(format!("{PAK_NAME} → {PAK_OFF}"));
        restore_valve(&dir, &mut done)?;
    }
    Ok(done)
}

/// Убирает наш пак (включённый, выключенный и прежней версии) и возвращает озвучку Valve.
/// Возвращает список сделанного.
pub fn rollback(dota: &DotaInstall) -> Result<Vec<String>> {
    ensure!(!is_dota_running(), tr!("Дота запущена — закрой её, она держит паки открытыми", "Dota is running — close it, it keeps the packs open"));
    let dir = dota.language_dir().context(tr!("у Доты нет папки языка озвучки", "Dota has no voice-over language folder"))?;
    let mut done = Vec::new();

    let mut ours: Vec<&str> = vec![PAK_OFF];
    ours.extend(LEGACY);
    if ours_on(&dir) {
        ours.insert(0, PAK_NAME);
    }
    for name in ours {
        let pak = dir.join(name);
        if pak.is_file() {
            std::fs::remove_file(&pak).with_context(|| trf!("не удаляется {}", "can't delete {}", pak.display()))?;
            done.push(trf!("Удалён {name}", "Deleted {name}"));
        }
    }
    restore_valve(&dir, &mut done)?;
    if done.is_empty() {
        bail!(trf!("в {} нечего откатывать", "nothing to roll back in {}", dir.display()));
    }
    Ok(done)
}

/// Что не так с игрой с точки зрения мода — показываем предупреждением.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    /// Мод был включён, но Steam положил свой `pak01_dir.vpk` (обновление или проверка
    /// файлов) — в игре снова озвучка Valve.
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
    // Отложенный оригинал рядом с включённым — значит, наш пак был включён и Steam его заменил.
    // Выключенный мод так не выглядит: при выключении оригинал возвращается из `.off`.
    let replaced = valve_on(&dir) && (dir.join(VALVE_OFF).is_file() || dir.join(LEGACY[0]).is_file());
    replaced.then_some(Alert::ValveRestored)
}

/// Установлено ли что-то нашего: наш пак (в том числе выключенный или прежней версии)
/// или отложенный оригинал Valve.
pub fn has_changes(dota: &DotaInstall) -> bool {
    dota.language_dir().is_some_and(|d| ours_on(&d) || legacy_present(&d) || [PAK_OFF, VALVE_OFF].iter().any(|n| d.join(n).is_file()))
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
    ensure!(status.context(tr!("Steam не запустился", "Steam didn't start"))?.success(), tr!("Steam не открыл Доту — он установлен?", "Steam didn't open Dota — is it installed?"));
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
    use std::collections::BTreeMap;

    use super::*;

    /// Оглавление «как у Valve»: пустое дерево, но с подписью и таблицей MD5 томов в заголовке.
    fn valve_index_bytes(tag: u8) -> Vec<u8> {
        let mut b = Vec::new();
        for v in [0x55AA_1234u32, 2, 1, 0, 16, 48, 20] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.push(0); // пустое дерево
        b.extend(std::iter::repeat_n(tag, 16 + 48 + 20));
        b
    }

    fn fake_dota(name: &str) -> (DotaInstall, PathBuf) {
        let root = std::env::temp_dir().join(format!("dota_revoice_install_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let lang = root.join("game/dota_russian");
        std::fs::create_dir_all(&lang).unwrap();
        std::fs::write(lang.join(VALVE_ON), valve_index_bytes(1)).unwrap();
        std::fs::write(lang.join("pak01_000.vpk"), b"volume").unwrap();
        let russian = Some("russian".to_owned());
        (DotaInstall { game_dir: root.join("game"), audio_language: russian.clone(), voice_language: russian }, root)
    }

    /// Собирает наш пак на текущем оглавлении Valve, как вкладка сборки.
    fn build(dota: &DotaInstall, out: &Path) {
        let base = valve_index(dota).unwrap();
        pack::build_overlay(Some(&base), &BTreeMap::new(), out, None).unwrap();
    }

    #[test]
    fn install_toggle_and_rollback() {
        if is_dota_running() {
            return; // на машине с запущенной Дотой установка честно откажет
        }
        let (dota, root) = fake_dota("toggle");
        let lang = dota.language_dir().unwrap();
        let valve = std::fs::read(lang.join(VALVE_ON)).unwrap();
        let built = root.join("build").join(PAK_NAME);
        build(&dota, &built);
        assert_eq!(mod_state(&dota), ModState::Absent);
        assert!(set_enabled(&dota, true).is_err(), "включать нечего");
        assert!(!vpk::is_signed(&built) && vpk::is_signed(&lang.join(VALVE_ON)));

        let plan = Plan::for_install(&dota, &built).unwrap();
        assert!(plan.sets_valve_aside && !plan.drops_stale_valve && !plan.replaces_existing);
        install(&plan, &built).unwrap();
        assert_eq!(mod_state(&dota), ModState::Enabled);
        assert_eq!(std::fs::read(lang.join(PAK_NAME)).unwrap(), std::fs::read(&built).unwrap());
        assert_eq!(std::fs::read(lang.join(VALVE_OFF)).unwrap(), valve, "оригинал отложен");
        assert!(lang.join("pak01_000.vpk").is_file());
        assert_eq!(check(&dota), None);
        assert_eq!(valve_index(&dota).unwrap(), lang.join(VALVE_OFF));
        assert!(!built_outdated(&dota, &built));

        // Выключили: наш пак отложен, оригинал Valve вернулся.
        set_enabled(&dota, false).unwrap();
        assert_eq!(mod_state(&dota), ModState::Disabled);
        assert_eq!(std::fs::read(lang.join(VALVE_ON)).unwrap(), valve);
        assert!(!lang.join(VALVE_OFF).exists());
        assert_eq!(check(&dota), None, "выключенный мод — не повод для тревоги");
        assert_eq!(installed_pak(&dota).unwrap(), lang.join(PAK_OFF));

        // Включили обратно.
        set_enabled(&dota, true).unwrap();
        assert_eq!(mod_state(&dota), ModState::Enabled);
        assert!(lang.join(VALVE_OFF).is_file() && !lang.join(PAK_OFF).exists());

        // Установка поверх выключенного убирает отложенный наш пак.
        set_enabled(&dota, false).unwrap();
        let plan = Plan::for_install(&dota, &built).unwrap();
        assert!(plan.sets_valve_aside && plan.drops_disabled);
        install(&plan, &built).unwrap();
        assert!(!lang.join(PAK_OFF).exists());

        rollback(&dota).unwrap();
        assert_eq!(mod_state(&dota), ModState::Absent);
        assert!(!has_changes(&dota));
        assert_eq!(std::fs::read(lang.join(VALVE_ON)).unwrap(), valve);
        assert!(rollback(&dota).is_err(), "второй откат — нечего откатывать");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Steam обновил Доту: положил свой свежий `pak01_dir.vpk` поверх нашего.
    #[test]
    fn steam_update_needs_rebuild() {
        if is_dota_running() {
            return;
        }
        let (dota, root) = fake_dota("update");
        let lang = dota.language_dir().unwrap();
        let built = root.join("build").join(PAK_NAME);
        build(&dota, &built);
        install(&Plan::for_install(&dota, &built).unwrap(), &built).unwrap();

        std::fs::write(lang.join(VALVE_ON), valve_index_bytes(2)).unwrap();
        assert_eq!(check(&dota), Some(Alert::ValveRestored));
        assert_eq!(mod_state(&dota), ModState::Absent);
        assert_eq!(valve_index(&dota).unwrap(), lang.join(VALVE_ON), "собирать — на свежем оглавлении");
        assert!(built_outdated(&dota, &built));
        assert!(Plan::for_install(&dota, &built).is_err(), "старую сборку не ставим");

        build(&dota, &built);
        let plan = Plan::for_install(&dota, &built).unwrap();
        assert!(plan.sets_valve_aside && plan.drops_stale_valve);
        install(&plan, &built).unwrap();
        assert_eq!(std::fs::read(lang.join(VALVE_OFF)).unwrap(), valve_index_bytes(2), "отложен свежий оригинал");
        assert_eq!(check(&dota), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Прежняя версия программы ставила полную копию озвучки в `pak02`.
    #[test]
    fn install_replaces_legacy_pak02() {
        if is_dota_running() {
            return;
        }
        let (dota, root) = fake_dota("legacy");
        let lang = dota.language_dir().unwrap();
        std::fs::rename(lang.join(VALVE_ON), lang.join(VALVE_OFF)).unwrap();
        std::fs::write(lang.join(LEGACY[0]), b"old full copy").unwrap();
        assert_eq!(mod_state(&dota), ModState::Legacy);
        assert!(has_changes(&dota));
        assert_eq!(check(&dota), None);

        let built = root.join("build").join(PAK_NAME);
        build(&dota, &built);
        let plan = Plan::for_install(&dota, &built).unwrap();
        assert!(!plan.sets_valve_aside && plan.legacy == [LEGACY[0]]);
        install(&plan, &built).unwrap();
        assert_eq!(mod_state(&dota), ModState::Enabled);
        assert!(!lang.join(LEGACY[0]).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn alerts() {
        let (dota, root) = fake_dota("alerts");
        let lang = dota.language_dir().unwrap();
        // Только оригинал Valve — всё штатно.
        assert_eq!(check(&dota), None);
        // Прежняя версия: pak02 лежит, а Steam вернул pak01 — наш пак снова перекрыт.
        std::fs::write(lang.join(LEGACY[0]), b"ours").unwrap();
        assert_eq!(check(&dota), Some(Alert::ValveRestored));
        // Английская озвучка, другой не выбрано — папки языка нет.
        let english = DotaInstall { audio_language: None, voice_language: None, ..dota.clone() };
        assert_eq!(check(&english), Some(Alert::EnglishAudio));
        // В игре английская, а мод для русской — его не будет слышно.
        let mismatch = DotaInstall { audio_language: None, ..dota.clone() };
        assert_eq!(check(&mismatch), Some(Alert::OtherAudio));
        std::fs::remove_dir_all(root).unwrap();
    }
}
