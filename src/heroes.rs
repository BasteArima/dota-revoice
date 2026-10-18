//! Герои из файлов игры: список, имена, папки озвучки, портреты — и фразы героя из пака.
//!
//! Список берётся из основного пака `game/dota/pak01_dir.vpk`:
//! - `scripts/npc/heroes/npc_dota_hero_<id>.txt` — `VoiceFile` даёт папку озвучки
//!   (у восьми героев она не совпадает с id: `crystal_maiden` → `crystalmaiden`);
//! - `resource/localization/abilities_<язык>.txt` — имя по ключу `npc_dota_hero_<id>:n`;
//! - `panorama/images/heroes/selection/npc_dota_hero_<id>_png.vtex_c` — портрет: внутри PNG,
//!   а у новых героев — сжатая текстура DXT5 (её распаковываем сами и сохраняем в PNG).
//!
//! Фразы — в паке выбранной озвучки (`dota_russian/pak01`): `sounds/vo/<папка>/*.vsnd_c`.
//! Озвучка на других языках есть не у всех героев: у остальных оригинал — английский
//! из основного пака, а свои фразы из папки языка всё равно перекрывают его в игре.
//! Личности и автоматоны (`voice.rs`) — отдельные карточки с той же папкой озвучки:
//! их фраз нет в других языках, оригинал у них всегда английский.
//! Разбор терпеливый: нет имени — имя из id, нет портрета — у героя будет заглушка.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::vpk::VpkReader;
use crate::sfx::{self, SoundSet};
use crate::voice::{self, Part, Variant};
use crate::vsnd;
use crate::trf;

/// Меняется, когда меняется содержимое кеша — тогда список пересобирается.
const CACHE_VERSION: u32 = 15;
const HERO_SCRIPTS: &str = "scripts/npc/heroes/npc_dota_hero_";
/// Меняется, когда распаковка начинает понимать новые форматы звука.
const EXTRACT_VERSION: u32 = 3;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hero {
    /// Системное имя без префикса: `crystal_maiden`.
    pub id: String,
    /// Папка озвучки: `crystalmaiden`.
    pub vo: String,
    pub name: String,
    /// Сколько фраз в паке озвучки.
    pub lines: usize,
    /// В выбранной озвучке героя нет — оригинал берём английский из основного пака.
    pub english_only: bool,
    /// Звуки способностей героя по способностям (из основного пака).
    #[serde(default)]
    pub sounds: Option<SoundSet>,
    /// Личность или автоматон основного героя (у них свой `id`, папка — его).
    #[serde(default)]
    pub variant: Option<Variant>,
}

impl Hero {
    /// Ключ карточки для счётчиков «записано»: `axe`, `axe:automaton`.
    pub fn card_key(&self) -> String {
        voice::card_key(&self.vo, self.variant)
    }

    /// Звуков способностей у этой карточки.
    pub fn sound_count(&self) -> usize {
        self.sounds.as_ref().map_or(0, |s| s.files.iter().filter(|f| f.variant == self.variant).count())
    }
}

/// Что программа знает об игре: герои и звуки предметов.
#[derive(Clone, Serialize, Deserialize)]
pub struct Catalog {
    stamp: String,
    pub heroes: Vec<Hero>,
    #[serde(default)]
    pub items: Option<SoundSet>,
}

/// Список героев: из кеша, если паки игры не менялись, иначе — разбором паков.
/// `main_pak` — `game/dota/pak01_dir.vpk`, `voice_pak` — пак выбранной озвучки,
/// `name_lang` — язык имён (`russian`). Портреты кладутся в `cache_dir/portraits`,
/// иконки способностей и предметов — в `cache_dir/icons`.
pub fn load_or_scan(main_pak: &Path, voice_pak: &Path, name_lang: &str, cache_dir: &Path) -> Result<Catalog> {
    let stamp = format!("{CACHE_VERSION}|{name_lang}|{}|{}", file_stamp(main_pak), file_stamp(voice_pak));
    let cache = cache_dir.join("heroes.json");
    if let Some(c) = std::fs::read(&cache).ok().and_then(|b| serde_json::from_slice::<Catalog>(&b).ok())
        && c.stamp == stamp
    {
        return Ok(c);
    }
    let (heroes, items) = scan(main_pak, voice_pak, name_lang, cache_dir)?;
    let catalog = Catalog { stamp, heroes, items };
    std::fs::create_dir_all(cache_dir).with_context(|| trf!("не создаётся {}", "can't create {}", cache_dir.display()))?;
    let json = serde_json::to_vec_pretty(&catalog)?;
    std::fs::write(&cache, json).with_context(|| trf!("не записывается {}", "can't write {}", cache.display()))?;
    Ok(catalog)
}

pub fn portrait_path(cache_dir: &Path, id: &str) -> PathBuf {
    cache_dir.join("portraits").join(format!("{id}.png"))
}

fn scan(main_pak: &Path, voice_pak: &Path, name_lang: &str, cache_dir: &Path) -> Result<(Vec<Hero>, Option<SoundSet>)> {
    let main = VpkReader::open(main_pak)?;
    let mut heroes = Vec::new();
    let mut scripts = HashMap::new();
    for path in main.entries().keys().filter(|p| p.starts_with(HERO_SCRIPTS) && p.ends_with(".txt")) {
        let id = &path[HERO_SCRIPTS.len()..path.len() - 4];
        let Ok(bytes) = main.read(path) else { continue };
        let text = String::from_utf8_lossy(&bytes);
        let Some(vo) = kv_value(&text, "VoiceFile").and_then(|f| voice_folder(&f)) else { continue };
        if kv_value(&text, "Enabled").as_deref() == Some("0") {
            continue;
        }
        heroes.push(Hero { id: id.to_owned(), vo, name: String::new(), lines: 0, english_only: false, sounds: None, variant: None });
        scripts.insert(id.to_owned(), text.into_owned());
    }
    ensure!(!heroes.is_empty(), trf!("в {} не нашлось героев — формат игры поменялся?", "no heroes found in {} — did the game format change?", main_pak.display()));

    // Личности и автоматоны — по своим фразам в английской озвучке основного пака.
    let english = line_names(&main);
    let mut variants = Vec::new();
    for h in &heroes {
        for v in Variant::ALL {
            let lines = english.get(&h.vo).map_or(0, |names| names.iter().filter(|n| voice::split(&h.vo, n).0 == Part::Variant(v)).count());
            if lines > 0 {
                variants.push(Hero { id: v.hero_id(&h.id), vo: h.vo.clone(), name: String::new(), lines, english_only: false, sounds: None, variant: Some(v) });
            }
        }
    }
    heroes.extend(variants);

    let names = [name_lang, "english"].map(|lang| {
        main.read(&format!("resource/localization/abilities_{lang}.txt"))
            .map(|b| hero_names(&String::from_utf8_lossy(&b)))
            .unwrap_or_default()
    });
    let name_of = |id: &str| names.iter().find_map(|n| n.get(id).cloned());
    let portraits = cache_dir.join("portraits");
    std::fs::create_dir_all(&portraits).with_context(|| trf!("не создаётся {}", "can't create {}", portraits.display()))?;
    for h in &mut heroes {
        h.name = name_of(&h.id).unwrap_or_else(|| match h.variant.and_then(|v| Some((Variant::of_hero_id(&h.id)?.0, v))) {
            // Имени в игре нет — «Axe (автоматон)».
            Some((base, v)) => format!("{} ({})", name_of(base).unwrap_or_else(|| title_case(base)), v.label()),
            None => title_case(&h.id),
        });
        let image = format!("panorama/images/heroes/selection/npc_dota_hero_{}_png.vtex_c", h.id);
        let Ok(vtex) = main.read(&image) else { continue };
        let path = portrait_path(cache_dir, &h.id);
        if let Some(png) = png_of(&vtex) {
            let _ = std::fs::write(&path, png);
        } else if let Some((w, h, rgba)) = texture_of(&vtex) {
            // У новых героев портрет вдвое мельче (71×94) — доводим до размера остальных.
            let (w, h, rgba) = upscale2x(w as usize, h as usize, &rgba);
            let _ = write_png(&path, w as u32, h as u32, &rgba);
        }
    }
    // Звуки способностей и предметов — по подписям из игры (на языке интерфейса, иначе английские).
    let tooltips = [name_lang, "english"].map(|lang| {
        main.read(&format!("resource/localization/abilities_{lang}.txt"))
            .map(|b| sfx::tooltip_names(&String::from_utf8_lossy(&b)))
            .unwrap_or_default()
    });
    let folders: BTreeSet<String> = main
        .entries()
        .keys()
        .filter_map(|p| p.strip_prefix(&format!("{}/", sfx::HERO_SOUNDS))?.split_once('/').map(|(f, _)| f.to_owned()))
        .collect();
    for h in heroes.iter_mut().filter(|h| h.variant.is_none()) {
        let script = scripts.get(&h.id).map_or("", String::as_str);
        h.sounds = sfx::hero_folder(&h.id, &h.vo, &folders).and_then(|f| sfx::hero_set(&main, f, &h.id, script, &tooltips, cache_dir));
    }
    // У личности и автоматона — звуки основного героя, помеченные для них (`blink_in_persona`).
    // Помеченные для карточки, которой нет, остаются у основного.
    let cards: BTreeSet<String> = heroes.iter().map(Hero::card_key).collect();
    let mut shared = HashMap::new();
    for h in heroes.iter_mut().filter(|h| h.variant.is_none()) {
        let Some(set) = &mut h.sounds else { continue };
        for f in &mut set.files {
            if f.variant.is_some_and(|v| !cards.contains(&voice::card_key(&h.vo, Some(v)))) {
                f.variant = None;
            }
        }
        shared.insert(h.vo.clone(), set.clone());
    }
    for h in heroes.iter_mut().filter(|h| h.variant.is_some()) {
        h.sounds = shared.get(&h.vo).filter(|s| s.files.iter().any(|f| f.variant == h.variant)).cloned();
    }
    let items = sfx::item_set(&main, &tooltips, cache_dir);
    drop(main);

    let same = voice_pak == main_pak;
    let voiced = if same { english.clone() } else { line_names(&VpkReader::open(voice_pak)?) };
    for h in heroes.iter_mut() {
        if h.variant.is_some() {
            // Фраз личностей и автоматонов в других языках нет.
            h.english_only = !same;
            continue;
        }
        let own = voiced.get(&h.vo);
        h.english_only = own.is_none() && !same;
        let mut lines: BTreeSet<&str> = own
            .or_else(|| english.get(&h.vo))
            .into_iter()
            .flatten()
            .filter(|n| !matches!(voice::split(&h.vo, n).0, Part::Variant(_)))
            .map(String::as_str)
            .collect();
        // Арканы и наборы обычно озвучены только по-английски — они тоже у героя.
        if !h.english_only && !same {
            let skins = english.get(&h.vo).into_iter().flatten().filter(|n| matches!(voice::split(&h.vo, n).0, Part::Skin { .. }));
            lines.extend(skins.map(String::as_str));
        }
        h.lines = lines.len();
    }
    // Героя без фраз в этой озвучке записывать не с чем.
    heroes.retain(|h| h.lines > 0);
    // Личность и автоматон — сразу за своим героем.
    let base_names: HashMap<String, String> = heroes.iter().filter(|h| h.variant.is_none()).map(|h| (h.vo.clone(), h.name.to_lowercase())).collect();
    heroes.sort_by_key(|h| (base_names.get(&h.vo).cloned().unwrap_or_else(|| h.name.to_lowercase()), h.variant.map_or(0, |v| 1 + v as usize)));
    Ok((heroes, items))
}

/// Имена фраз в каждой папке `sounds/vo/<папка>/` (без вложенных папок).
fn line_names(pak: &VpkReader) -> HashMap<String, Vec<String>> {
    let mut names: HashMap<String, Vec<String>> = HashMap::new();
    for path in pak.entries().keys() {
        if let Some((folder, name)) = vo_entry(path) {
            names.entry(folder.to_owned()).or_default().push(name.to_owned());
        }
    }
    names
}

/// `sounds/vo/axe/axe_kill_01.vsnd_c` → (`axe`, `axe_kill_01`).
fn vo_entry(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("sounds/vo/")?.strip_suffix(".vsnd_c")?;
    let (folder, name) = rest.split_once('/')?;
    (!name.contains('/')).then_some((folder, name))
}

/// Достаёт фразы героя из пака озвучки в `out_dir` mp3-файлами с игровыми именами.
/// Пока пак тот же, повторно не распаковывает. Возвращает число фраз.
pub fn extract_lines(voice_pak: &Path, vo: &str, out_dir: &Path) -> Result<usize> {
    extract_folder(voice_pak, &format!("sounds/vo/{vo}"), out_dir)
}

/// Достаёт звуки одной папки пака (`sounds/weapons/hero/axe`, `sounds/items`) в `out_dir`:
/// mp3 — как есть, несжатые — в wav. Вложенные папки не берёт.
pub fn extract_folder(pak_path: &Path, folder: &str, out_dir: &Path) -> Result<usize> {
    // Версия распаковки в метке: новая версия читает больше форматов — достаём заново.
    let stamp = format!("{}|{EXTRACT_VERSION}", file_stamp(pak_path));
    let marker = out_dir.join(".source");
    let existing = count_audio(out_dir);
    if existing > 0 && std::fs::read_to_string(&marker).is_ok_and(|s| s == stamp) {
        return Ok(existing);
    }
    let pak = VpkReader::open(pak_path)?;
    std::fs::create_dir_all(out_dir).with_context(|| trf!("не создаётся {}", "can't create {}", out_dir.display()))?;
    let prefix = format!("{folder}/");
    let mut n = 0;
    for path in pak.entries().keys() {
        let Some(name) = path.strip_prefix(&prefix).and_then(|r| r.strip_suffix(".vsnd_c")).filter(|r| !r.contains('/')) else { continue };
        let data = pak.read(path)?;
        // Фразы почти все — mp3; звуки способностей и предметов (и Марси) — несжатые, в wav.
        // Сначала PCM: его описание проверяется строго, а mp3 узнаётся по началу потока —
        // несжатый звук, начатый с тишины `FF FF`, похож на кадр mp3 (`ink_swell_cast`).
        let (file, other) = (out_dir.join(format!("{name}.wav")), out_dir.join(format!("{name}.mp3")));
        let written = if let Ok(pcm) = vsnd::pcm_of(&data) {
            write_wav(&file, &pcm).with_context(|| trf!("не записывается {}", "can't write {}", file.display()))?;
            Some(other)
        } else if let Ok(mp3) = vsnd::mp3_of(&data) {
            std::fs::write(&other, mp3).with_context(|| trf!("не записывается {}", "can't write {}", other.display()))?;
            Some(file)
        } else {
            None
        };
        // Прежняя распаковка могла положить тот же звук с другим расширением — иначе в списке два.
        if let Some(stale) = written {
            let _ = std::fs::remove_file(stale);
            n += 1;
        }
    }
    ensure!(n > 0, trf!("в {} нет звуков в {folder}", "no sounds in {folder} inside {}", pak_path.display()));
    std::fs::write(&marker, stamp)?;
    Ok(n)
}

/// Сколько моих фраз записано у каждой карточки (`Hero::card_key`): mp3 в
/// `export/sounds/vo/<папка>/`, фразы личности и автоматона — отдельно.
pub fn recorded_counts(export_root: &Path) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    let Ok(dirs) = std::fs::read_dir(export_root.join("sounds/vo")) else { return counts };
    for dir in dirs.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()) {
        let Some(folder) = dir.file_name().to_str().map(str::to_owned) else { continue };
        let Ok(files) = std::fs::read_dir(dir.path()) else { continue };
        for f in files.filter_map(|e| e.ok()).map(|e| e.path()) {
            if f.extension().is_some_and(|x| x.eq_ignore_ascii_case("mp3"))
                && let Some(name) = f.file_stem().and_then(|s| s.to_str())
            {
                *counts.entry(voice::card_of(&folder, &name.to_ascii_lowercase())).or_default() += 1;
            }
        }
    }
    counts
}

/// Сколько оригиналов уже распаковано: mp3, у Марси — wav.
fn count_audio(dir: &Path) -> usize {
    count_files(dir, &["mp3", "wav"])
}

fn count_files(dir: &Path, exts: &[&str]) -> usize {
    std::fs::read_dir(dir)
        .map(|it| {
            it.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| exts.iter().any(|ext| x.eq_ignore_ascii_case(ext))))
                .count()
        })
        .unwrap_or(0)
}

fn write_wav(path: &Path, pcm: &vsnd::Pcm) -> Result<()> {
    let spec = hound::WavSpec { channels: pcm.channels, sample_rate: pcm.sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in &pcm.samples {
        w.write_sample(s)?;
    }
    w.finalize()?;
    Ok(())
}

/// Путь (без `.off`: наше переключение не делает пак «другим») и время изменения.
fn file_stamp(path: &Path) -> String {
    let secs = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let shown = path.display().to_string();
    format!("{}@{secs}", shown.strip_suffix(".off").unwrap_or(&shown))
}

/// Значение ключа в тексте KeyValues: `"key"   "value"`.
fn kv_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let mut parts = line.split('"').skip(1).step_by(2);
        (parts.next()? == key).then(|| parts.next().map(str::to_owned))?
    })
}

/// `soundevents/voscripts/game_sounds_vo_crystalmaiden.vsndevts` → `crystalmaiden`.
fn voice_folder(voice_file: &str) -> Option<String> {
    let name = voice_file.rsplit('/').next()?;
    let folder = name.strip_prefix("game_sounds_vo_")?.split('.').next()?;
    (!folder.is_empty()).then(|| folder.to_ascii_lowercase())
}

/// Имена героев из `abilities_<язык>.txt`: `"npc_dota_hero_nevermore:n" "Shadow Fiend"`.
fn hero_names(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split('"').skip(1).step_by(2);
            let key = parts.next()?.strip_prefix("npc_dota_hero_")?;
            let id = key.split(':').next()?;
            // `npc_dota_hero_x__en:n` — служебные варианты, `npc_dota_hero_x_bio` — описания.
            // В русском перед именем стоит род: `#|f|#Лина` — отрезаем.
            let value = parts.next()?;
            let value = value.strip_prefix("#|").and_then(|v| v.split_once("|#")).map_or(value, |(_, v)| v).trim();
            (key.contains(':') && !id.contains("__") && !value.is_empty()).then(|| (id.to_owned(), value.to_owned()))
        })
        .collect()
}

/// PNG внутри `*_png.vtex_c`: от сигнатуры до конца чанка IEND.
pub fn png_of(vtex: &[u8]) -> Option<&[u8]> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    let start = vtex.windows(SIGNATURE.len()).position(|w| w == SIGNATURE)?;
    let end = vtex[start..].windows(4).position(|w| w == b"IEND")? + start + 8;
    vtex.get(start..end)
}

/// Текстура из `.vtex_c`: (ширина, высота, RGBA). DXT5 — портреты новых героев и
/// большинство иконок, несжатый BGRA8888 — часть иконок предметов. Размеры и формат —
/// из блока DATA, пиксели самой крупной мип-карты лежат в конце файла.
pub fn texture_of(vtex: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    const DXT5: u8 = 2;
    const BGRA8888: u8 = 28;
    let u32_at = |at: usize| vtex.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let table = 8 + u32_at(8)? as usize;
    let data = (0..u32_at(12)? as usize).find_map(|i| {
        let entry = table + i * 12;
        (vtex.get(entry..entry + 4)? == b"DATA").then(|| Some(entry + 4 + u32_at(entry + 4)? as usize))?
    })?;
    let info = vtex.get(data + 20..data + 28)?;
    let (w, h) = (u16::from_le_bytes([info[0], info[1]]) as usize, u16::from_le_bytes([info[2], info[3]]) as usize);
    if ![DXT5, BGRA8888].contains(&info[6]) || w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
    }
    if info[6] == BGRA8888 {
        let pixels = vtex.get(vtex.len().checked_sub(w * h * 4)?..)?;
        let rgba = pixels.as_chunks::<4>().0.iter().flat_map(|&[b, g, r, a]| [r, g, b, a]).collect();
        return Some((w as u32, h as u32, rgba));
    }
    let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
    let pixels_at = vtex.len().checked_sub(bw * bh * 16)?;
    let blocks = &vtex[pixels_at..];
    // Компилятор Valve может хранить цвет как YCoCg (яркость — в альфе): так помечено в заголовке.
    let ycocg = vtex[..pixels_at].windows(5).any(|w| w == b"YCoCg");
    let mut rgba = vec![0u8; w * h * 4];
    for (b, block) in blocks.as_chunks::<16>().0.iter().enumerate() {
        let (bx, by) = (b % bw * 4, b / bw * 4);
        for (i, px) in dxt5_block(block).iter().enumerate() {
            let (x, y) = (bx + i % 4, by + i / 4);
            if x < w && y < h {
                let px = if ycocg { from_ycocg(*px) } else { *px };
                rgba[(y * w + x) * 4..][..4].copy_from_slice(&px);
            }
        }
    }
    Some((w as u32, h as u32, rgba))
}

/// Один блок 4×4: альфа — 8 уровней с 3-битными индексами, цвет — 4 уровня RGB565.
fn dxt5_block(b: &[u8]) -> [[u8; 4]; 16] {
    let (a0, a1) = (b[0] as u32, b[1] as u32);
    let alpha = |i: u64| -> u8 {
        match (i, a0 > a1) {
            (0, _) => a0 as u8,
            (1, _) => a1 as u8,
            (i, true) => (((8 - i as u32) * a0 + (i as u32 - 1) * a1) / 7) as u8,
            (6, false) => 0,
            (7, false) => 255,
            (i, false) => (((6 - i as u32) * a0 + (i as u32 - 1) * a1) / 5) as u8,
        }
    };
    let alpha_bits = b[2..8].iter().rev().fold(0u64, |acc, &x| (acc << 8) | x as u64);
    let rgb565 = |v: u16| [((v >> 11) & 31) as u32 * 255 / 31, ((v >> 5) & 63) as u32 * 255 / 63, (v & 31) as u32 * 255 / 31];
    let (c0, c1) = (rgb565(u16::from_le_bytes([b[8], b[9]])), rgb565(u16::from_le_bytes([b[10], b[11]])));
    let mix = |w0: u32, w1: u32| [0, 1, 2].map(|k| ((w0 * c0[k] + w1 * c1[k]) / 3) as u8);
    let colors = [c0.map(|v| v as u8), c1.map(|v| v as u8), mix(2, 1), mix(1, 2)];
    let color_bits = u32::from_le_bytes([b[12], b[13], b[14], b[15]]);
    std::array::from_fn(|i| {
        let c = colors[((color_bits >> (i * 2)) & 3) as usize];
        [c[0], c[1], c[2], alpha((alpha_bits >> (i * 3)) & 7)]
    })
}

/// Масштабированный YCoCg-DXT5: Co и Cg в красном и зелёном, масштаб в синем, яркость в альфе.
fn from_ycocg([r, g, b, a]: [u8; 4]) -> [u8; 4] {
    // Без округления до целых на делении — иначе на плавных переходах видны ступеньки.
    let scale = ((b >> 3) + 1) as f32;
    let (co, cg, y) = ((r as f32 - 128.0) / scale, (g as f32 - 128.0) / scale, a as f32);
    let c = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    [c(y + co - cg), c(y + cg), c(y - co - cg), 255]
}

/// Увеличение вдвое бикубическим фильтром (Catmull-Rom), сначала по строкам, потом по столбцам.
/// Мягче и чище, чем растянуть мелкую картинку при показе.
fn upscale2x(w: usize, h: usize, rgba: &[u8]) -> (usize, usize, Vec<u8>) {
    let kernel = |t: f32| {
        let t = t.abs();
        if t < 1.0 {
            1.5 * t * t * t - 2.5 * t * t + 1.0
        } else if t < 2.0 {
            -0.5 * t * t * t + 2.5 * t * t - 4.0 * t + 2.0
        } else {
            0.0
        }
    };
    // Четыре соседних пикселя исходника и их веса для точки `x` результата.
    let taps = |x: usize, n: usize| -> [(usize, f32); 4] {
        let s = (x as f32 + 0.5) / 2.0 - 0.5;
        let base = s.floor() as isize - 1;
        std::array::from_fn(|k| {
            let i = base + k as isize;
            (i.clamp(0, n as isize - 1) as usize, kernel(s - i as f32))
        })
    };
    let (nw, nh) = (w * 2, h * 2);
    let mut rows = vec![0f32; nw * h * 4];
    for y in 0..h {
        for x in 0..nw {
            for (i, wt) in taps(x, w) {
                for c in 0..4 {
                    rows[(y * nw + x) * 4 + c] += rgba[(y * w + i) * 4 + c] as f32 * wt;
                }
            }
        }
    }
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..nh {
        let t = taps(y, h);
        for x in 0..nw {
            for c in 0..4 {
                let v: f32 = t.iter().map(|&(i, wt)| rows[(i * nw + x) * 4 + c] * wt).sum();
                out[(y * nw + x) * 4 + c] = v.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    (nw, nh, out)
}

pub(crate) fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(rgba)?;
    Ok(())
}

/// `crystal_maiden` → `Crystal Maiden`.
pub fn title_case(id: &str) -> String {
    id.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp3::Mp3Stream;
    use crate::vpk::VpkBuilder;

    fn hero_script(voice: &str, enabled: &str) -> Vec<u8> {
        format!("\"DOTAHeroes\"\n{{\n\t\"npc_dota_hero_x\"\n\t{{\n\t\t\"Enabled\"\t\t\"{enabled}\"\n\t\t\"VoiceFile\"\t\t\"soundevents/voscripts/game_sounds_vo_{voice}.vsndevts\"\n\t}}\n}}\n").into_bytes()
    }

    /// Несжатый звук как у старых ресурсов: DATA на 48 байт (частота, тип 0, каналы, сэмплы, размер).
    fn pcm_vsnd(samples: &[i16]) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&76u32.to_le_bytes());
        f.extend_from_slice(&[12, 0, 1, 0]);
        f.extend_from_slice(&8u32.to_le_bytes());
        f.extend_from_slice(&1u32.to_le_bytes());
        f.extend_from_slice(b"DATA");
        f.extend_from_slice(&8u32.to_le_bytes());
        f.extend_from_slice(&48u32.to_le_bytes());
        let mut data = [0u8; 48];
        data[..4].copy_from_slice(&[0x44, 0xAC, 0, 1]);
        data[8..12].copy_from_slice(&(samples.len() as u32).to_le_bytes());
        data[28..32].copy_from_slice(&(samples.len() as u32 * 2).to_le_bytes());
        f.extend_from_slice(&data);
        f.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
        f
    }

    fn vsnd(seed: u8) -> Vec<u8> {
        let mut data = vec![0xFF, 0xFB, 0x50, seed];
        data.resize(300, seed);
        let mp3 = Mp3Stream { sample_rate: 44100, channels: 1, samples_per_frame: 1152, frame_count: 1, data };
        vsnd::build("sounds/vo/x/y", &mp3)
    }

    #[test]
    fn scans_heroes_and_extracts_lines() {
        let root = std::env::temp_dir().join(format!("dota_revoice_heroes_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let png = b"\x89PNG\r\n\x1a\nDATA....IEND\xAE\x42\x60\x82";

        let mut main = VpkBuilder::default();
        main.add("scripts/npc/heroes/npc_dota_hero_crystal_maiden.txt", hero_script("crystalmaiden", "1")).unwrap();
        main.add("scripts/npc/heroes/npc_dota_hero_axe.txt", hero_script("axe", "1")).unwrap();
        main.add("scripts/npc/heroes/npc_dota_hero_old.txt", hero_script("old", "0")).unwrap();
        main.add("scripts/npc/heroes/npc_dota_hero_mute.txt", hero_script("mute", "1")).unwrap();
        main.add(
            "resource/localization/abilities_russian.txt",
            "\"lang\"\n{\n\t\"npc_dota_hero_crystal_maiden:n\"\t\"Crystal Maiden\"\n\t\"npc_dota_hero_crystal_maiden__en:n\"\t\"x\"\n}\n".into(),
        )
        .unwrap();
        main.add("panorama/images/heroes/selection/npc_dota_hero_axe_png.vtex_c", [b"head".as_slice(), png, b"tail"].concat()).unwrap();
        main.add("scripts/npc/heroes/npc_dota_hero_lina.txt", hero_script("lina", "1")).unwrap();
        // Lina есть только в английской озвучке основного пака.
        main.add("sounds/vo/lina/lina_kill_01.vsnd_c", vsnd(3)).unwrap();
        main.add("sounds/vo/axe/axe_kill_01.vsnd_c", vsnd(4)).unwrap();
        // Автоматон и набор Акса — только в английской озвучке.
        main.add("sounds/vo/axe/auto_axe_kill_01.vsnd_c", vsnd(5)).unwrap();
        main.add("sounds/vo/axe/jung_axe_kill_01.vsnd_c", vsnd(6)).unwrap();
        let main_pak = root.join("pak01_dir.vpk");
        std::fs::write(&main_pak, main.write()).unwrap();

        let mut voice = VpkBuilder::default();
        for (i, name) in ["axe_kill_01", "axe_kill_02"].iter().enumerate() {
            voice.add(&format!("sounds/vo/axe/{name}.vsnd_c"), vsnd(i as u8)).unwrap();
        }
        voice.add("sounds/vo/crystalmaiden/cm_kill_01.vsnd_c", vsnd(7)).unwrap();
        voice.add("sounds/vo/axe/sub/skip.vsnd_c", vsnd(9)).unwrap();
        // У «mute» фраз нет ни в одной озвучке — в списке его не будет.
        let voice_pak = root.join("voice_dir.vpk.off");
        std::fs::write(&voice_pak, voice.write()).unwrap();

        let cache = root.join("cache");
        let heroes = load_or_scan(&main_pak, &voice_pak, "russian", &cache).unwrap().heroes;
        let brief: Vec<_> = heroes.iter().map(|h| (h.id.as_str(), h.vo.as_str(), h.name.as_str(), h.lines, h.english_only)).collect();
        // У Акса две фразы на языке и набор из английской; автоматон — своя карточка.
        let auto = format!("Axe ({})", Variant::Automaton.label());
        assert_eq!(
            brief,
            [
                ("axe", "axe", "Axe", 3, false),
                ("axe_carnival", "axe", auto.as_str(), 1, true),
                ("crystal_maiden", "crystalmaiden", "Crystal Maiden", 1, false),
                ("lina", "lina", "Lina", 1, true),
            ]
        );
        assert_eq!(std::fs::read(portrait_path(&cache, "axe")).unwrap(), png);
        assert!(!portrait_path(&cache, "crystal_maiden").exists());
        // Второй раз — из кеша: пак с тем же временем изменения не перечитывается.
        let modified = std::fs::metadata(&main_pak).unwrap().modified().unwrap();
        std::fs::write(&main_pak, b"broken").unwrap();
        std::fs::File::options().write(true).open(&main_pak).unwrap().set_modified(modified).unwrap();
        assert_eq!(load_or_scan(&main_pak, &voice_pak, "russian", &cache).unwrap().heroes, heroes);

        let out = root.join("originals/sounds/vo/axe");
        assert_eq!(extract_lines(&voice_pak, "axe", &out).unwrap(), 2);
        let mp3 = std::fs::read(out.join("axe_kill_02.mp3")).unwrap();
        assert_eq!(&mp3[..4], &[0xFF, 0xFB, 0x50, 1]);
        assert_eq!(extract_lines(&voice_pak, "axe", &out).unwrap(), 2, "повторно — из готовых файлов");
        assert!(extract_lines(&voice_pak, "nobody", &root.join("none")).is_err());

        // Несжатый звук, начатый с `FF FF` (как `ink_swell_cast`), — в wav, старый «mp3» убирается.
        let mut sfx = VpkBuilder::default();
        sfx.add("sounds/weapons/hero/x/quiet.vsnd_c", pcm_vsnd(&[-1, -1, 300, -400])).unwrap();
        let sfx_pak = root.join("sfx_dir.vpk");
        std::fs::write(&sfx_pak, sfx.write()).unwrap();
        let sfx_out = root.join("originals/sfx");
        std::fs::create_dir_all(&sfx_out).unwrap();
        std::fs::write(sfx_out.join("quiet.mp3"), b"old").unwrap();
        assert_eq!(extract_folder(&sfx_pak, "sounds/weapons/hero/x", &sfx_out).unwrap(), 1);
        assert!(sfx_out.join("quiet.wav").is_file() && !sfx_out.join("quiet.mp3").exists());

        std::fs::create_dir_all(root.join("export/sounds/vo/axe")).unwrap();
        std::fs::write(root.join("export/sounds/vo/axe/axe_kill_01.mp3"), b"x").unwrap();
        std::fs::write(root.join("export/sounds/vo/axe/auto_axe_kill_01.mp3"), b"x").unwrap();
        let counts = recorded_counts(&root.join("export"));
        assert_eq!((counts.get("axe"), counts.get("axe:automaton")), (Some(&1), Some(&1)));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn upscale_keeps_flat_color_and_doubles_size() {
        let flat: Vec<u8> = [10u8, 200, 30, 255].repeat(3 * 2);
        let (w, h, out) = upscale2x(3, 2, &flat);
        assert_eq!((w, h, out.len()), (6, 4, 6 * 4 * 4));
        assert!(out.chunks(4).all(|px| px == [10, 200, 30, 255]));
        // Ступенька чёрное → белое остаётся монотонной по краям и не выходит за пределы.
        let step: Vec<u8> = [0u8, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255].to_vec();
        let (_, _, out) = upscale2x(4, 1, &step);
        assert_eq!(out[0], 0);
        assert_eq!(out[7 * 4], 255);
    }

    #[test]
    fn parsing_helpers() {
        assert_eq!(voice_folder("soundevents/voscripts/game_sounds_vo_pangolin.vsndevts").as_deref(), Some("pangolin"));
        assert_eq!(voice_folder("soundevents/game_sounds_heroes.vsndevts"), None);
        assert_eq!(title_case("obsidian_destroyer"), "Obsidian Destroyer");
        assert_eq!(png_of(b"no png here"), None);

        // Текстура 4×4 DXT5: красный → синий по столбцам, альфа 255 → 0 сверху вниз.
        let mut vtex = Vec::new();
        vtex.extend_from_slice(&0u32.to_le_bytes());
        vtex.extend_from_slice(&[12, 0, 1, 0]);
        vtex.extend_from_slice(&8u32.to_le_bytes()); // таблица блоков — сразу за полем
        vtex.extend_from_slice(&1u32.to_le_bytes());
        vtex.extend_from_slice(b"DATA");
        vtex.extend_from_slice(&8u32.to_le_bytes()); // от поля смещения до DATA
        vtex.extend_from_slice(&28u32.to_le_bytes());
        let mut data = vec![0u8; 20];
        data.extend_from_slice(&[4, 0, 4, 0, 1, 0, 2, 1]);
        vtex.extend_from_slice(&data);
        let alpha_rows: u64 = (0..16).map(|i| ((i / 4) as u64 & 1) << (i * 3)).sum(); // строки 0,2 — a0, 1,3 — a1
        let mut block = vec![255, 0];
        block.extend_from_slice(&alpha_rows.to_le_bytes()[..6]);
        block.extend_from_slice(&0xF800u16.to_le_bytes());
        block.extend_from_slice(&0x001Fu16.to_le_bytes());
        block.extend_from_slice(&0b01_11_10_00_01_11_10_00_01_11_10_00_01_11_10_00u32.to_le_bytes());
        vtex.extend_from_slice(&block);
        let (w, h, rgba) = texture_of(&vtex).unwrap();
        assert_eq!((w, h), (4, 4));
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255], "левый верхний — красный, непрозрачный");
        assert_eq!(&rgba[4..8], &[170, 0, 85, 255], "2/3 красного + 1/3 синего");
        assert_eq!(&rgba[12..16], &[0, 0, 255, 255]);
        assert_eq!(rgba[16 + 3], 0, "вторая строка — прозрачная");
        let names = hero_names(
            "\t\"npc_dota_hero_nevermore:n\"\t\t\"Shadow Fiend\"\n\t\"npc_dota_hero_nevermore_bio\"\t\"long\"\n\t\"npc_dota_hero_lina:n\"\t\"#|f|#Lina\"\n",
        );
        assert_eq!(names.get("nevermore").map(String::as_str), Some("Shadow Fiend"));
        assert_eq!(names.get("lina").map(String::as_str), Some("Lina"));
        assert_eq!(names.len(), 2);
    }
}
