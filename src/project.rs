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
use crate::voice::{self, Part, Skin, Variant};
use crate::{tr, trf};

pub const AUDIO_EXTENSIONS: [&str; 8] = ["mp3", "wav", "ogg", "oga", "opus", "flac", "m4a", "aac"];
const STATE_FILE: &str = "dota_revoice.json";

#[derive(Debug, Clone)]
pub struct Line {
    /// Имя файла без расширения: `bount_kill_01`.
    pub name: String,
    /// `kill`, `ability_track`… — для группировки; у звуков — способность или предмет.
    /// Фразы арканы или набора — в своём разделе: `jung_axe/kill`.
    pub category: String,
    pub original: PathBuf,
    /// Фраза или звук арканы или набора, а не базовые.
    pub skin: Option<Skin>,
    /// Как показать в списке: без начала героя (`kill_01` вместо `bount_kill_01`).
    pub short: String,
}

/// Какие фразы папки героя открыть.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum View {
    /// Все (своя папка): личность и автоматон — разделами.
    #[default]
    All,
    /// Основной герой с арканами и наборами.
    Hero,
    /// Только личность или только автоматон.
    Variant(Variant),
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
    /// Фразы из пачек друзей: от кого и какая это версия.
    #[serde(default)]
    origins: BTreeMap<String, Origin>,
}

/// Откуда моя версия фразы, если она пришла из пачки друга.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// Кто записал (подпись в пачке; пустая — без подписи).
    pub author: String,
    /// Отпечаток mp3 в пачке — по нему узнаём ту же версию при повторном импорте.
    pub hash: String,
    /// Отпечаток моего mp3 сразу после импорта: совпадает с нынешним — я фразу не менял.
    pub local: String,
}

/// Прежний дубль фразы: исходник и то, как он был обрезан и обработан.
#[derive(Debug, Clone)]
pub struct Take {
    pub id: u32,
    pub path: PathBuf,
    pub saved: Option<SystemTime>,
    pub secs: f32,
    /// Дубль из пачки друга — чья это версия.
    pub origin: Option<Origin>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TakeMeta {
    pub trim: Option<[f32; 2]>,
    pub fx: crate::process::Settings,
    pub origin: Option<Origin>,
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

    pub fn origin(&self, res: &str) -> Option<&Origin> {
        self.state.origins.get(res)
    }

    pub fn set_origin(&mut self, res: &str, origin: Option<Origin>) -> Result<()> {
        let changed = match origin {
            Some(o) => self.state.origins.insert(res.to_owned(), o.clone()) != Some(o),
            None => self.state.origins.remove(res).is_some(),
        };
        self.save_if(changed)
    }

    /// Отпечаток моего mp3 сейчас; `None` — фразы нет.
    pub fn mp3_hash(&self, res: &str) -> Option<String> {
        std::fs::read(self.mp3(res)).ok().map(|b| crate::exchange::content_hash(&b))
    }

    /// Чужая фраза пересобрана без моих правок (вернули дубль, отменили импорт) —
    /// запоминаем её новый mp3 как «нетронутый».
    pub fn refresh_origin_local(&mut self, res: &str) -> Result<()> {
        let (Some(mut origin), Some(local)) = (self.origin(res).cloned(), self.mp3_hash(res)) else { return Ok(()) };
        origin.local = local;
        self.set_origin(res, Some(origin))
    }

    /// Кладёт запись сразу в дубли, не трогая текущую версию (чужая версия при конфликте).
    /// Возвращает номер дубля.
    pub fn add_take(&mut self, res: &str, samples: &[f32], meta: &TakeMeta) -> Result<u32> {
        let dir = self.takes_dir(res);
        std::fs::create_dir_all(&dir)?;
        let id = self.takes(res).first().map_or(1, |t| t.id + 1);
        std::fs::write(dir.join(format!("{id:03}.json")), serde_json::to_vec_pretty(meta)?)?;
        crate::encode::save_raw(&dir.join(format!("{id:03}.wav")), samples)?;
        Ok(id)
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
                let meta: Option<TakeMeta> = std::fs::read(path.with_extension("json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
                Some(Take { id, path, saved, secs, origin: meta.and_then(|m| m.origin) })
            })
            .collect();
        takes.sort_by_key(|t| std::cmp::Reverse(t.id));
        takes
    }

    /// Убирает текущий исходник в дубли (вместе с обрезкой, обработкой и тем, от кого он).
    /// Возвращает номер дубля; `None` — исходника не было.
    pub fn archive_current(&mut self, res: &str) -> Result<Option<u32>> {
        let raw = self.raw(res);
        if !raw.is_file() {
            return Ok(None);
        }
        let dir = self.takes_dir(res);
        std::fs::create_dir_all(&dir)?;
        let id = self.takes(res).first().map_or(1, |t| t.id + 1);
        let meta = TakeMeta { trim: self.trim(res).map(|(a, b)| [a, b]), fx: self.processing(res), origin: self.origin(res).cloned() };
        std::fs::write(dir.join(format!("{id:03}.json")), serde_json::to_vec_pretty(&meta)?)?;
        std::fs::rename(&raw, dir.join(format!("{id:03}.wav"))).with_context(|| trf!("не перенёсся в дубли {}", "couldn't move to takes: {}", raw.display()))?;
        self.set_origin(res, None)?;
        Ok(Some(id))
    }

    /// Возвращает дубль `id` в работу, текущий исходник уходит в дубли (его номер — в ответе).
    /// mp3 для игры после этого нужно пересобрать.
    pub fn restore_take(&mut self, res: &str, id: u32) -> Result<Option<u32>> {
        let dir = self.takes_dir(res);
        let (wav, json) = (dir.join(format!("{id:03}.wav")), dir.join(format!("{id:03}.json")));
        ensure!(wav.is_file(), trf!("нет дубля №{id}", "no take #{id}"));
        let meta: TakeMeta = std::fs::read(&json).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let archived = self.archive_current(res)?;
        let raw = self.raw(res);
        if let Some(parent) = raw.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&wav, &raw).with_context(|| trf!("не вернулся дубль {}", "couldn't restore take {}", wav.display()))?;
        let _ = std::fs::remove_file(json);
        self.set_trim(res, meta.trim.map(|[a, b]| (a, b)))?;
        self.set_processing(res, meta.fx)?;
        self.set_origin(res, meta.origin)?;
        Ok(archived)
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
                    .with_context(|| trf!("не перенёсся в корзину {}", "couldn't move to trash: {}", src.display()))?;
            }
        }
        self.set_trim(res, None)?;
        self.set_processing(res, crate::process::Settings::default())?;
        self.set_origin(res, None)?;
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
    /// Чьи фразы открыты (у звуков способностей — чьи звуки).
    pub view: View,
    pub lines: Vec<Line>,
    recorded: HashSet<String>,
    store: Store,
    /// Группы звуков способностей или предметов (`apply_sound_set`): подпись, иконка, порядок.
    groups: Vec<crate::sfx::SoundGroup>,
}

impl Project {
    /// Свои фразы — в `export` рядом с выгрузкой (как было до настроек папок).
    pub fn open(source_dir: &Path) -> Result<Self> {
        Self::open_in(source_dir, &legacy_export_root(source_dir))
    }

    /// Свои фразы — в `export_root` (папка «Мои фразы» из настроек). Все фразы папки.
    pub fn open_in(source_dir: &Path, export_root: &Path) -> Result<Self> {
        Self::open_view(source_dir, None, export_root, View::All)
    }

    /// Фразы героя, его личности или автоматона (`view`). `english_dir` — те же фразы
    /// в английской озвучке: оттуда берутся арканы и наборы, которых нет в этом языке.
    pub fn open_view(source_dir: &Path, english_dir: Option<&Path>, export_root: &Path, view: View) -> Result<Self> {
        let source_dir = source_dir.to_path_buf();
        let mut files = audio_files(&source_dir)?;
        ensure!(!files.is_empty(), trf!("в папке {} нет звуков (mp3, wav, ogg…)", "no sounds in folder {} (mp3, wav, ogg…)", source_dir.display()));

        let (resource_dir, _) = locate(&source_dir);
        let export_root = export_root.to_path_buf();
        let hero = resource_dir.rsplit('/').next().unwrap_or_default().to_owned();
        // Разбор по личностям, автоматонам и арканам — только у фраз героя.
        let folder = resource_dir.strip_prefix("sounds/vo/").filter(|f| !f.contains('/')).map(str::to_owned);
        if let (Some(folder), Some(english)) = (&folder, english_dir.filter(|d| *d != source_dir))
            && view == View::Hero
        {
            let have: HashSet<String> = files.iter().map(|p| file_stem(p)).collect();
            let skins = audio_files(english).unwrap_or_default().into_iter().filter(|p| {
                let name = file_stem(p);
                !have.contains(&name) && matches!(voice::split(folder, &name).0, Part::Skin { .. })
            });
            files.extend(skins.collect::<Vec<_>>());
        }
        files.sort_by_key(|p| file_stem(p));

        let parts: Vec<(PathBuf, String, Part)> = files
            .into_iter()
            .map(|p| {
                let name = file_stem(&p);
                let part = folder.as_deref().map_or(Part::Hero, |f| voice::split(f, &name).0);
                (p, name, part)
            })
            // Звуки способностей делит по карточкам `apply_sound_set`, здесь — только фразы.
            .filter(|(_, _, part)| match (view, part) {
                _ if folder.is_none() => true,
                (View::All, _) => true,
                (View::Hero, Part::Variant(_)) => false,
                (View::Hero, _) => true,
                (View::Variant(v), Part::Variant(w)) => v == *w,
                (View::Variant(_), _) => false,
            })
            .collect();
        let hero_names: Vec<String> = parts.iter().filter(|(_, _, part)| *part == Part::Hero).map(|(_, n, _)| n.clone()).collect();
        let prefix = main_prefix(&hero_names);
        let lines = parts
            .into_iter()
            .map(|(original, name, part)| {
                let rest = folder.as_deref().map_or(name.as_str(), |f| voice::split(f, &name).1).to_owned();
                let (category, skin, short) = match part {
                    // У звуков способностей и предметов имя показываем целиком.
                    Part::Hero if folder.is_none() => (category(&name, &prefix), None, name.clone()),
                    Part::Hero => (category(&name, &prefix), None, strip_hero(&name, &prefix).to_owned()),
                    Part::Skin { skin, section } => (format!("{section}/{}", without_number(&rest)), Some(skin), rest),
                    Part::Variant(v) => {
                        // У личности бывают подразделы (дракон Dragon Knight), во «всех» — раздел самой личности.
                        let section = match view {
                            View::All => Some(v.key()),
                            _ => folder.as_deref().and_then(|f| voice::persona_section(f, &name)),
                        };
                        let base = without_number(&rest);
                        (section.map_or_else(|| base.to_owned(), |s| format!("{s}/{base}")), None, rest)
                    }
                };
                Line { category, original, skin, short, name }
            })
            .collect::<Vec<_>>();
        ensure!(!lines.is_empty(), trf!("в папке {} нет фраз этой карточки", "no lines for this card in folder {}", source_dir.display()));

        let store = Store::open(&export_root);
        let mut project = Self { source_dir, export_root, resource_dir, hero, view, lines, recorded: HashSet::new(), store, groups: Vec::new() };
        project.rescan();
        Ok(project)
    }

    /// Звуки способностей или предметов (а не фразы): папка не из `sounds/vo/`.
    pub fn is_sounds(&self) -> bool {
        !self.resource_dir.starts_with("sounds/vo/")
    }

    /// Раскладывает звуки по способностям или предметам из каталога игры. Звук, которого
    /// в каталоге нет (своя папка, новый патч), — в «Прочее». `view` — чьи звуки оставить:
    /// у личности и автоматона свои (`blink_in_persona`), остальные — у основного героя.
    pub fn apply_sound_set(&mut self, set: &crate::sfx::SoundSet, view: View) {
        let variant = |name: &str| set.file(name).and_then(|f| f.variant);
        self.lines.retain(|l| match view {
            View::All => true,
            View::Hero => variant(&l.name).is_none(),
            View::Variant(v) => variant(&l.name) == Some(v),
        });
        self.view = view;
        for line in &mut self.lines {
            line.short = line.name.clone();
            match set.file(&line.name) {
                Some(f) => {
                    line.category = f.group.clone();
                    line.skin = f.skin.then_some(Skin::Set);
                }
                None => {
                    line.category = crate::sfx::OTHER.to_owned();
                    line.skin = None;
                }
            }
        }
        self.rescan();
        self.groups = set.groups.clone();
        if !self.groups.iter().any(|g| g.key == crate::sfx::OTHER) && self.lines.iter().any(|l| l.category == crate::sfx::OTHER) {
            self.groups.push(crate::sfx::SoundGroup { key: crate::sfx::OTHER.to_owned(), label: tr!("Прочее", "Other").to_owned(), icon: None });
        }
    }

    /// Подпись группы: способность или предмет, у фраз — категория (с разделом:
    /// «Набор Bare Knuckle · Убийства»).
    pub fn category_label(&self, category: &str) -> String {
        match self.groups.iter().find(|g| g.key == category) {
            Some(g) => g.label.clone(),
            None => category_label(category),
        }
    }

    /// Подпись группы в списке: раздел там — свой заголовок, здесь только категория.
    pub fn group_label(&self, category: &str) -> String {
        match category.split_once('/') {
            Some((_, base)) if self.groups.is_empty() => category_label(base),
            _ => self.category_label(category),
        }
    }

    /// Иконка группы в кеше (способности и предметы).
    pub fn group_icon(&self, category: &str) -> Option<&str> {
        self.groups.iter().find(|g| g.key == category)?.icon.as_deref()
    }

    /// Порядок групп: у звуков — как в каталоге (Q, W, E, R…), у фраз основные — раньше вторых версий.
    pub fn group_rank(&self, category: &str) -> usize {
        match self.groups.iter().position(|g| g.key == category) {
            Some(i) => i,
            None if self.groups.is_empty() => usize::from(category.contains('/')),
            None => self.groups.len(),
        }
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
        self.store.archive_current(&res).map(|_| ())
    }

    /// Возвращает дубль `id` в работу, текущий исходник уходит в дубли.
    pub fn restore_take(&mut self, line: &Line, id: u32) -> Result<()> {
        let res = self.resource_path(line);
        self.store.restore_take(&res, id).map(|_| ())
    }

    /// От кого моя версия (если из пачки) и правил ли я её после импорта.
    pub fn origin(&self, line: &Line) -> Option<(Origin, bool)> {
        let res = self.resource_path(line);
        let origin = self.store.origin(&res)?.clone();
        let edited = self.store.mp3_hash(&res).is_some_and(|h| h != origin.local);
        Some((origin, edited))
    }

    pub fn refresh_origin_local(&mut self, line: &Line) -> Result<()> {
        let res = self.resource_path(line);
        self.store.refresh_origin_local(&res)
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

/// Звуки и фразы папки, по имени.
fn audio_files(dir: &Path) -> Result<Vec<PathBuf>> {
    Ok(std::fs::read_dir(dir)
        .with_context(|| trf!("не открывается папка {}", "can't open folder {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && has_audio_ext(p))
        .collect())
}

/// Префикс героя (`bount_`): самый частый первый кусок имени.
fn main_prefix(names: &[String]) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for n in names {
        if let Some((p, _)) = n.split_once('_') {
            *counts.entry(p).or_default() += 1;
        }
    }
    counts.into_iter().max_by_key(|&(p, n)| (n, std::cmp::Reverse(p))).map(|(p, _)| format!("{p}_")).unwrap_or_default()
}

/// Фраза без начала героя. Базовые фразы бывают и с другим началом (`dragon_` рядом
/// с `drag_` у Dragon Knight) — у них снимается их первое слово.
fn strip_hero<'a>(name: &'a str, prefix: &str) -> &'a str {
    if prefix.is_empty() {
        return name;
    }
    name.strip_prefix(prefix).or_else(|| name.split_once('_').map(|(_, r)| r)).unwrap_or(name)
}

/// Хвост вида `_01`, `_12b` или `_03_02` — номер варианта, в категорию не входит.
fn without_number(rest: &str) -> &str {
    match rest.rsplit_once('_') {
        Some((head, tail)) if tail.starts_with(|c: char| c.is_ascii_digit()) => without_number(head),
        _ => rest,
    }
}

/// `bount_ability_track_kill_03` → `ability_track_kill`, `dragon_kill_01` → `kill`.
fn category(name: &str, prefix: &str) -> String {
    without_number(strip_hero(name, prefix)).to_owned()
}

/// Раздел группы фраз: `jung_axe/kill` → `jung_axe`.
pub fn section_of(category: &str) -> Option<&str> {
    category.split_once('/').map(|(s, _)| s)
}

/// Человеческое название категории.
pub fn category_label(category: &str) -> String {
    if let Some((section, base)) = category.split_once('/') {
        return format!("{} · {}", voice::section_label(section), category_label(base));
    }
    if let Some(ability) = category.strip_prefix("ability_") {
        return trf!("Способность: {ability}", "Ability: {ability}");
    }
    let label = match category {
        "attack" => tr!("Атака", "Attack"),
        "kill" => tr!("Убийства", "Kills"),
        "move" => tr!("Движение", "Movement"),
        "death" => tr!("Смерть", "Death"),
        "respawn" => tr!("Возрождение", "Respawn"),
        "spawn" => tr!("Появление", "Spawn"),
        "laugh" => tr!("Смех", "Laugh"),
        "deny" => tr!("Добивание своих", "Deny"),
        "lasthit" => tr!("Последний удар", "Last hit"),
        "level" => tr!("Новый уровень", "Level up"),
        "pain" => tr!("Боль", "Pain"),
        "ally" => tr!("Союзники", "Allies"),
        "item" => tr!("Предметы", "Items"),
        "purch" => tr!("Покупка", "Purchase"),
        "rival" => tr!("Соперники", "Rivals"),
        "win" => tr!("Победа", "Victory"),
        "lose" => tr!("Поражение", "Defeat"),
        "cast" => tr!("Применение", "Cast"),
        "notyet" => tr!("Перезарядка", "Cooldown"),
        "nomana" => tr!("Нет маны", "No mana"),
        "thanks" => tr!("Благодарность", "Thanks"),
        "anger" => tr!("Злость", "Anger"),
        "happy" => tr!("Радость", "Joy"),
        "haste" => tr!("Руна ускорения", "Haste rune"),
        "regen" => tr!("Руна регенерации", "Regeneration rune"),
        "invis" => tr!("Руна невидимости", "Invisibility rune"),
        "illus" => tr!("Руна иллюзий", "Illusion rune"),
        "doubdam" => tr!("Руна двойного урона", "Double damage rune"),
        "bottle" => "Bottle",
        "blink" => "Blink Dagger",
        "scepter" => "Aghanim's Scepter",
        "immort" => "Aegis",
        "fastres" => tr!("Быстрое возрождение", "Fast respawn"),
        "firstblood" => tr!("Первая кровь", "First blood"),
        "battlebegins" => tr!("Начало битвы", "Battle begins"),
        "underattack" => tr!("Под атакой", "Under attack"),
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
        assert_eq!(category("dk_ability_failure_03_02", "dk_"), "ability_failure");
        // Базовая фраза с другим началом — та же категория.
        assert_eq!(category("dragon_kill_01", "drag_"), "kill");
        assert_eq!(category_label("jung_axe/kill"), "Набор Bare Knuckle · Убийства");

        let names: Vec<String> = ["axe_kill_01", "axe_kill_02", "jung_kill_01"].map(String::from).into();
        assert_eq!(main_prefix(&names), "axe_");
    }

    #[test]
    fn hero_persona_automaton_and_skins() {
        let root = std::env::temp_dir().join(format!("dota_revoice_views_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ru = root.join("russian/sounds/vo/axe");
        let en = root.join("english/sounds/vo/axe");
        std::fs::create_dir_all(&ru).unwrap();
        std::fs::create_dir_all(&en).unwrap();
        for n in ["axe_kill_01", "axe_kill_02"] {
            std::fs::write(ru.join(format!("{n}.mp3")), b"x").unwrap();
        }
        for n in ["axe_kill_01", "axe_kill_02", "auto_axe_kill_01", "jung_axe_kill_01", "jung_axe_kill_02"] {
            std::fs::write(en.join(format!("{n}.mp3")), b"x").unwrap();
        }
        let export = root.join("export");
        let brief = |p: &Project| p.lines.iter().map(|l| (l.name.clone(), l.category.clone(), l.short.clone(), l.skin.is_some())).collect::<Vec<_>>();
        let s = |a: &str, b: &str, c: &str, d: bool| (a.to_owned(), b.to_owned(), c.to_owned(), d);

        // Герой на своём языке + набор из английской озвучки.
        let hero = Project::open_view(&ru, Some(&en), &export, View::Hero).unwrap();
        assert_eq!(
            brief(&hero),
            [
                s("axe_kill_01", "kill", "kill_01", false),
                s("axe_kill_02", "kill", "kill_02", false),
                s("jung_axe_kill_01", "jung_axe/kill", "kill_01", true),
                s("jung_axe_kill_02", "jung_axe/kill", "kill_02", true),
            ]
        );
        assert_eq!(hero.group_label("jung_axe/kill"), "Убийства");
        assert_eq!(hero.lines[2].original, en.join("jung_axe_kill_01.mp3"));

        let auto = Project::open_view(&en, None, &export, View::Variant(Variant::Automaton)).unwrap();
        assert_eq!(brief(&auto), [s("auto_axe_kill_01", "kill", "kill_01", false)]);
        // Своя папка — всё вместе, автоматон отдельным разделом.
        let all = Project::open_in(&en, &export).unwrap();
        assert_eq!(all.lines.len(), 5);
        assert!(all.lines.iter().any(|l| l.category == "automaton/kill"));
        std::fs::remove_dir_all(root).unwrap();
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
