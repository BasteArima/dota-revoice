//! Обмен фразами с друзьями: «пачка» — zip с готовыми mp3 и заметками.
//!
//! ```text
//! dota_revoice_pack.json                         ← формат, подпись автора, заметки, отпечатки фраз
//! sounds/vo/bounty_hunter/bount_kill_01.mp3      ← фраза, путь как в игре
//! .raw/sounds/vo/bounty_hunter/bount_kill_01.wav ← исходник для правок (если положили)
//! ```
//!
//! Импорт идёт в два шага: `preview` раскладывает пачку на новые фразы, совпадающие,
//! обновления от того же автора и конфликты, а `apply` выполняет решения пользователя.
//! Ничего не теряется: заменённая версия уходит в дубли, а при «оставить мою» в дубли
//! уходит версия из пачки. Откуда фраза, помнит `Store::origin`; что сделал последний
//! импорт — журнал `.imports/last.json`, по нему импорт можно отменить.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use crate::process::{self, Settings};
use crate::project::{Origin, Store, TakeMeta};
use crate::{audio, encode, mp3, pack};
use crate::{tr, trf};

const MANIFEST: &str = "dota_revoice_pack.json";
/// Исходники в пачке: `.raw/<путь ресурса>.wav`.
const RAW_PREFIX: &str = ".raw/";
/// 1 — заметки; 2 — подпись автора, отпечатки фраз, исходники по желанию.
const FORMAT: u32 = 2;
const JOURNAL: &str = ".imports/last.json";

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Manifest {
    format: u32,
    app_version: String,
    created_unix: u64,
    /// Подпись того, кто собрал пачку.
    author: String,
    /// Путь ресурса → заметка («покупка BKB»).
    notes: BTreeMap<String, String>,
    /// Путь ресурса → чья это версия и её отпечаток.
    lines: BTreeMap<String, LineMeta>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
struct LineMeta {
    /// Кто записал эту версию (может отличаться от того, кто собрал пачку).
    author: String,
    hash: String,
    /// Обрезка и обработка исходника — если он лежит в пачке.
    trim: Option<[f32; 2]>,
    fx: Option<Settings>,
}

/// Отпечаток mp3 — одинаковые файлы узнаём при повторном импорте.
pub fn content_hash(bytes: &[u8]) -> String {
    format!("{:x}", Md5::digest(bytes))
}

/// Группа фразы — её папка: `sounds/vo/axe/axe_kill_01` → `sounds/vo/axe` (герой).
pub fn group_of(res: &str) -> &str {
    res.rsplit_once('/').map_or("", |(g, _)| g)
}

/// Сколько моих фраз в каждой группе (у героя) — для окна экспорта.
pub fn recorded_groups(export_root: &Path) -> Result<BTreeMap<String, usize>> {
    let mut groups = BTreeMap::new();
    for res in pack::collect_export(export_root)?.keys() {
        *groups.entry(group_of(res).to_owned()).or_default() += 1;
    }
    Ok(groups)
}

/// Собирает мои фразы в пачку: все или только из `groups`. С `sources` кладёт и исходники
/// (wav, обрезку, обработку) — друг сможет править без потерь. Возвращает число фраз.
pub fn export_pack(export_root: &Path, out: &Path, author: &str, groups: Option<&BTreeSet<String>>, sources: bool) -> Result<usize> {
    let lines: BTreeMap<_, _> = pack::collect_export(export_root)?
        .into_iter()
        .filter(|(res, _)| groups.is_none_or(|g| g.contains(group_of(res))))
        .collect();
    ensure!(!lines.is_empty(), tr!("пока нечего отправлять — нет ни одной записанной фразы", "nothing to send yet — no recorded lines"));
    let store = Store::open(export_root);
    let mut manifest = Manifest {
        format: FORMAT,
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        created_unix: now_unix(),
        author: author.trim().to_owned(),
        notes: lines.keys().filter_map(|res| Some((res.clone(), store.notes().get(res)?.clone()))).collect(),
        lines: BTreeMap::new(),
    };
    let mut data = Vec::with_capacity(lines.len());
    for (res, path) in &lines {
        let bytes = std::fs::read(path).with_context(|| trf!("не читается {}", "can't read {}", path.display()))?;
        let hash = content_hash(&bytes);
        // Чужую фразу, которую я не менял, пересылаем под именем её автора.
        let line_author = match store.origin(res) {
            Some(o) if o.local == hash => o.author.clone(),
            _ => manifest.author.clone(),
        };
        let raw = store.raw(res);
        let raw = (sources && raw.is_file()).then_some(raw);
        let (trim, fx) = match raw {
            Some(_) => (store.trim(res).map(|(a, b)| [a, b]), Some(store.processing(res))),
            None => (None, None),
        };
        manifest.lines.insert(res.clone(), LineMeta { author: line_author, hash, trim, fx });
        data.push((res, bytes, raw));
    }

    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = File::create(out).with_context(|| trf!("не создаётся {}", "can't create {}", out.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    // mp3 уже сжат — кладём как есть; wav жмём.
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file(MANIFEST, stored)?;
    zip.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
    for (res, bytes, raw) in data {
        zip.start_file(format!("{res}.mp3"), stored)?;
        zip.write_all(&bytes)?;
        if let Some(raw) = raw {
            zip.start_file(format!("{RAW_PREFIX}{res}.wav"), deflated)?;
            zip.write_all(&std::fs::read(&raw).with_context(|| trf!("не читается {}", "can't read {}", raw.display()))?)?;
        }
    }
    zip.finish()?;
    Ok(lines.len())
}

/// Что за фраза пришла в пачке относительно моих.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// У меня этой фразы нет.
    New,
    /// Эта версия у меня уже есть: та же запись, она же с моими правками или в дублях.
    Same,
    /// У меня прежняя версия этого же автора, и я её не трогал.
    Update,
    /// Своя версия у обоих — решает пользователь.
    Conflict,
    /// Такой фразы нет в игре (опечатка в имени, звук из другого мода).
    Unknown,
}

pub struct Incoming {
    pub res: String,
    pub kind: Kind,
    /// Кто записал версию из пачки.
    pub author: String,
    pub hash: String,
    pub mp3: Vec<u8>,
    /// Исходник с обрезкой и обработкой — если автор положил его в пачку.
    pub raw: Option<Vec<u8>>,
    trim: Option<[f32; 2]>,
    fx: Option<Settings>,
}

/// Разобранная пачка: что в ней и как это ложится на мои фразы.
pub struct Preview {
    /// Подпись того, кто собрал пачку.
    pub author: String,
    pub created_unix: u64,
    /// Пачка из более новой версии программы — что-то в ней может быть непонятно.
    pub newer_format: bool,
    pub lines: Vec<Incoming>,
    /// Файлы, которые не похожи на фразы (не тот путь или битый mp3).
    pub rejected: Vec<String>,
    notes: BTreeMap<String, String>,
}

impl Preview {
    pub fn count(&self, kind: Kind) -> usize {
        self.lines.iter().filter(|l| l.kind == kind).count()
    }

    /// Сколько фраз пришло с исходниками.
    pub fn with_sources(&self) -> usize {
        self.lines.iter().filter(|l| l.raw.is_some()).count()
    }
}

/// Читает пачку и сравнивает с моими фразами, ничего не меняя.
/// `in_game` — есть ли такой звук в игре (путь ресурса без расширения).
pub fn preview(export_root: &Path, pack_path: &Path, in_game: &dyn Fn(&str) -> bool) -> Result<Preview> {
    let file = File::open(pack_path).with_context(|| trf!("не открывается {}", "can't open {}", pack_path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context(tr!("это не zip-архив", "this isn't a zip archive"))?;
    let store = Store::open(export_root);
    let mut manifest = Manifest::default();
    let mut files = Vec::new();
    let mut raws = BTreeMap::new();
    let mut rejected = Vec::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        if name == MANIFEST {
            manifest = serde_json::from_slice(&bytes).unwrap_or_default();
            continue;
        }
        if let Some(res) = name.strip_prefix(RAW_PREFIX).and_then(|n| resource_of(&n.replace(".wav", ".mp3"))) {
            raws.insert(res, bytes);
            continue;
        }
        match resource_of(&name) {
            Some(res) if mp3::parse(&bytes).is_ok() => files.push((res, bytes)),
            _ => rejected.push(name),
        }
    }

    let mut lines = Vec::with_capacity(files.len());
    for (res, mp3) in files {
        let hash = content_hash(&mp3);
        let meta = manifest.lines.get(&res).cloned().unwrap_or_default();
        // В пачках первой версии подписей нет — тогда автор фразы тот, кто собрал пачку.
        let author = if manifest.lines.contains_key(&res) { meta.author.clone() } else { manifest.author.clone() };
        let mine = store.mp3_hash(&res);
        let origin = store.origin(&res);
        let untouched = |o: &Origin| mine.as_deref() == Some(o.local.as_str());
        let in_takes = || store.takes(&res).iter().any(|t| t.origin.as_ref().is_some_and(|o| o.hash == hash));
        let kind = match &mine {
            _ if !in_game(&res) => Kind::Unknown,
            None => Kind::New,
            Some(m) if *m == hash => Kind::Same,
            // Эту версию я уже брал: правил я её после или нет — пачке нечего добавить.
            Some(_) if origin.is_some_and(|o| o.hash == hash) => Kind::Same,
            Some(_) if origin.is_some_and(|o| o.author == author && untouched(o)) => Kind::Update,
            Some(_) if in_takes() => Kind::Same,
            Some(_) => Kind::Conflict,
        };
        let raw = raws.remove(&res);
        let (trim, fx) = if raw.is_some() { (meta.trim, meta.fx) } else { (None, None) };
        lines.push(Incoming { res, kind, author, hash, mp3, raw, trim, fx });
    }
    lines.sort_by(|a, b| a.res.cmp(&b.res));
    Ok(Preview {
        author: manifest.author,
        created_unix: manifest.created_unix,
        newer_format: manifest.format > FORMAT,
        lines,
        rejected,
        notes: manifest.notes,
    })
}

/// Что сделать с фразой из пачки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Не трогать.
    Skip,
    /// Версия из пачки становится моей; моя прежняя (если была) — в дубли.
    Take,
    /// Моя остаётся, версия из пачки — в дубли: её можно послушать и вернуть.
    KeepMine,
}

/// Решение по умолчанию: новое и обновления берём, совпадения и «не из игры» — нет,
/// в конфликтах — как выбрано для всех сразу.
pub fn default_decision(kind: Kind, conflicts: Decision) -> Decision {
    match kind {
        Kind::New | Kind::Update => Decision::Take,
        Kind::Same | Kind::Unknown => Decision::Skip,
        Kind::Conflict => conflicts,
    }
}

#[derive(Debug, Default)]
pub struct ImportSummary {
    /// Новых фраз.
    pub added: usize,
    /// Заменено моих (прежние — в дублях).
    pub replaced: usize,
    /// Версий из пачки, положенных в дубли рядом с моими.
    pub to_takes: usize,
}

/// Что сделал импорт — чтобы его можно было отменить.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Journal {
    /// От кого была пачка.
    pub author: String,
    /// Когда импортировали.
    pub imported_unix: u64,
    pub steps: Vec<Step>,
}

#[derive(Serialize, Deserialize, Clone)]
pub enum Step {
    /// Новая фраза; `local` — отпечаток её mp3 после импорта.
    Added { res: String, local: String },
    /// Моя версия ушла в дубль `mine_take`, на её месте — из пачки.
    Replaced { res: String, local: String, mine_take: Option<u32> },
    /// Версия из пачки положена в дубль `take`.
    ToTakes { res: String, take: u32 },
    /// Заметка из пачки там, где своей не было.
    Note { res: String, note: String },
}

impl Journal {
    /// Сколько фраз затронул импорт.
    pub fn lines(&self) -> usize {
        self.steps.iter().filter(|s| !matches!(s, Step::Note { .. })).count()
    }
}

/// Выполняет решения по фразам пачки. Решение для фразы — `decide`.
/// Что сделано, записывается в журнал — последний импорт можно отменить.
pub fn apply(export_root: &Path, preview: &Preview, decide: impl Fn(&Incoming) -> Decision) -> Result<ImportSummary> {
    let mut store = Store::open(export_root);
    let mut summary = ImportSummary::default();
    let mut journal = Journal { author: preview.author.clone(), imported_unix: now_unix(), steps: Vec::new() };
    let result = (|| -> Result<()> {
        for line in &preview.lines {
            match decide(line) {
                Decision::Skip => {}
                Decision::KeepMine => {
                    let origin = Origin { author: line.author.clone(), hash: line.hash.clone(), local: String::new() };
                    let (samples, meta) = match &line.raw {
                        Some(raw) => (decode(raw, "wav")?, TakeMeta { trim: line.trim, fx: line.fx.unwrap_or_default(), origin: Some(origin) }),
                        None => (decode(&line.mp3, "mp3")?, TakeMeta { origin: Some(origin), ..Default::default() }),
                    };
                    let take = store.add_take(&line.res, &samples, &meta)?;
                    journal.steps.push(Step::ToTakes { res: line.res.clone(), take });
                    summary.to_takes += 1;
                }
                Decision::Take => {
                    let existed = store.mp3(&line.res).is_file();
                    let (mine_take, local) = take(&mut store, line)?;
                    if existed {
                        journal.steps.push(Step::Replaced { res: line.res.clone(), local, mine_take });
                        summary.replaced += 1;
                    } else {
                        journal.steps.push(Step::Added { res: line.res.clone(), local });
                        summary.added += 1;
                    }
                }
            }
        }
        // Заметки друга берём только туда, где своих нет.
        for (res, note) in &preview.notes {
            if store.note(res).is_empty() && store.mp3(res).is_file() {
                store.set_note(res, note)?;
                journal.steps.push(Step::Note { res: res.clone(), note: note.trim().to_owned() });
            }
        }
        Ok(())
    })();
    // Журнал пишем и при ошибке посередине — тогда отменить можно хотя бы сделанное.
    if !journal.steps.is_empty() {
        save_journal(export_root, &journal)?;
    }
    result.map(|()| summary)
}

/// Версия из пачки становится моей. Моя прежняя — в дубли (без исходника — из её mp3).
/// Возвращает номер дубля с моей прежней версией и отпечаток нового mp3.
fn take(store: &mut Store, line: &Incoming) -> Result<(Option<u32>, String)> {
    let (mp3_path, raw) = (store.mp3(&line.res), store.raw(&line.res));
    let mut mine_take = None;
    if mp3_path.is_file() {
        if !raw.is_file() {
            encode::save_raw(&raw, &audio::decode_file(&mp3_path)?)?;
        }
        mine_take = store.archive_current(&line.res)?;
    }
    if let Some(dir) = mp3_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&mp3_path, &line.mp3)?;
    let stream = mp3::parse(&line.mp3)?;
    let wrong_format = stream.sample_rate != audio::RATE || stream.channels != 1;
    match &line.raw {
        // Исходник от автора — без потерь, с его обрезкой и обработкой.
        Some(wav) => {
            if let Some(dir) = raw.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&raw, wav)?;
            store.set_trim(&line.res, line.trim.map(|[a, b]| (a, b)))?;
            store.set_processing(&line.res, line.fx.unwrap_or_default())?;
            if wrong_format {
                rebuild_mp3(store, &line.res)?;
            }
        }
        // Исходника нет — им становится сам mp3. Не в формате игры — пересобираем.
        None => {
            let samples = audio::decode_file(&mp3_path)?;
            if wrong_format {
                encode::write_mp3(&mp3_path, &samples)?;
            }
            encode::save_raw(&raw, &samples)?;
            store.set_trim(&line.res, None)?;
            store.set_processing(&line.res, Default::default())?;
        }
    }
    store.set_redo(&line.res, false)?;
    let local = store.mp3_hash(&line.res).unwrap_or_default();
    store.set_origin(&line.res, Some(Origin { author: line.author.clone(), hash: line.hash.clone(), local: local.clone() }))?;
    Ok((mine_take, local))
}

/// mp3 для игры из исходника, его обрезки и обработки — как после правки в программе.
fn rebuild_mp3(store: &Store, res: &str) -> Result<()> {
    let raw = audio::decode_file(&store.raw(res))?;
    let full = process::process(&raw, store.processing(res));
    let to_index = |secs: f32| ((secs.max(0.0) * audio::RATE as f32) as usize).min(full.len());
    let range = store.trim(res).map(|(a, b)| (to_index(a), to_index(b))).filter(|(a, b)| b > a).unwrap_or((0, full.len()));
    encode::write_mp3(&store.mp3(res), &process::cut(&full, range))
}

fn journal_path(export_root: &Path) -> PathBuf {
    export_root.join(JOURNAL)
}

fn save_journal(export_root: &Path, journal: &Journal) -> Result<()> {
    let path = journal_path(export_root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(journal)?)?;
    Ok(())
}

/// Последний импорт, который ещё можно отменить.
pub fn last_import(export_root: &Path) -> Option<Journal> {
    std::fs::read(journal_path(export_root)).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

#[derive(Debug, Default)]
pub struct UndoSummary {
    /// Сколько фраз вернулось как было.
    pub reverted: usize,
    /// Фразы, которые после импорта уже поменяли, — их не трогаем.
    pub kept: usize,
}

/// Отменяет последний импорт: новые фразы — в корзину, заменённые — обратно из дублей,
/// версии из пачки, положенные в дубли, — в корзину. Фразы, которые после импорта успели
/// поменять, остаются как есть.
pub fn undo_import(export_root: &Path) -> Result<UndoSummary> {
    let journal = last_import(export_root).context(tr!("нечего отменять — импортов не было", "nothing to undo — no imports yet"))?;
    let mut store = Store::open(export_root);
    let mut summary = UndoSummary::default();
    for step in journal.steps.iter().rev() {
        match step {
            Step::Added { res, local } | Step::Replaced { res, local, .. } if store.mp3_hash(res).as_deref() != Some(local.as_str()) => {
                summary.kept += 1;
            }
            Step::Added { res, .. } => {
                store.trash(res)?;
                summary.reverted += 1;
            }
            Step::Replaced { res, mine_take, .. } => {
                match mine_take {
                    Some(id) => {
                        // Версия из пачки уходит в дубли, оттуда — в корзину; моя — на место.
                        if let Some(imported) = store.restore_take(res, *id)? {
                            store.trash_take(res, imported)?;
                        }
                        rebuild_mp3(&store, res)?;
                        store.refresh_origin_local(res)?;
                    }
                    None => store.trash(res)?,
                }
                summary.reverted += 1;
            }
            Step::ToTakes { res, take } => {
                store.trash_take(res, *take)?;
                summary.reverted += 1;
            }
            Step::Note { res, note } => {
                if store.note(res) == note {
                    store.set_note(res, "")?;
                }
            }
        }
    }
    let _ = std::fs::remove_file(journal_path(export_root));
    Ok(summary)
}

/// Сэмплы из mp3 или wav в памяти — через временный файл (декодер читает файлы).
fn decode(bytes: &[u8], ext: &str) -> Result<Vec<f32>> {
    let tmp = std::env::temp_dir().join(format!("dota_revoice_take_{}_{}.{ext}", std::process::id(), content_hash(bytes)));
    std::fs::write(&tmp, bytes)?;
    let samples = audio::decode_file(&tmp);
    let _ = std::fs::remove_file(&tmp);
    samples
}

/// Сэмплы фразы из пачки — послушать перед решением (как она звучит в игре).
pub fn samples_of(line: &Incoming) -> Result<Vec<f32>> {
    decode(&line.mp3, "mp3")
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `sounds/vo/hero/name.mp3` → `sounds/vo/hero/name`; всё остальное — не фраза.
fn resource_of(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let res = lower.strip_prefix("sounds/")?.strip_suffix(".mp3")?;
    let ok = !res.is_empty()
        && res.split('/').all(|part| !part.is_empty() && part != ".." && part != "." && !part.starts_with('.'))
        && res.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '/'));
    ok.then(|| format!("sounds/{res}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f32, hz: f32) -> Vec<f32> {
        (0..(secs * 44100.0) as usize).map(|i| (i as f32 * hz * std::f32::consts::TAU / 44100.0).sin() * 0.3).collect()
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("dota_revoice_pack_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const AXE: &str = "sounds/vo/axe/axe_kill_01";
    const BOUNTY: &str = "sounds/vo/bounty_hunter/bount_item_01";
    const EVERYTHING: &dyn Fn(&str) -> bool = &|_| true;

    /// Запись как в программе: исходник wav и mp3 для игры.
    fn record(root: &Path, res: &str, hz: f32) {
        let store = Store::open(root);
        encode::save_take(&store.mp3(res), &store.raw(res), &tone(0.8, hz)).unwrap();
    }

    fn kinds(p: &Preview) -> Vec<(&str, Kind)> {
        p.lines.iter().map(|l| (l.res.as_str(), l.kind)).collect()
    }

    fn kind_of(p: &Preview, res: &str) -> Kind {
        p.lines.iter().find(|l| l.res == res).unwrap().kind
    }

    fn import_all(root: &Path, pack: &Path, conflicts: Decision) -> ImportSummary {
        let p = preview(root, pack, EVERYTHING).unwrap();
        apply(root, &p, |l| default_decision(l.kind, conflicts)).unwrap()
    }

    #[test]
    fn rejects_bad_paths() {
        assert_eq!(resource_of("sounds/vo/axe/axe_kill_01.mp3").as_deref(), Some(AXE));
        assert_eq!(resource_of("SOUNDS/VO/Axe/Axe_Kill_01.MP3").as_deref(), Some(AXE));
        for bad in ["sounds/../evil.mp3", "readme.txt", "other/vo/a.mp3", "sounds/vo/.raw/a.mp3", "sounds/vo/a b.mp3"] {
            assert_eq!(resource_of(bad), None, "{bad}");
        }
        assert_eq!(group_of(AXE), "sounds/vo/axe");
    }

    #[test]
    fn export_selected_heroes_and_import_into_friend() {
        // У меня: две фразы двух героев и заметка.
        let mine = temp("mine");
        record(&mine, AXE, 220.0);
        record(&mine, BOUNTY, 330.0);
        Store::open(&mine).set_note(BOUNTY, "покупка BKB").unwrap();
        assert_eq!(recorded_groups(&mine).unwrap().len(), 2);

        // Только Bounty Hunter.
        let only = BTreeSet::from(["sounds/vo/bounty_hunter".to_owned()]);
        let pack_file = mine.join("pack.zip");
        assert_eq!(export_pack(&mine, &pack_file, "Вася", Some(&only), false).unwrap(), 1);

        // У друга пусто — фраза новая, заметка переезжает, видно, от кого.
        let friend = temp("friend");
        let p = preview(&friend, &pack_file, EVERYTHING).unwrap();
        assert_eq!(p.author, "Вася");
        assert_eq!(p.with_sources(), 0);
        assert_eq!(kinds(&p), [(BOUNTY, Kind::New)]);
        let s = apply(&friend, &p, |l| default_decision(l.kind, Decision::KeepMine)).unwrap();
        assert_eq!((s.added, s.replaced, s.to_takes), (1, 0, 0));
        let fs = Store::open(&friend);
        assert_eq!(fs.note(BOUNTY), "покупка BKB");
        assert_eq!(fs.origin(BOUNTY).unwrap().author, "Вася");
        assert!(fs.raw(BOUNTY).is_file());

        // Повторный импорт — та же запись, её не трогаем.
        let p = preview(&friend, &pack_file, EVERYTHING).unwrap();
        assert_eq!(kinds(&p), [(BOUNTY, Kind::Same)]);

        // Отправляю пачку дальше: фраза Васи, которую я не менял, уходит под его именем.
        let relay = friend.join("relay.zip");
        export_pack(&friend, &relay, "Петя", None, false).unwrap();
        let third = temp("third");
        let p = preview(&third, &relay, EVERYTHING).unwrap();
        assert_eq!((p.author.as_str(), p.lines[0].author.as_str()), ("Петя", "Вася"));

        for d in [mine, friend, third] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }

    #[test]
    fn conflicts_updates_and_unknown() {
        let vasya = temp("vasya");
        record(&vasya, AXE, 220.0);
        record(&vasya, BOUNTY, 330.0);
        let first = vasya.join("first.zip");
        export_pack(&vasya, &first, "Вася", None, false).unwrap();

        // У Пети своя версия Axe и пусто у Bounty Hunter.
        let petya = temp("petya");
        record(&petya, AXE, 440.0);
        let p = preview(&petya, &first, EVERYTHING).unwrap();
        assert_eq!(kinds(&p), [(AXE, Kind::Conflict), (BOUNTY, Kind::New)]);

        // «Оставить мои»: Axe Пети на месте, версия Васи — в дублях, подписанная.
        let s = import_all(&petya, &first, Decision::KeepMine);
        assert_eq!((s.added, s.replaced, s.to_takes), (1, 0, 1));
        let takes = Store::open(&petya).takes(AXE);
        assert_eq!(takes.len(), 1);
        assert_eq!(takes[0].origin.as_ref().unwrap().author, "Вася");
        // Та же пачка ещё раз: версия Васи уже лежит в дублях — это «совпадает», а не конфликт.
        let p = preview(&petya, &first, EVERYTHING).unwrap();
        assert_eq!(kind_of(&p, AXE), Kind::Same);

        // Вася перезаписал Bounty Hunter — у Пети его прежняя нетронутая версия: обновление.
        record(&vasya, BOUNTY, 550.0);
        let second = vasya.join("second.zip");
        export_pack(&vasya, &second, "Вася", None, false).unwrap();
        let p = preview(&petya, &second, EVERYTHING).unwrap();
        assert_eq!(kind_of(&p, BOUNTY), Kind::Update);

        // Взять всё из пачки: обновление ложится поверх, прежняя версия — в дублях.
        let s = import_all(&petya, &second, Decision::Take);
        assert_eq!((s.added, s.replaced), (0, 1));
        assert_eq!(Store::open(&petya).takes(BOUNTY).len(), 1);

        // Петя поправил фразу Васи — та же пачка всё равно «совпадает» (нового в ней нет),
        // а новая версия Васи — уже конфликт, а не обновление.
        record(&petya, BOUNTY, 660.0);
        assert_eq!(kind_of(&preview(&petya, &second, EVERYTHING).unwrap(), BOUNTY), Kind::Same);
        record(&vasya, BOUNTY, 770.0);
        let third = vasya.join("third.zip");
        export_pack(&vasya, &third, "Вася", None, false).unwrap();
        assert_eq!(kind_of(&preview(&petya, &third, EVERYTHING).unwrap(), BOUNTY), Kind::Conflict);

        // Звука нет в игре — по умолчанию не берём.
        let empty = temp("empty");
        let p = preview(&empty, &second, &|res| res != AXE).unwrap();
        assert_eq!(kind_of(&p, AXE), Kind::Unknown);
        assert_eq!(default_decision(Kind::Unknown, Decision::Take), Decision::Skip);

        for d in [vasya, petya, empty] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }

    #[test]
    fn sources_travel_with_trim_and_sound() {
        let vasya = temp("src_vasya");
        record(&vasya, AXE, 220.0);
        let mut store = Store::open(&vasya);
        store.set_trim(AXE, Some((0.1, 0.6))).unwrap();
        store.set_processing(AXE, Settings { gain_db: 3.0, ..Default::default() }).unwrap();
        let pack_file = vasya.join("src.zip");
        export_pack(&vasya, &pack_file, "Вася", None, true).unwrap();

        let petya = temp("src_petya");
        let p = preview(&petya, &pack_file, EVERYTHING).unwrap();
        assert_eq!(p.with_sources(), 1);
        import_all(&petya, &pack_file, Decision::KeepMine);
        let s = Store::open(&petya);
        assert_eq!(s.trim(AXE), Some((0.1, 0.6)));
        assert_eq!(s.processing(AXE).gain_db, 3.0);
        assert_eq!(std::fs::read(s.raw(AXE)).unwrap(), std::fs::read(store.raw(AXE)).unwrap(), "исходник без потерь");
        // mp3 взят из пачки как есть — значит, повторный импорт узнаёт ту же версию.
        assert_eq!(kind_of(&preview(&petya, &pack_file, EVERYTHING).unwrap(), AXE), Kind::Same);

        for d in [vasya, petya] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }

    #[test]
    fn undo_puts_everything_back() {
        let vasya = temp("undo_vasya");
        record(&vasya, AXE, 220.0);
        record(&vasya, BOUNTY, 330.0);
        Store::open(&vasya).set_note(BOUNTY, "покупка").unwrap();
        let pack_file = vasya.join("undo.zip");
        export_pack(&vasya, &pack_file, "Вася", None, false).unwrap();

        // У Пети своя версия Axe: импорт с «взять из пачки» её заменит, Bounty Hunter — новая.
        let petya = temp("undo_petya");
        record(&petya, AXE, 440.0);
        let mine_axe = std::fs::read(Store::open(&petya).mp3(AXE)).unwrap();
        assert!(last_import(&petya).is_none());
        import_all(&petya, &pack_file, Decision::Take);
        let journal = last_import(&petya).unwrap();
        assert_eq!((journal.author.as_str(), journal.lines()), ("Вася", 2));

        let u = undo_import(&petya).unwrap();
        assert_eq!((u.reverted, u.kept), (2, 0));
        let s = Store::open(&petya);
        assert!(!s.mp3(BOUNTY).is_file(), "новая фраза — в корзине");
        assert_eq!(s.note(BOUNTY), "");
        assert!(s.origin(AXE).is_none(), "моя версия снова моя");
        assert_eq!(s.takes(AXE).len(), 0, "версия Васи ушла в корзину, а не в дубли");
        // Моя версия Axe пересобрана из её же исходника — та же запись.
        assert_eq!(audio::decode_file(&s.mp3(AXE)).unwrap().len(), decode(&mine_axe, "mp3").unwrap().len());
        assert!(last_import(&petya).is_none() && undo_import(&petya).is_err());

        // Если после импорта фразу поменяли — отмена её не трогает.
        import_all(&petya, &pack_file, Decision::Take);
        record(&petya, BOUNTY, 990.0);
        let u = undo_import(&petya).unwrap();
        assert_eq!((u.reverted, u.kept), (1, 1));
        assert!(Store::open(&petya).mp3(BOUNTY).is_file());

        for d in [vasya, petya] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }
}
