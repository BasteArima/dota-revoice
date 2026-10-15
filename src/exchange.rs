//! Обмен фразами с друзьями: «пачка» — zip с готовыми mp3 и заметками.
//!
//! ```text
//! dota_revoice_pack.json                    ← версия формата, кто и когда собрал, заметки
//! sounds/vo/bounty_hunter/bount_kill_01.mp3 ← фраза, путь как в игре
//! ```
//!
//! При импорте своя версия фразы не трогается, пока явно не попросили заменить;
//! заменённая уходит в дубли, так что ничего не теряется.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::project::Store;
use crate::{audio, encode, mp3, pack};

const MANIFEST: &str = "dota_revoice_pack.json";
const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Manifest {
    format: u32,
    app_version: String,
    created_unix: u64,
    /// Путь ресурса → заметка («покупка BKB»).
    notes: BTreeMap<String, String>,
}

#[derive(Debug, Default)]
pub struct ImportSummary {
    pub added: usize,
    pub replaced: usize,
    /// Фразы, которые уже были у меня, — их не трогали.
    pub kept: Vec<String>,
    /// Файлы, которые не похожи на фразы (не тот путь или битый mp3).
    pub rejected: Vec<String>,
}

/// Собирает все мои фразы (всех героев) в пачку. Возвращает число фраз.
pub fn export_pack(export_root: &Path, out: &Path) -> Result<usize> {
    let lines = pack::collect_export(export_root)?;
    ensure!(!lines.is_empty(), "пока нечего отправлять — нет ни одной записанной фразы");
    let store = Store::open(export_root);
    let manifest = Manifest {
        format: FORMAT,
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        created_unix: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        notes: lines.keys().filter_map(|res| Some((res.clone(), store.notes().get(res)?.clone()))).collect(),
    };

    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = File::create(out).with_context(|| format!("не создаётся {}", out.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    // mp3 уже сжат — кладём как есть.
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file(MANIFEST, opts)?;
    zip.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
    for (res, path) in &lines {
        zip.start_file(format!("{res}.mp3"), opts)?;
        zip.write_all(&std::fs::read(path)?)?;
    }
    zip.finish()?;
    Ok(lines.len())
}

/// Раскладывает пачку по `export/`. `replace` — заменять фразы, которые у меня уже есть
/// (моя версия при этом уходит в дубли).
pub fn import_pack(export_root: &Path, pack_path: &Path, replace: bool) -> Result<ImportSummary> {
    let file = File::open(pack_path).with_context(|| format!("не открывается {}", pack_path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("это не zip-архив")?;
    let mut store = Store::open(export_root);
    let mut summary = ImportSummary::default();
    let mut manifest = Manifest::default();

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
        let Some(res) = resource_of(&name) else {
            summary.rejected.push(name);
            continue;
        };
        let Ok(stream) = mp3::parse(&bytes) else {
            summary.rejected.push(name);
            continue;
        };

        let mp3_path = store.mp3(&res);
        let exists = mp3_path.is_file();
        if exists && !replace {
            summary.kept.push(res);
            continue;
        }
        if exists {
            store.archive_current(&res)?;
        }
        if let Some(dir) = mp3_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&mp3_path, &bytes)?;
        // Исходник для правок в программе. mp3 не в формате игры — пересобираем.
        let samples = audio::decode_file(&mp3_path)?;
        if stream.sample_rate != audio::RATE || stream.channels != 1 {
            encode::write_mp3(&mp3_path, &samples)?;
        }
        encode::save_raw(&store.raw(&res), &samples)?;
        store.set_trim(&res, None)?;
        store.set_processing(&res, Default::default())?;
        store.set_redo(&res, false)?;
        if exists {
            summary.replaced += 1;
        } else {
            summary.added += 1;
        }
    }

    // Заметки друга берём только туда, где своих нет.
    for (res, note) in manifest.notes {
        if store.note(&res).is_empty() && store.mp3(&res).is_file() {
            store.set_note(&res, &note)?;
        }
    }
    Ok(summary)
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

    #[test]
    fn rejects_bad_paths() {
        assert_eq!(resource_of("sounds/vo/axe/axe_kill_01.mp3").as_deref(), Some("sounds/vo/axe/axe_kill_01"));
        assert_eq!(resource_of("SOUNDS/VO/Axe/Axe_Kill_01.MP3").as_deref(), Some("sounds/vo/axe/axe_kill_01"));
        for bad in ["sounds/../evil.mp3", "readme.txt", "other/vo/a.mp3", "sounds/vo/.raw/a.mp3", "sounds/vo/a b.mp3"] {
            assert_eq!(resource_of(bad), None, "{bad}");
        }
    }

    #[test]
    fn export_then_import_into_friend() {
        // У меня: две фразы и заметка.
        let mine = temp("mine");
        let mut store = Store::open(&mine);
        for (res, hz) in [("sounds/vo/axe/axe_kill_01", 220.0), ("sounds/vo/bounty_hunter/bount_item_01", 330.0)] {
            encode::write_mp3(&store.mp3(res), &tone(0.8, hz)).unwrap();
        }
        store.set_note("sounds/vo/bounty_hunter/bount_item_01", "покупка BKB").unwrap();
        let pack_file = mine.join("pack.zip");
        assert_eq!(export_pack(&mine, &pack_file).unwrap(), 2);

        // У друга пусто — всё добавляется, заметка переезжает, исходник для правок есть.
        let friend = temp("friend");
        let s = import_pack(&friend, &pack_file, false).unwrap();
        assert_eq!((s.added, s.replaced, s.kept.len(), s.rejected.len()), (2, 0, 0, 0));
        let fs = Store::open(&friend);
        assert_eq!(fs.note("sounds/vo/bounty_hunter/bount_item_01"), "покупка BKB");
        assert!(fs.raw("sounds/vo/axe/axe_kill_01").is_file());

        // Повторный импорт без замены ничего не трогает.
        let s = import_pack(&friend, &pack_file, false).unwrap();
        assert_eq!((s.added, s.replaced, s.kept.len()), (0, 0, 2));

        // С заменой — прежние версии уходят в дубли.
        let s = import_pack(&friend, &pack_file, true).unwrap();
        assert_eq!((s.added, s.replaced), (0, 2));
        assert_eq!(Store::open(&friend).takes("sounds/vo/axe/axe_kill_01").len(), 1);

        std::fs::remove_dir_all(mine).unwrap();
        std::fs::remove_dir_all(friend).unwrap();
    }
}
