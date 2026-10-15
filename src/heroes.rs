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
//! Разбор терпеливый: нет имени — имя из id, нет портрета — у героя будет заглушка.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::vpk::VpkReader;
use crate::vsnd;
use crate::trf;

/// Меняется, когда меняется содержимое кеша — тогда список пересобирается.
const CACHE_VERSION: u32 = 4;
const HERO_SCRIPTS: &str = "scripts/npc/heroes/npc_dota_hero_";

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
}

#[derive(Serialize, Deserialize)]
struct Catalog {
    stamp: String,
    heroes: Vec<Hero>,
}

/// Список героев: из кеша, если паки игры не менялись, иначе — разбором паков.
/// `main_pak` — `game/dota/pak01_dir.vpk`, `voice_pak` — пак выбранной озвучки,
/// `name_lang` — язык имён (`russian`). Портреты кладутся в `cache_dir/portraits`.
pub fn load_or_scan(main_pak: &Path, voice_pak: &Path, name_lang: &str, cache_dir: &Path) -> Result<Vec<Hero>> {
    let stamp = format!("{CACHE_VERSION}|{name_lang}|{}|{}", file_stamp(main_pak), file_stamp(voice_pak));
    let cache = cache_dir.join("heroes.json");
    if let Some(c) = std::fs::read(&cache).ok().and_then(|b| serde_json::from_slice::<Catalog>(&b).ok())
        && c.stamp == stamp
    {
        return Ok(c.heroes);
    }
    let heroes = scan(main_pak, voice_pak, name_lang, cache_dir)?;
    std::fs::create_dir_all(cache_dir).with_context(|| trf!("не создаётся {}", "can't create {}", cache_dir.display()))?;
    let json = serde_json::to_vec_pretty(&Catalog { stamp, heroes: heroes.clone() })?;
    std::fs::write(&cache, json).with_context(|| trf!("не записывается {}", "can't write {}", cache.display()))?;
    Ok(heroes)
}

pub fn portrait_path(cache_dir: &Path, id: &str) -> PathBuf {
    cache_dir.join("portraits").join(format!("{id}.png"))
}

fn scan(main_pak: &Path, voice_pak: &Path, name_lang: &str, cache_dir: &Path) -> Result<Vec<Hero>> {
    let main = VpkReader::open(main_pak)?;
    let mut heroes = Vec::new();
    for path in main.entries().keys().filter(|p| p.starts_with(HERO_SCRIPTS) && p.ends_with(".txt")) {
        let id = &path[HERO_SCRIPTS.len()..path.len() - 4];
        let Ok(bytes) = main.read(path) else { continue };
        let text = String::from_utf8_lossy(&bytes);
        let Some(vo) = kv_value(&text, "VoiceFile").and_then(|f| voice_folder(&f)) else { continue };
        if kv_value(&text, "Enabled").as_deref() == Some("0") {
            continue;
        }
        heroes.push(Hero { id: id.to_owned(), vo, name: String::new(), lines: 0, english_only: false });
    }
    ensure!(!heroes.is_empty(), trf!("в {} не нашлось героев — формат игры поменялся?", "no heroes found in {} — did the game format change?", main_pak.display()));

    let names = [name_lang, "english"].map(|lang| {
        main.read(&format!("resource/localization/abilities_{lang}.txt"))
            .map(|b| hero_names(&String::from_utf8_lossy(&b)))
            .unwrap_or_default()
    });
    let portraits = cache_dir.join("portraits");
    std::fs::create_dir_all(&portraits).with_context(|| trf!("не создаётся {}", "can't create {}", portraits.display()))?;
    for h in &mut heroes {
        h.name = names.iter().find_map(|n| n.get(&h.id).cloned()).unwrap_or_else(|| title_case(&h.id));
        let image = format!("panorama/images/heroes/selection/npc_dota_hero_{}_png.vtex_c", h.id);
        let Ok(vtex) = main.read(&image) else { continue };
        let path = portrait_path(cache_dir, &h.id);
        if let Some(png) = png_of(&vtex) {
            let _ = std::fs::write(&path, png);
        } else if let Some((w, h, rgba)) = dxt5_of(&vtex) {
            let _ = write_png(&path, w, h, &rgba);
        }
    }
    let english = line_counts(&main);
    drop(main);

    let same = voice_pak == main_pak;
    let voiced = if same { english.clone() } else { line_counts(&VpkReader::open(voice_pak)?) };
    for h in &mut heroes {
        match voiced.get(&h.vo) {
            Some(&n) => h.lines = n,
            None => {
                h.lines = english.get(&h.vo).copied().unwrap_or(0);
                h.english_only = !same;
            }
        }
    }
    // Героя без фраз в этой озвучке записывать не с чем.
    heroes.retain(|h| h.lines > 0);
    heroes.sort_by_key(|h| h.name.to_lowercase());
    Ok(heroes)
}

/// Фраз в каждой папке `sounds/vo/<папка>/` (без вложенных папок).
fn line_counts(pak: &VpkReader) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for path in pak.entries().keys() {
        if let Some((folder, _)) = vo_entry(path) {
            *counts.entry(folder.to_owned()).or_default() += 1;
        }
    }
    counts
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
    let stamp = file_stamp(voice_pak);
    let marker = out_dir.join(".source");
    let existing = count_mp3(out_dir);
    if existing > 0 && std::fs::read_to_string(&marker).is_ok_and(|s| s == stamp) {
        return Ok(existing);
    }
    let pak = VpkReader::open(voice_pak)?;
    std::fs::create_dir_all(out_dir).with_context(|| trf!("не создаётся {}", "can't create {}", out_dir.display()))?;
    let mut n = 0;
    for path in pak.entries().keys() {
        let Some((folder, name)) = vo_entry(path) else { continue };
        if folder != vo {
            continue;
        }
        let data = pak.read(path)?;
        // Редкие звуки не в mp3 пропускаем — записать поверх них всё равно можно будет позже.
        if let Ok(mp3) = vsnd::mp3_of(&data) {
            let file = out_dir.join(format!("{name}.mp3"));
            std::fs::write(&file, mp3).with_context(|| trf!("не записывается {}", "can't write {}", file.display()))?;
            n += 1;
        }
    }
    ensure!(n > 0, trf!("в {} нет фраз героя ({vo})", "no hero lines in {} ({vo})", voice_pak.display()));
    std::fs::write(&marker, stamp)?;
    Ok(n)
}

/// Сколько моих фраз записано у каждого героя: mp3 в `export/sounds/vo/<папка>/`.
pub fn recorded_counts(export_root: &Path) -> HashMap<String, usize> {
    let Ok(dirs) = std::fs::read_dir(export_root.join("sounds/vo")) else { return HashMap::new() };
    dirs.filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| Some((e.file_name().to_str()?.to_owned(), count_mp3(&e.path()))))
        .collect()
}

fn count_mp3(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|it| {
            it.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("mp3")))
                .count()
        })
        .unwrap_or(0)
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

/// Текстура DXT5 из `.vtex_c`: (ширина, высота, RGBA). Размеры и формат — из блока DATA,
/// пиксели самой крупной мип-карты лежат в конце файла.
pub fn dxt5_of(vtex: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    const DXT5: u8 = 2;
    let u32_at = |at: usize| vtex.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let table = 8 + u32_at(8)? as usize;
    let data = (0..u32_at(12)? as usize).find_map(|i| {
        let entry = table + i * 12;
        (vtex.get(entry..entry + 4)? == b"DATA").then(|| Some(entry + 4 + u32_at(entry + 4)? as usize))?
    })?;
    let info = vtex.get(data + 20..data + 28)?;
    let (w, h) = (u16::from_le_bytes([info[0], info[1]]) as usize, u16::from_le_bytes([info[2], info[3]]) as usize);
    if info[6] != DXT5 || w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
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
    let scale = (b as i32 >> 3) + 1;
    let (co, cg, y) = ((r as i32 - 128) / scale, (g as i32 - 128) / scale, a as i32);
    let c = |v: i32| v.clamp(0, 255) as u8;
    [c(y + co - cg), c(y + cg), c(y - co - cg), 255]
}

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<()> {
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
        let heroes = load_or_scan(&main_pak, &voice_pak, "russian", &cache).unwrap();
        let brief: Vec<_> = heroes.iter().map(|h| (h.id.as_str(), h.vo.as_str(), h.name.as_str(), h.lines, h.english_only)).collect();
        assert_eq!(
            brief,
            [
                ("axe", "axe", "Axe", 2, false),
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
        assert_eq!(load_or_scan(&main_pak, &voice_pak, "russian", &cache).unwrap(), heroes);

        let out = root.join("originals/sounds/vo/axe");
        assert_eq!(extract_lines(&voice_pak, "axe", &out).unwrap(), 2);
        let mp3 = std::fs::read(out.join("axe_kill_02.mp3")).unwrap();
        assert_eq!(&mp3[..4], &[0xFF, 0xFB, 0x50, 1]);
        assert_eq!(extract_lines(&voice_pak, "axe", &out).unwrap(), 2, "повторно — из готовых файлов");
        assert!(extract_lines(&voice_pak, "nobody", &root.join("none")).is_err());

        std::fs::create_dir_all(root.join("export/sounds/vo/axe")).unwrap();
        std::fs::write(root.join("export/sounds/vo/axe/axe_kill_01.mp3"), b"x").unwrap();
        assert_eq!(recorded_counts(&root.join("export")).get("axe"), Some(&1));
        std::fs::remove_dir_all(root).unwrap();
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
        let (w, h, rgba) = dxt5_of(&vtex).unwrap();
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
