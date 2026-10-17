//! Обмен фразами с друзьями: «пачка» — zip с готовыми mp3 и заметками.
//!
//! ```text
//! dota_revoice_pack.json                    ← формат, подпись автора, заметки, отпечатки фраз
//! sounds/vo/bounty_hunter/bount_kill_01.mp3 ← фраза, путь как в игре
//! ```
//!
//! Импорт идёт в два шага: `preview` раскладывает пачку на новые фразы, совпадающие,
//! обновления от того же автора и конфликты, а `apply` выполняет решения пользователя.
//! Ничего не теряется: заменённая версия уходит в дубли, а при «оставить мою» в дубли
//! уходит версия из пачки. Откуда фраза, помнит `Store::origin`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result, ensure};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use crate::project::{Origin, Store};
use crate::{audio, encode, mp3, pack};
use crate::{tr, trf};

const MANIFEST: &str = "dota_revoice_pack.json";
/// 1 — заметки; 2 — подпись автора и отпечатки фраз.
const FORMAT: u32 = 2;

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

/// Собирает мои фразы в пачку: все или только из `groups`. Возвращает число фраз.
pub fn export_pack(export_root: &Path, out: &Path, author: &str, groups: Option<&BTreeSet<String>>) -> Result<usize> {
    let lines: BTreeMap<_, _> = pack::collect_export(export_root)?
        .into_iter()
        .filter(|(res, _)| groups.is_none_or(|g| g.contains(group_of(res))))
        .collect();
    ensure!(!lines.is_empty(), tr!("пока нечего отправлять — нет ни одной записанной фразы", "nothing to send yet — no recorded lines"));
    let store = Store::open(export_root);
    let mut manifest = Manifest {
        format: FORMAT,
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        created_unix: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
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
        manifest.lines.insert(res.clone(), LineMeta { author: line_author, hash });
        data.push((res, bytes));
    }

    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = File::create(out).with_context(|| trf!("не создаётся {}", "can't create {}", out.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    // mp3 уже сжат — кладём как есть.
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file(MANIFEST, opts)?;
    zip.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
    for (res, bytes) in data {
        zip.start_file(format!("{res}.mp3"), opts)?;
        zip.write_all(&bytes)?;
    }
    zip.finish()?;
    Ok(lines.len())
}

/// Что за фраза пришла в пачке относительно моих.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// У меня этой фразы нет.
    New,
    /// Та же запись уже у меня (или я брал её у этого же человека и не менял).
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
}

/// Читает пачку и сравнивает с моими фразами, ничего не меняя.
/// `in_game` — есть ли такой звук в игре (путь ресурса без расширения).
pub fn preview(export_root: &Path, pack_path: &Path, in_game: &dyn Fn(&str) -> bool) -> Result<Preview> {
    let file = File::open(pack_path).with_context(|| trf!("не открывается {}", "can't open {}", pack_path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context(tr!("это не zip-архив", "this isn't a zip archive"))?;
    let store = Store::open(export_root);
    let mut manifest = Manifest::default();
    let mut files = Vec::new();
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
        match resource_of(&name) {
            Some(res) if mp3::parse(&bytes).is_ok() => files.push((res, bytes)),
            _ => rejected.push(name),
        }
    }

    let mut lines = Vec::with_capacity(files.len());
    for (res, mp3) in files {
        let hash = content_hash(&mp3);
        // В пачках первой версии подписей нет — тогда автор фразы тот, кто собрал пачку.
        let author = manifest.lines.get(&res).map_or_else(|| manifest.author.clone(), |m| m.author.clone());
        let mine = std::fs::read(store.mp3(&res)).ok().map(|b| content_hash(&b));
        let origin = store.origin(&res);
        let untouched = |o: &Origin| mine.as_deref() == Some(o.local.as_str());
        let kind = match &mine {
            _ if !in_game(&res) => Kind::Unknown,
            None => Kind::New,
            Some(m) if *m == hash => Kind::Same,
            Some(_) if origin.is_some_and(|o| o.hash == hash && untouched(o)) => Kind::Same,
            Some(_) if origin.is_some_and(|o| o.author == author && untouched(o)) => Kind::Update,
            Some(_) => Kind::Conflict,
        };
        lines.push(Incoming { res, kind, author, hash, mp3 });
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

/// Выполняет решения по фразам пачки. Решение для фразы — `decide`.
pub fn apply(export_root: &Path, preview: &Preview, decide: impl Fn(&Incoming) -> Decision) -> Result<ImportSummary> {
    let mut store = Store::open(export_root);
    let mut summary = ImportSummary::default();
    for line in &preview.lines {
        match decide(line) {
            Decision::Skip => {}
            Decision::KeepMine => {
                let samples = decode_mp3(&line.mp3)?;
                store.add_take(&line.res, &samples)?;
                summary.to_takes += 1;
            }
            Decision::Take => {
                let existed = store.mp3(&line.res).is_file();
                take(&mut store, line)?;
                if existed {
                    summary.replaced += 1;
                } else {
                    summary.added += 1;
                }
            }
        }
    }
    // Заметки друга берём только туда, где своих нет.
    for (res, note) in &preview.notes {
        if store.note(res).is_empty() && store.mp3(res).is_file() {
            store.set_note(res, note)?;
        }
    }
    Ok(summary)
}

/// Версия из пачки становится моей. Моя прежняя — в дубли (без исходника — из её mp3).
fn take(store: &mut Store, line: &Incoming) -> Result<()> {
    let (mp3_path, raw) = (store.mp3(&line.res), store.raw(&line.res));
    if mp3_path.is_file() {
        if !raw.is_file() {
            encode::save_raw(&raw, &audio::decode_file(&mp3_path)?)?;
        }
        store.archive_current(&line.res)?;
    }
    if let Some(dir) = mp3_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&mp3_path, &line.mp3)?;
    // Исходник для правок в программе. mp3 не в формате игры — пересобираем.
    let samples = audio::decode_file(&mp3_path)?;
    let stream = mp3::parse(&line.mp3)?;
    if stream.sample_rate != audio::RATE || stream.channels != 1 {
        encode::write_mp3(&mp3_path, &samples)?;
    }
    encode::save_raw(&raw, &samples)?;
    store.set_trim(&line.res, None)?;
    store.set_processing(&line.res, Default::default())?;
    store.set_redo(&line.res, false)?;
    let local = content_hash(&std::fs::read(&mp3_path)?);
    store.set_origin(&line.res, Some(Origin { author: line.author.clone(), hash: line.hash.clone(), local }))
}

/// Для дубля нужны сэмплы — декодируем mp3 из пачки через временный файл.
fn decode_mp3(bytes: &[u8]) -> Result<Vec<f32>> {
    let tmp = std::env::temp_dir().join(format!("dota_revoice_take_{}_{}.mp3", std::process::id(), content_hash(bytes)));
    std::fs::write(&tmp, bytes)?;
    let samples = audio::decode_file(&tmp);
    let _ = std::fs::remove_file(&tmp);
    samples
}

/// Сэмплы фразы из пачки — послушать перед решением.
pub fn samples_of(line: &Incoming) -> Result<Vec<f32>> {
    decode_mp3(&line.mp3)
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

    fn record(root: &Path, res: &str, hz: f32) {
        encode::write_mp3(&Store::open(root).mp3(res), &tone(0.8, hz)).unwrap();
    }

    fn kinds(p: &Preview) -> Vec<(&str, Kind)> {
        p.lines.iter().map(|l| (l.res.as_str(), l.kind)).collect()
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
        assert_eq!(export_pack(&mine, &pack_file, "Вася", Some(&only)).unwrap(), 1);

        // У друга пусто — фраза новая, заметка переезжает, видно, от кого.
        let friend = temp("friend");
        let p = preview(&friend, &pack_file, EVERYTHING).unwrap();
        assert_eq!(p.author, "Вася");
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
        export_pack(&friend, &relay, "Петя", None).unwrap();
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
        export_pack(&vasya, &first, "Вася", None).unwrap();

        // У Пети своя версия Axe и пусто у Bounty Hunter.
        let petya = temp("petya");
        record(&petya, AXE, 440.0);
        let p = preview(&petya, &first, EVERYTHING).unwrap();
        assert_eq!(kinds(&p), [(AXE, Kind::Conflict), (BOUNTY, Kind::New)]);

        // «Оставить мои»: Axe Пети на месте, версия Васи — в дублях.
        let s = import_all(&petya, &first, Decision::KeepMine);
        assert_eq!((s.added, s.replaced, s.to_takes), (1, 0, 1));
        assert_eq!(Store::open(&petya).takes(AXE).len(), 1);

        // Вася перезаписал Bounty Hunter — у Пети его прежняя нетронутая версия: обновление.
        record(&vasya, BOUNTY, 550.0);
        let second = vasya.join("second.zip");
        export_pack(&vasya, &second, "Вася", None).unwrap();
        let p = preview(&petya, &second, EVERYTHING).unwrap();
        assert_eq!(p.lines.iter().find(|l| l.res == BOUNTY).unwrap().kind, Kind::Update);

        // Взять всё из пачки: моя версия Axe уходит в дубли, обновление ложится поверх.
        let s = import_all(&petya, &second, Decision::Take);
        assert_eq!((s.added, s.replaced), (0, 2));
        assert_eq!(Store::open(&petya).takes(AXE).len(), 2);

        // Если Петя потом поправил фразу Васи — это уже конфликт, а не обновление.
        record(&petya, BOUNTY, 660.0);
        let p = preview(&petya, &second, EVERYTHING).unwrap();
        assert_eq!(p.lines.iter().find(|l| l.res == BOUNTY).unwrap().kind, Kind::Conflict);

        // Звука нет в игре — по умолчанию не берём.
        let p = preview(&temp("empty"), &second, &|res| res != AXE).unwrap();
        assert_eq!(p.lines.iter().find(|l| l.res == AXE).unwrap().kind, Kind::Unknown);
        assert_eq!(default_decision(Kind::Unknown, Decision::Take), Decision::Skip);

        for d in [vasya, petya, temp("empty")] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }
}
