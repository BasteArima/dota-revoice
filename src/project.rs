//! Проект переозвучки одного героя.
//!
//! Открываем папку с оригиналами, выгруженными из игры, например
//! `example/sounds/vo/bounty_hunter`. Свои фразы кладём рядом с корнем выгрузки:
//!
//! ```text
//! example/sounds/vo/bounty_hunter/bount_kill_01.mp3        ← оригинал
//! example/export/sounds/vo/bounty_hunter/bount_kill_01.mp3 ← моя фраза, то же имя
//! example/export/.raw/sounds/vo/bounty_hunter/…wav          ← исходник без потерь
//! example/export/.trash/…                                    ← удалённое
//! example/export/.takes/…                                    ← прежние дубли
//! example/export/dota_revoice.json                          ← обрезка, обработка, заметки
//! ```

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

pub const AUDIO_EXTENSIONS: [&str; 8] = ["mp3", "wav", "ogg", "oga", "opus", "flac", "m4a", "aac"];
const STATE_FILE: &str = "dota_revoice.json";

#[derive(Debug, Clone)]
pub struct Line {
    /// Имя файла без расширения: `bount_kill_01`.
    pub name: String,
    /// `kill`, `ability_track`… — для группировки.
    pub category: String,
    pub original: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Empty,
    Recorded,
    Redo,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    /// Пути ресурсов (`sounds/vo/hero/name`), помеченные «переделать».
    #[serde(default)]
    redo: BTreeSet<String>,
    /// Обрезка: какой кусок исходного wav идёт в игру, секунды [начало, конец].
    #[serde(default)]
    trims: BTreeMap<String, [f32; 2]>,
    /// Заметки пользователя к фразам: «покупка BKB» и т.п.
    #[serde(default)]
    notes: BTreeMap<String, String>,
    /// Усиление моей версии, dB (подгонка громкости под оригинал).
    #[serde(default)]
    gains: BTreeMap<String, f32>,
    /// Фразы, к которым применён шумодав.
    #[serde(default)]
    denoise: BTreeSet<String>,
    /// Сдвиг тона моей версии, полутоны.
    #[serde(default)]
    pitches: BTreeMap<String, f32>,
}

/// Прежний дубль фразы: исходник и то, как он был обрезан и обработан.
#[derive(Debug, Clone)]
pub struct Take {
    pub id: u32,
    pub path: PathBuf,
    pub saved: Option<SystemTime>,
    pub secs: f32,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct TakeMeta {
    trim: Option<[f32; 2]>,
    fx: crate::process::Settings,
}

/// Хранилище моих фраз в `export/` — по путям ресурсов (`sounds/vo/hero/name`),
/// без привязки к открытому герою: им пользуются и проект, и импорт пачек.
pub struct Store {
    pub root: PathBuf,
    state: State,
}

impl Store {
    pub fn open(root: &Path) -> Self {
        let state = std::fs::read(root.join(STATE_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { root: root.to_path_buf(), state }
    }

    fn save(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::write(self.root.join(STATE_FILE), serde_json::to_vec_pretty(&self.state)?)?;
        Ok(())
    }

    fn save_if(&self, changed: bool) -> Result<()> {
        if changed { self.save() } else { Ok(()) }
    }

    /// Моя фраза для игры.
    pub fn mp3(&self, res: &str) -> PathBuf {
        self.root.join(format!("{res}.mp3"))
    }

    /// Исходник без потерь.
    pub fn raw(&self, res: &str) -> PathBuf {
        self.root.join(".raw").join(format!("{res}.wav"))
    }

    /// Папка прежних дублей: `export/.takes/sounds/vo/hero/name/`.
    fn takes_dir(&self, res: &str) -> PathBuf {
        self.root.join(".takes").join(res)
    }

    /// Корзина для фразы и её имя файла: (`export/.trash/sounds/vo/hero`, `name`).
    fn trash_place(&self, res: &str) -> (PathBuf, String) {
        let (dir, name) = res.rsplit_once('/').unwrap_or(("", res));
        (self.root.join(".trash").join(dir), name.to_owned())
    }

    pub fn is_redo(&self, res: &str) -> bool {
        self.state.redo.contains(res)
    }

    pub fn set_redo(&mut self, res: &str, redo: bool) -> Result<()> {
        let changed = if redo { self.state.redo.insert(res.to_owned()) } else { self.state.redo.remove(res) };
        self.save_if(changed)
    }

    /// Обрезка фразы в секундах; `None` — фраза целиком.
    pub fn trim(&self, res: &str) -> Option<(f32, f32)> {
        self.state.trims.get(res).map(|&[a, b]| (a, b))
    }

    pub fn set_trim(&mut self, res: &str, trim: Option<(f32, f32)>) -> Result<()> {
        let changed = match trim {
            Some((a, b)) => self.state.trims.insert(res.to_owned(), [a, b]) != Some([a, b]),
            None => self.state.trims.remove(res).is_some(),
        };
        self.save_if(changed)
    }

    /// Шумодав, усиление и тон для фразы.
    pub fn processing(&self, res: &str) -> crate::process::Settings {
        crate::process::Settings {
            denoise: self.state.denoise.contains(res),
            gain_db: self.state.gains.get(res).copied().unwrap_or(0.0),
            pitch: self.state.pitches.get(res).copied().unwrap_or(0.0),
        }
    }

    pub fn set_processing(&mut self, res: &str, s: crate::process::Settings) -> Result<()> {
        let denoise_changed = if s.denoise { self.state.denoise.insert(res.to_owned()) } else { self.state.denoise.remove(res) };
        let set = |map: &mut BTreeMap<String, f32>, v: f32| {
            if v.abs() < 0.05 { map.remove(res).is_some() } else { map.insert(res.to_owned(), v) != Some(v) }
        };
        let gain_changed = set(&mut self.state.gains, s.gain_db);
        let pitch_changed = set(&mut self.state.pitches, s.pitch);
        self.save_if(denoise_changed || gain_changed || pitch_changed)
    }

    pub fn note(&self, res: &str) -> &str {
        self.state.notes.get(res).map_or("", String::as_str)
    }

    /// Пустая заметка удаляется.
    pub fn set_note(&mut self, res: &str, note: &str) -> Result<()> {
        let note = note.trim();
        let changed = if note.is_empty() {
            self.state.notes.remove(res).is_some()
        } else {
            self.state.notes.insert(res.to_owned(), note.to_owned()).as_deref() != Some(note)
        };
        self.save_if(changed)
    }

    /// Все заметки: путь ресурса → текст.
    pub fn notes(&self) -> &BTreeMap<String, String> {
        &self.state.notes
    }

    /// Прежние дубли, новые первыми.
    pub fn takes(&self, res: &str) -> Vec<Take> {
        let Ok(entries) = std::fs::read_dir(self.takes_dir(res)) else { return Vec::new() };
        let mut takes: Vec<Take> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter_map(|path| {
                let id = path.extension().filter(|e| *e == "wav")?.to_str().and(path.file_stem()?.to_str()?.parse().ok())?;
                let secs = hound::WavReader::open(&path).map(|r| r.duration() as f32 / r.spec().sample_rate as f32).unwrap_or(0.0);
                let saved = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                Some(Take { id, path, saved, secs })
            })
            .collect();
        takes.sort_by_key(|t| std::cmp::Reverse(t.id));
        takes
    }

    /// Убирает текущий исходник в дубли (вместе с его обрезкой и обработкой).
    pub fn archive_current(&mut self, res: &str) -> Result<()> {
        let raw = self.raw(res);
        if !raw.is_file() {
            return Ok(());
        }
        let dir = self.takes_dir(res);
        std::fs::create_dir_all(&dir)?;
        let id = self.takes(res).first().map_or(1, |t| t.id + 1);
        let meta = TakeMeta { trim: self.trim(res).map(|(a, b)| [a, b]), fx: self.processing(res) };
        std::fs::write(dir.join(format!("{id:03}.json")), serde_json::to_vec_pretty(&meta)?)?;
        std::fs::rename(&raw, dir.join(format!("{id:03}.wav"))).with_context(|| format!("не перенёсся в дубли {}", raw.display()))?;
        Ok(())
    }

    /// Возвращает дубль `id` в работу, текущий исходник уходит в дубли.
    /// mp3 для игры после этого нужно пересобрать.
    pub fn restore_take(&mut self, res: &str, id: u32) -> Result<()> {
        let dir = self.takes_dir(res);
        let (wav, json) = (dir.join(format!("{id:03}.wav")), dir.join(format!("{id:03}.json")));
        ensure!(wav.is_file(), "нет дубля №{id}");
        let meta: TakeMeta = std::fs::read(&json).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        self.archive_current(res)?;
        let raw = self.raw(res);
        if let Some(parent) = raw.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&wav, &raw).with_context(|| format!("не вернулся дубль {}", wav.display()))?;
        let _ = std::fs::remove_file(json);
        self.set_trim(res, meta.trim.map(|[a, b]| (a, b)))?;
        self.set_processing(res, meta.fx)
    }

    /// Дубль — в корзину.
    pub fn trash_take(&mut self, res: &str, id: u32) -> Result<()> {
        let dir = self.takes_dir(res);
        let (trash, name) = self.trash_place(res);
        std::fs::create_dir_all(&trash)?;
        for ext in ["wav", "json"] {
            let src = dir.join(format!("{id:03}.{ext}"));
            if src.is_file() {
                std::fs::rename(&src, trash.join(format!("{}_{name}_take{id:03}.{ext}", stamp())))?;
            }
        }
        Ok(())
    }

    /// Переносит мою фразу (и её wav) в `export/.trash`, ничего не удаляя насовсем.
    pub fn trash(&mut self, res: &str) -> Result<()> {
        let (trash, _) = self.trash_place(res);
        std::fs::create_dir_all(&trash)?;
        for src in [self.mp3(res), self.raw(res)] {
            if src.is_file() {
                let name = src.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                std::fs::rename(&src, trash.join(format!("{}_{name}", stamp())))
                    .with_context(|| format!("не перенёсся в корзину {}", src.display()))?;
            }
        }
        self.set_trim(res, None)?;
        self.set_processing(res, crate::process::Settings::default())?;
        self.set_redo(res, false)
    }
}

fn stamp() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub struct Project {
    pub source_dir: PathBuf,
    pub export_root: PathBuf,
    /// Каталог внутри игры: `sounds/vo/bounty_hunter`.
    pub resource_dir: String,
    pub hero: String,
    pub lines: Vec<Line>,
    recorded: HashSet<String>,
    store: Store,
}

impl Project {
    /// Свои фразы — в `export` рядом с выгрузкой (как было до настроек папок).
    pub fn open(source_dir: &Path) -> Result<Self> {
        Self::open_in(source_dir, &legacy_export_root(source_dir))
    }

    /// Свои фразы — в `export_root` (папка «Мои фразы» из настроек).
    pub fn open_in(source_dir: &Path, export_root: &Path) -> Result<Self> {
        let source_dir = source_dir.to_path_buf();
        let mut files: Vec<PathBuf> = std::fs::read_dir(&source_dir)
            .with_context(|| format!("не открывается папка {}", source_dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && has_audio_ext(p))
            .collect();
        ensure!(!files.is_empty(), "в папке {} нет звуков (mp3, wav, ogg…)", source_dir.display());
        files.sort();

        let (resource_dir, _) = locate(&source_dir);
        let export_root = export_root.to_path_buf();
        let hero = resource_dir.rsplit('/').next().unwrap_or_default().to_owned();

        let names: Vec<String> = files.iter().map(|p| file_stem(p)).collect();
        let prefix = main_prefix(&names);
        let lines = files
            .into_iter()
            .zip(names)
            .map(|(original, name)| Line { category: category(&name, &prefix), name, original })
            .collect();

        let store = Store::open(&export_root);
        let mut project = Self { source_dir, export_root, resource_dir, hero, lines, recorded: HashSet::new(), store };
        project.rescan();
        Ok(project)
    }

    /// Перечитать с диска: какие фразы записаны и состояние (после импорта пачки).
    pub fn rescan(&mut self) {
        self.store = Store::open(&self.export_root);
        self.recorded = self.lines.iter().filter(|l| self.export_mp3(l).is_file()).map(|l| l.name.clone()).collect();
    }

    pub fn resource_path(&self, line: &Line) -> String {
        format!("{}/{}", self.resource_dir, line.name)
    }

    /// Моя фраза: то же имя, что у оригинала.
    pub fn export_mp3(&self, line: &Line) -> PathBuf {
        self.store.mp3(&self.resource_path(line))
    }

    pub fn raw_wav(&self, line: &Line) -> PathBuf {
        self.store.raw(&self.resource_path(line))
    }

    pub fn status(&self, line: &Line) -> Status {
        if self.store.is_redo(&self.resource_path(line)) {
            Status::Redo
        } else if self.recorded.contains(&line.name) {
            Status::Recorded
        } else {
            Status::Empty
        }
    }

    pub fn recorded_count(&self) -> usize {
        self.lines.iter().filter(|l| self.recorded.contains(&l.name)).count()
    }

    pub fn mark_recorded(&mut self, line: &Line) -> Result<()> {
        self.recorded.insert(line.name.clone());
        self.set_redo(line, false)
    }

    pub fn set_redo(&mut self, line: &Line, redo: bool) -> Result<()> {
        let res = self.resource_path(line);
        self.store.set_redo(&res, redo)
    }

    /// Обрезка фразы в секундах; `None` — фраза целиком.
    pub fn trim(&self, line: &Line) -> Option<(f32, f32)> {
        self.store.trim(&self.resource_path(line))
    }

    pub fn set_trim(&mut self, line: &Line, trim: Option<(f32, f32)>) -> Result<()> {
        let res = self.resource_path(line);
        self.store.set_trim(&res, trim)
    }

    /// Шумодав, усиление и тон для фразы.
    pub fn processing(&self, line: &Line) -> crate::process::Settings {
        self.store.processing(&self.resource_path(line))
    }

    pub fn set_processing(&mut self, line: &Line, s: crate::process::Settings) -> Result<()> {
        let res = self.resource_path(line);
        self.store.set_processing(&res, s)
    }

    /// Прежние дубли, новые первыми.
    pub fn takes(&self, line: &Line) -> Vec<Take> {
        self.store.takes(&self.resource_path(line))
    }

    /// Убирает текущий исходник в дубли (вместе с его обрезкой и обработкой).
    pub fn archive_current(&mut self, line: &Line) -> Result<()> {
        let res = self.resource_path(line);
        self.store.archive_current(&res)
    }

    /// Возвращает дубль `id` в работу, текущий исходник уходит в дубли.
    pub fn restore_take(&mut self, line: &Line, id: u32) -> Result<()> {
        let res = self.resource_path(line);
        self.store.restore_take(&res, id)
    }

    /// Дубль — в корзину.
    pub fn trash_take(&mut self, line: &Line, id: u32) -> Result<()> {
        let res = self.resource_path(line);
        self.store.trash_take(&res, id)
    }

    pub fn note(&self, line: &Line) -> &str {
        self.store.note(&self.resource_path(line))
    }

    /// Пустая заметка удаляется.
    pub fn set_note(&mut self, line: &Line, note: &str) -> Result<()> {
        let res = self.resource_path(line);
        self.store.set_note(&res, note)
    }

    /// Переносит мою фразу (и её wav) в `export/.trash`, ничего не удаляя насовсем.
    pub fn trash(&mut self, line: &Line) -> Result<()> {
        let res = self.resource_path(line);
        self.store.trash(&res)?;
        self.recorded.remove(&line.name);
        Ok(())
    }
}

pub fn has_audio_ext(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.iter().any(|a| a.eq_ignore_ascii_case(e)))
}

fn file_stem(p: &Path) -> String {
    p.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_ascii_lowercase()
}

/// Где лежали свои фразы до настроек папок: `export` рядом с выгрузкой героя.
pub fn legacy_export_root(source_dir: &Path) -> PathBuf {
    locate(source_dir).1
}

/// Каталог ресурса внутри игры и корень экспорта.
/// `X/sounds/vo/hero` → (`sounds/vo/hero`, `X/export`);
/// папка без `sounds` в пути считается `sounds/vo/<имя папки>`.
fn locate(source_dir: &Path) -> (String, PathBuf) {
    let parts: Vec<&str> = source_dir.iter().filter_map(|c| c.to_str()).collect();
    if let Some(i) = parts.iter().rposition(|c| c.eq_ignore_ascii_case("sounds")) {
        let resource = parts[i..].join("/").to_ascii_lowercase();
        let mut root = source_dir.to_path_buf();
        for _ in i..parts.len() {
            root.pop();
        }
        return (resource, root.join("export"));
    }
    let hero = parts.last().copied().unwrap_or("hero").to_ascii_lowercase();
    let root = source_dir.parent().unwrap_or(source_dir).join("export");
    (format!("sounds/vo/{hero}"), root)
}

/// Префикс героя (`bount_`): самый частый первый кусок имени. `auto_` не в счёт —
/// это вторые версии фраз (`auto_axe_kill_01`), а не другой герой.
fn main_prefix(names: &[String]) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for n in names {
        let n = n.strip_prefix("auto_").unwrap_or(n);
        if let Some((p, _)) = n.split_once('_') {
            *counts.entry(p).or_default() += 1;
        }
    }
    counts.into_iter().max_by_key(|&(p, n)| (n, std::cmp::Reverse(p))).map(|(p, _)| format!("{p}_")).unwrap_or_default()
}

/// `bount_ability_track_kill_03` → `ability_track_kill`. Фразы с другим началом — в свои
/// группы: `auto_axe_kill_01` → `auto/kill`, `jung_kill_01` → `jung/kill`.
fn category(name: &str, prefix: &str) -> String {
    let mut tags = Vec::new();
    let mut rest = name;
    if let Some(r) = rest.strip_prefix("auto_") {
        tags.push("auto");
        rest = r;
    }
    if let Some(r) = rest.strip_prefix(prefix).filter(|_| !prefix.is_empty()) {
        rest = r;
    } else if !prefix.is_empty()
        && let Some((tag, r)) = rest.split_once('_')
    {
        tags.push(tag);
        rest = r;
    }
    // Хвост вида `_01` или `_12b` — номер варианта, в категорию не входит.
    let base = match rest.rsplit_once('_') {
        Some((head, tail)) if tail.starts_with(|c: char| c.is_ascii_digit()) => head,
        _ => rest,
    };
    if tags.is_empty() { base.to_owned() } else { format!("{}/{base}", tags.join("/")) }
}

/// Человеческое название категории.
pub fn category_label(category: &str) -> String {
    if let Some((tags, base)) = category.rsplit_once('/') {
        let tags: Vec<&str> = tags.split('/').map(|t| if t == "auto" { "Вторая версия" } else { t }).collect();
        return format!("{} · {}", tags.join(" · "), category_label(base));
    }
    if let Some(ability) = category.strip_prefix("ability_") {
        return format!("Способность: {ability}");
    }
    let label = match category {
        "attack" => "Атака",
        "kill" => "Убийства",
        "move" => "Движение",
        "death" => "Смерть",
        "respawn" => "Возрождение",
        "spawn" => "Появление",
        "laugh" => "Смех",
        "deny" => "Добивание своих",
        "lasthit" => "Последний удар",
        "level" => "Новый уровень",
        "pain" => "Боль",
        "ally" => "Союзники",
        "item" => "Предметы",
        "purch" => "Покупка",
        "rival" => "Соперники",
        "win" => "Победа",
        "lose" => "Поражение",
        "cast" => "Применение",
        "notyet" => "Перезарядка",
        "nomana" => "Нет маны",
        "thanks" => "Благодарность",
        "anger" => "Злость",
        "happy" => "Радость",
        "haste" => "Руна ускорения",
        "regen" => "Руна регенерации",
        "invis" => "Руна невидимости",
        "illus" => "Руна иллюзий",
        "doubdam" => "Руна двойного урона",
        "bottle" => "Bottle",
        "blink" => "Blink Dagger",
        "scepter" => "Aghanim's Scepter",
        "immort" => "Aegis",
        "fastres" => "Быстрое возрождение",
        "firstblood" => "Первая кровь",
        "battlebegins" => "Начало битвы",
        "underattack" => "Под атакой",
        _ => return category.to_owned(),
    };
    label.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories() {
        assert_eq!(category("bount_ability_track_kill_03", "bount_"), "ability_track_kill");
        assert_eq!(category("bount_kill_01", "bount_"), "kill");
        assert_eq!(category("bount_firstblood", "bount_"), "firstblood");
        assert_eq!(category("bount_rival_12b", "bount_"), "rival");
        assert_eq!(category("auto_axe_ability_berserk_02", "axe_"), "auto/ability_berserk");
        assert_eq!(category("jung_kill_01", "axe_"), "jung/kill");
        assert_eq!(category_label("auto/kill"), "Вторая версия · Убийства");

        let names: Vec<String> =
            ["auto_axe_kill_01", "auto_axe_kill_02", "axe_kill_01", "axe_kill_02", "jung_kill_01"].map(String::from).into();
        assert_eq!(main_prefix(&names), "axe_");
    }

    #[test]
    fn locates_export_next_to_dump() {
        let (res, root) = locate(Path::new("C:/x/example/sounds/vo/bounty_hunter"));
        assert_eq!(res, "sounds/vo/bounty_hunter");
        assert_eq!(root, Path::new("C:/x/example/export"));

        let (res, root) = locate(Path::new("C:/dl/Axe"));
        assert_eq!(res, "sounds/vo/axe");
        assert_eq!(root, Path::new("C:/dl/export"));
    }
}
