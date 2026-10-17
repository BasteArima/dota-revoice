//! Сборка пака с моими фразами. Основной способ — `build_overlay`: свой `pak01_dir.vpk`
//! с оглавлением пака Valve и моими фразами внутри (≈1 МБ + фразы). `build` — полная
//! копия озвучки одним файлом, осталась для отладочной утилиты.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use anyhow::{Context, Result};

use crate::vpk::{VpkBuilder, VpkReader};
use crate::{mp3, vsnd};
use crate::trf;

pub struct Summary {
    pub replaced: usize,
    /// Мои фразы, которых нет в базовом паке (опечатка в имени или новый файл).
    pub not_in_base: Vec<String>,
    pub copied: usize,
    pub size_bytes: u64,
}

/// Все mp3 в `export/sounds/**` → путь ресурса без расширения (`sounds/vo/hero/name`).
pub fn collect_export(export_root: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let mut out = BTreeMap::new();
    let sounds = export_root.join("sounds");
    if sounds.is_dir() {
        walk(&sounds, "sounds", &mut out)?;
    }
    Ok(out)
}

fn walk(dir: &Path, rel: &str, out: &mut BTreeMap<String, PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_ascii_lowercase();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk(&path, &format!("{rel}/{name}"), out)?;
        } else if let Some(stem) = name.strip_suffix(".mp3") {
            out.insert(format!("{rel}/{stem}"), path);
        }
    }
    Ok(())
}

/// `replacements`: путь ресурса без расширения → mp3-файл.
/// `progress` получает долю готовности 0..1.
pub fn build(
    base: Option<&Path>,
    replacements: &BTreeMap<String, PathBuf>,
    out_path: &Path,
    progress: impl Fn(f32),
) -> Result<Summary> {
    let mut vpk = VpkBuilder::default();
    let compiled = compile(replacements)?;

    let mut not_in_base = Vec::new();
    let mut copied = 0;
    if let Some(base) = base {
        let reader = VpkReader::open(base)?;
        not_in_base = compiled.keys().filter(|p| !reader.contains(p)).cloned().collect();
        let total = reader.entries().len().max(1);
        for (i, path) in reader.entries().keys().enumerate() {
            if !compiled.contains_key(path) {
                vpk.add(path, reader.read(path)?)?;
                copied += 1;
            }
            if i % 512 == 0 {
                progress(0.9 * i as f32 / total as f32);
            }
        }
    }
    let replaced = compiled.len() - not_in_base.len();
    for (path, data) in compiled {
        vpk.add(&path, data)?;
    }

    progress(0.9);
    if let Some(dir) = out_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = File::create(out_path).with_context(|| trf!("не создаётся {}", "can't create {}", out_path.display()))?;
    vpk.write_to(BufWriter::new(file)).with_context(|| trf!("не записался {}", "couldn't write {}", out_path.display()))?;
    progress(1.0);
    Ok(Summary { replaced, not_in_base, copied, size_bytes: std::fs::metadata(out_path)?.len() })
}

/// Пак-надстройка `pak01_dir.vpk`: оглавление пака Valve (`base`) и мои фразы внутри.
/// Остальная озвучка не копируется — игра читает её из томов Valve `pak01_00N.vpk`,
/// которые лежат рядом. `copied` в итоге — сколько файлов Valve взято ссылкой.
/// Без `base` в паке только мои фразы — остальные в игре станут английскими.
pub fn build_overlay(base: Option<&Path>, replacements: &BTreeMap<String, PathBuf>, out_path: &Path) -> Result<Summary> {
    let mut own = compile(replacements)?;
    let reader = base.map(VpkReader::open).transpose()?;
    let not_in_base: Vec<String> = match &reader {
        Some(r) => own.keys().filter(|p| !r.contains(p)).cloned().collect(),
        None => Vec::new(),
    };
    let replaced = own.len() - not_in_base.len();
    let stamp = match base {
        Some(b) => index_stamp(b).with_context(|| trf!("не читается {}", "can't read {}", b.display()))?,
        None => "none".to_owned(),
    };
    own.insert(MARKER.to_owned(), format!("Dota Revoice {}\nbase={stamp}\n", env!("CARGO_PKG_VERSION")).into_bytes());
    if let Some(dir) = out_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = File::create(out_path).with_context(|| trf!("не создаётся {}", "can't create {}", out_path.display()))?;
    crate::vpk::write_overlay(reader.as_ref(), &own, BufWriter::new(file))
        .with_context(|| trf!("не записался {}", "couldn't write {}", out_path.display()))?;
    let copied = reader.map_or(0, |r| r.entries().len() - replaced);
    Ok(Summary { replaced, not_in_base, copied, size_bytes: std::fs::metadata(out_path)?.len() })
}

/// Служебный файл внутри нашего пака: версия программы и отпечаток оглавления Valve,
/// на котором пак собран (`base=…`; `none` — собран без озвучки игры).
pub const MARKER: &str = "dota_revoice/pack.txt";

/// Отпечаток оглавления Valve — размер и CRC32 файла. Ссылки нашего пака верны,
/// пока оглавление то же: обновление Доты меняет и тома, и оглавление.
pub fn index_stamp(path: &Path) -> Option<String> {
    cached(&STAMPS, path, || {
        let bytes = std::fs::read(path).ok()?;
        Some(format!("{}:{:08x}", bytes.len(), crc32fast::hash(&bytes)))
    })
}

/// На каком оглавлении Valve собран наш пак: `Some(Some(отпечаток))`, `Some(None)` — без
/// озвучки игры, `None` — метки нет (собран до неё или это вообще не наш пак).
pub fn built_on(pak: &Path) -> Option<Option<String>> {
    cached(&MARKERS, pak, || {
        let text = String::from_utf8(VpkReader::open(pak).ok()?.read(MARKER).ok()?).ok()?;
        let base = text.lines().find_map(|l| l.strip_prefix("base="))?;
        Some((base != "none").then(|| base.to_owned()))
    })
}

/// Ответы по файлам, пока не поменялись их размер и время: вкладка сборки спрашивает часто.
type Cache<T> = Mutex<HashMap<PathBuf, (u64, SystemTime, Option<T>)>>;
static STAMPS: LazyLock<Cache<String>> = LazyLock::new(Default::default);
static MARKERS: LazyLock<Cache<Option<String>>> = LazyLock::new(Default::default);

fn cached<T: Clone>(cache: &Cache<T>, path: &Path, compute: impl FnOnce() -> Option<T>) -> Option<T> {
    let meta = std::fs::metadata(path).ok()?;
    let key = (meta.len(), meta.modified().ok()?);
    if let Some((len, at, value)) = cache.lock().ok()?.get(path)
        && (*len, *at) == key
    {
        return value.clone();
    }
    let value = compute();
    cache.lock().ok()?.insert(path.to_owned(), (key.0, key.1, value.clone()));
    value
}

/// Мои mp3 → `.vsnd_c` по путям ресурсов.
fn compile(replacements: &BTreeMap<String, PathBuf>) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut compiled = BTreeMap::new();
    for (resource, file) in replacements {
        let bytes = std::fs::read(file).with_context(|| trf!("не читается {}", "can't read {}", file.display()))?;
        let stream = mp3::parse(&bytes).with_context(|| trf!("не разобрал mp3 {}", "couldn't parse mp3 {}", file.display()))?;
        compiled.insert(format!("{resource}.vsnd_c"), vsnd::build(resource, &stream));
    }
    Ok(compiled)
}
