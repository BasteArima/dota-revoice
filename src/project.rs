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
//! example/export/dota_revoice.json                          ← пометки «переделать»
//! ```

use std::collections::{BTreeMap, BTreeSet, HashSet};
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

pub struct Project {
    pub source_dir: PathBuf,
    pub export_root: PathBuf,
    /// Каталог внутри игры: `sounds/vo/bounty_hunter`.
    pub resource_dir: String,
    pub hero: String,
    pub lines: Vec<Line>,
    recorded: HashSet<String>,
    state: State,
}

impl Project {
    pub fn open(source_dir: &Path) -> Result<Self> {
        let source_dir = source_dir.to_path_buf();
        let mut files: Vec<PathBuf> = std::fs::read_dir(&source_dir)
            .with_context(|| format!("не открывается папка {}", source_dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && has_audio_ext(p))
            .collect();
        ensure!(!files.is_empty(), "в папке {} нет звуков (mp3, wav, ogg…)", source_dir.display());
        files.sort();

        let (resource_dir, export_root) = locate(&source_dir);
        let hero = resource_dir.rsplit('/').next().unwrap_or_default().to_owned();

        let names: Vec<String> = files.iter().map(|p| file_stem(p)).collect();
        let prefix = common_prefix(&names);
        let lines = files
            .into_iter()
            .zip(names)
            .map(|(original, name)| Line { category: category(&name, &prefix), name, original })
            .collect();

        let state = std::fs::read(export_root.join(STATE_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();

        let mut project =
            Self { source_dir, export_root, resource_dir, hero, lines, recorded: HashSet::new(), state };
        project.rescan();
        Ok(project)
    }

    /// Перечитать, какие фразы уже записаны.
    pub fn rescan(&mut self) {
        self.recorded = self
            .lines
            .iter()
            .filter(|l| self.export_mp3(l).is_file())
            .map(|l| l.name.clone())
            .collect();
    }

    pub fn resource_path(&self, line: &Line) -> String {
        format!("{}/{}", self.resource_dir, line.name)
    }

    /// Моя фраза: то же имя, что у оригинала.
    pub fn export_mp3(&self, line: &Line) -> PathBuf {
        self.export_root.join(&self.resource_dir).join(format!("{}.mp3", line.name))
    }

    pub fn raw_wav(&self, line: &Line) -> PathBuf {
        self.export_root.join(".raw").join(&self.resource_dir).join(format!("{}.wav", line.name))
    }

    pub fn status(&self, line: &Line) -> Status {
        if self.state.redo.contains(&self.resource_path(line)) {
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
        let key = self.resource_path(line);
        let changed = if redo { self.state.redo.insert(key) } else { self.state.redo.remove(&key) };
        if changed { self.save_state() } else { Ok(()) }
    }

    /// Обрезка фразы в секундах; `None` — фраза целиком.
    pub fn trim(&self, line: &Line) -> Option<(f32, f32)> {
        self.state.trims.get(&self.resource_path(line)).map(|&[a, b]| (a, b))
    }

    pub fn set_trim(&mut self, line: &Line, trim: Option<(f32, f32)>) -> Result<()> {
        let key = self.resource_path(line);
        let changed = match trim {
            Some((a, b)) => self.state.trims.insert(key, [a, b]) != Some([a, b]),
            None => self.state.trims.remove(&key).is_some(),
        };
        if changed { self.save_state() } else { Ok(()) }
    }

    /// Шумодав и усиление для фразы.
    pub fn processing(&self, line: &Line) -> crate::process::Settings {
        let key = self.resource_path(line);
        crate::process::Settings {
            denoise: self.state.denoise.contains(&key),
            gain_db: self.state.gains.get(&key).copied().unwrap_or(0.0),
            pitch: self.state.pitches.get(&key).copied().unwrap_or(0.0),
        }
    }

    pub fn set_processing(&mut self, line: &Line, s: crate::process::Settings) -> Result<()> {
        let key = self.resource_path(line);
        let denoise_changed = if s.denoise { self.state.denoise.insert(key.clone()) } else { self.state.denoise.remove(&key) };
        let set = |map: &mut BTreeMap<String, f32>, v: f32| {
            if v.abs() < 0.05 { map.remove(&key).is_some() } else { map.insert(key.clone(), v) != Some(v) }
        };
        let gain_changed = set(&mut self.state.gains, s.gain_db);
        let pitch_changed = set(&mut self.state.pitches, s.pitch);
        if denoise_changed || gain_changed || pitch_changed { self.save_state() } else { Ok(()) }
    }

    /// Папка прежних дублей фразы: `export/.takes/sounds/vo/hero/name/`.
    fn takes_dir(&self, line: &Line) -> PathBuf {
        self.export_root.join(".takes").join(&self.resource_dir).join(&line.name)
    }

    /// Прежние дубли, новые первыми.
    pub fn takes(&self, line: &Line) -> Vec<Take> {
        let Ok(entries) = std::fs::read_dir(self.takes_dir(line)) else { return Vec::new() };
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
    pub fn archive_current(&mut self, line: &Line) -> Result<()> {
        let raw = self.raw_wav(line);
        if !raw.is_file() {
            return Ok(());
        }
        let dir = self.takes_dir(line);
        std::fs::create_dir_all(&dir)?;
        let id = self.takes(line).first().map_or(1, |t| t.id + 1);
        let meta = TakeMeta { trim: self.trim(line).map(|(a, b)| [a, b]), fx: self.processing(line) };
        std::fs::write(dir.join(format!("{id:03}.json")), serde_json::to_vec_pretty(&meta)?)?;
        std::fs::rename(&raw, dir.join(format!("{id:03}.wav"))).with_context(|| format!("не перенёсся в дубли {}", raw.display()))?;
        Ok(())
    }

    /// Возвращает дубль `id` в работу, текущий исходник уходит в дубли.
    /// mp3 для игры после этого нужно пересобрать.
    pub fn restore_take(&mut self, line: &Line, id: u32) -> Result<()> {
        let dir = self.takes_dir(line);
        let (wav, json) = (dir.join(format!("{id:03}.wav")), dir.join(format!("{id:03}.json")));
        ensure!(wav.is_file(), "нет дубля №{id}");
        let meta: TakeMeta = std::fs::read(&json).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        self.archive_current(line)?;
        let raw = self.raw_wav(line);
        if let Some(parent) = raw.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&wav, &raw).with_context(|| format!("не вернулся дубль {}", wav.display()))?;
        let _ = std::fs::remove_file(json);
        self.set_trim(line, meta.trim.map(|[a, b]| (a, b)))?;
        self.set_processing(line, meta.fx)
    }

    /// Дубль — в корзину.
    pub fn trash_take(&mut self, line: &Line, id: u32) -> Result<()> {
        let dir = self.takes_dir(line);
        let trash = self.export_root.join(".trash").join(&self.resource_dir);
        std::fs::create_dir_all(&trash)?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        for ext in ["wav", "json"] {
            let src = dir.join(format!("{id:03}.{ext}"));
            if src.is_file() {
                std::fs::rename(&src, trash.join(format!("{stamp}_{}_take{id:03}.{ext}", line.name)))?;
            }
        }
        Ok(())
    }

    pub fn note(&self, line: &Line) -> &str {
        self.state.notes.get(&self.resource_path(line)).map_or("", String::as_str)
    }

    /// Пустая заметка удаляется.
    pub fn set_note(&mut self, line: &Line, note: &str) -> Result<()> {
        let key = self.resource_path(line);
        let note = note.trim();
        let changed = if note.is_empty() {
            self.state.notes.remove(&key).is_some()
        } else {
            self.state.notes.insert(key, note.to_owned()).as_deref() != Some(note)
        };
        if changed { self.save_state() } else { Ok(()) }
    }

    fn save_state(&self) -> Result<()> {
        std::fs::create_dir_all(&self.export_root)?;
        std::fs::write(self.export_root.join(STATE_FILE), serde_json::to_vec_pretty(&self.state)?)?;
        Ok(())
    }

    /// Переносит мою фразу (и её wav) в `export/.trash`, ничего не удаляя насовсем.
    pub fn trash(&mut self, line: &Line) -> Result<()> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let trash_dir = self.export_root.join(".trash").join(&self.resource_dir);
        std::fs::create_dir_all(&trash_dir)?;
        for src in [self.export_mp3(line), self.raw_wav(line)] {
            if src.is_file() {
                let name = src.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                std::fs::rename(&src, trash_dir.join(format!("{stamp}_{name}")))
                    .with_context(|| format!("не перенёсся в корзину {}", src.display()))?;
            }
        }
        self.recorded.remove(&line.name);
        self.set_trim(line, None)?;
        self.set_processing(line, crate::process::Settings::default())?;
        self.set_redo(line, false)
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

/// Префикс героя (`bount_`), если он общий у всех файлов.
fn common_prefix(names: &[String]) -> String {
    let first = names.first().and_then(|n| n.split_once('_')).map(|(p, _)| format!("{p}_"));
    match first {
        Some(p) if names.iter().all(|n| n.starts_with(&p)) => p,
        _ => String::new(),
    }
}

/// `bount_ability_track_kill_03` → `ability_track_kill`.
fn category(name: &str, prefix: &str) -> String {
    let rest = name.strip_prefix(prefix).unwrap_or(name);
    // Хвост вида `_01` или `_12b` — номер варианта, в категорию не входит.
    match rest.rsplit_once('_') {
        Some((head, tail)) if tail.starts_with(|c: char| c.is_ascii_digit()) => head.to_owned(),
        _ => rest.to_owned(),
    }
}

/// Человеческое название категории.
pub fn category_label(category: &str) -> String {
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
