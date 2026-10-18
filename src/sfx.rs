//! Звуки способностей и предметов: какие файлы к какой способности (предмету) относятся,
//! как их подписать и какие из них — звуки обликов (арканы, скины).
//!
//! Звуки героя лежат в `sounds/weapons/hero/<папка>/` основного пака, предметов — в
//! `sounds/items/`. Связи «звук → способность» в игре задают скомпилированные
//! soundevents; разбирать их не стали — хватает имён: `berserkers_call` →
//! `axe_berserkers_call`, `counterhelix_blood_chaser` → `axe_counter_helix` (облик).

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::heroes;
use crate::tr;
use crate::vpk::VpkReader;

/// Группа звуков в списке: способность, предмет, «Атака» или «Прочее».
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SoundGroup {
    /// Системное имя (`axe_berserkers_call`, `black_king_bar`) или `attack` / `other`.
    pub key: String,
    pub label: String,
    /// Иконка в кеше (`cache/icons/<id>.png`), если есть.
    pub icon: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SoundFile {
    /// Имя файла без расширения: `counterhelix_blood_chaser`.
    pub name: String,
    pub group: String,
    /// Звук облика (арканы, скина), а не базовый.
    pub skin: bool,
    /// Звук личности или автоматона героя: в его карточке, а не у основного.
    #[serde(default)]
    pub variant: Option<crate::voice::Variant>,
}

/// Звуки одной папки игры, разложенные по группам.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SoundSet {
    /// Путь ресурса папки: `sounds/weapons/hero/axe`, `sounds/items`.
    pub folder: String,
    /// Группы в порядке показа (способности — как у героя: Q, W, E, R…).
    pub groups: Vec<SoundGroup>,
    pub files: Vec<SoundFile>,
}

impl SoundSet {
    pub fn file(&self, name: &str) -> Option<&SoundFile> {
        self.files.iter().find(|f| f.name == name)
    }

    pub fn group(&self, key: &str) -> Option<&SoundGroup> {
        self.groups.iter().find(|g| g.key == key)
    }

    /// Сколько звуков в группе.
    pub fn count(&self, key: &str) -> usize {
        self.files.iter().filter(|f| f.group == key).count()
    }
}

pub const HERO_SOUNDS: &str = "sounds/weapons/hero";
pub const ITEM_SOUNDS: &str = "sounds/items";
pub const ATTACK: &str = "attack";
pub const OTHER: &str = "other";

/// Части имени, которые бывают у обычных звуков способностей: всё остальное в хвосте
/// имени (`blood_chaser`, `shoutmask`, `automaton`) — признак облика.
const PLAIN: &[&str] = &[
    "cast", "target", "targets", "impact", "loop", "lp", "start", "end", "stop", "success", "fail", "failure", "hit", "hits",
    "damage", "dmg", "proj", "projectile", "launch", "explode", "explosion", "buff", "debuff", "ally", "enemy", "self",
    "layer", "pre", "post", "channel", "tick", "spawn", "death", "die", "attack", "preattack", "alt", "return", "activate",
    "deactivate", "on", "off", "idle", "travel", "land", "burn", "heal", "bounce", "out", "in", "open", "close", "trigger",
    "creep", "hero", "a", "b", "c", "d", "small", "large", "big", "short", "long", "lvl", "level", "max", "min", "first",
    "second", "third", "end", "begin", "ready", "charge", "release", "swing", "slam", "shot", "cooldown", "ult", "ultimate",
    "area", "aoe", "wave", "move", "movement", "fire", "fly", "flight", "trail", "stun", "silence", "slow", "kill",
    "retract", "throw", "pull", "grab", "catch", "toss", "jump", "leap", "dash", "blast", "strike", "summon", "swap",
    "spin", "toggle", "pulse", "discharge", "ignite", "shield", "armor", "lock", "unlock", "drop", "pickup", "use",
    "expire", "timer", "warning", "ready",
];

/// `attack01` → `attack`: номер варианта для сравнения не важен.
fn word(token: &str) -> &str {
    token.trim_end_matches(|c: char| c.is_ascii_digit())
}

fn words(name: &str) -> Vec<&str> {
    name.split('_').map(word).filter(|w| !w.is_empty()).collect()
}

/// С чем сравнивать имя звука: ключ группы и варианты имени (`meat_hook` — из системного
/// имени, `meat_hook` — из английской подписи «Meat Hook»).
pub struct Candidate {
    pub key: String,
    pub bases: Vec<String>,
}

/// Подпись в имя для сравнения: `Revenant's Brooch` → `revenants_brooch`.
pub fn label_base(label: &str) -> String {
    label
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .map(|w| w.replace('\'', ""))
        .filter(|w| !w.is_empty() && !STOP.contains(&w.as_str()))
        .collect::<Vec<_>>()
        .join("_")
}

const STOP: &[&str] = &["of", "the", "and", "a"];

/// Раскладывает файлы по группам. Правила, от сильного к слабому:
/// 1. имя группы целиком — начало имени звука (`counterhelix` ↔ `counter_helix`);
/// 2. все слова имени группы есть в имени звука (`blink_arcane` ↔ `arcane_blink`);
/// 3. первое слово звука — слово, которое есть только у одной группы (`hook_cast` ↔ Meat Hook);
/// 4. у предметов — сокращение: каждое слово имени группы начинается со слова звука
///    (`rune_regen` ↔ `rune_regeneration`, `mek_cast` ↔ `mekansm`), и группа такая одна.
///
/// Оставшиеся слова решают, облик ли это. `hero` — звуки героя: удары идут в «Атаку»,
/// облики помечаются и в «Прочем»; у предметов обликов нет.
pub fn classify(names: &[String], candidates: &[Candidate], hero: bool) -> Vec<SoundFile> {
    // Слова, по которым группу можно узнать одну: длинные и встречающиеся у одной группы.
    let mut owners: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for c in candidates {
        for base in &c.bases {
            for w in words(base).into_iter().filter(|w| w.len() >= 4 && !PLAIN.contains(w)) {
                owners.entry(w).or_default().insert(c.key.as_str());
            }
        }
    }
    names
        .iter()
        .map(|name| {
            let norm = name.strip_prefix("dota_item_").or_else(|| name.strip_prefix("item_")).unwrap_or(name);
            let file = words(norm);
            let compact: String = file.concat();
            let mut best: Option<(usize, &str, Vec<&str>)> = None;
            for c in candidates {
                for base in &c.bases {
                    let base_words = words(base);
                    let base_compact: String = base_words.concat();
                    if base_compact.is_empty() || best.as_ref().is_some_and(|b| b.0 >= base_compact.len()) {
                        continue;
                    }
                    let rest = if compact.starts_with(&base_compact) {
                        // Слова, которые ушли на имя, — те, что покрывают его длину.
                        let mut len = 0;
                        file.iter()
                            .copied()
                            .skip_while(|w| {
                                let inside = len < base_compact.len();
                                len += w.len();
                                inside
                            })
                            .collect()
                    } else if base_words.iter().all(|w| file.contains(w)) {
                        file.iter().copied().filter(|w| !base_words.contains(w)).collect()
                    } else {
                        continue;
                    };
                    best = Some((base_compact.len(), &c.key, rest));
                }
            }
            // Хвост без слов самой группы. У предметов слово другого предмета в хвосте —
            // значит, звук его (`giants_maul_target` не Giant's Ring); у героя незнакомый
            // хвост — облик способности (`berserkers_shoutmask`).
            if best.is_none()
                && let Some(first) = file.first()
                && let Some(&key) = owners.get(first).filter(|k| k.len() == 1).and_then(|k| k.first())
            {
                let own: Vec<&str> = candidates.iter().filter(|c| c.key == key).flat_map(|c| c.bases.iter().flat_map(|b| words(b))).collect();
                let rest: Vec<&str> = file[1..].iter().copied().filter(|w| !own.contains(w)).collect();
                let foreign = rest.iter().any(|w| owners.get(w).is_some_and(|k| !k.contains(key)));
                // У предмета такой хвост — просто подробность звука (`meteor_fall`), не облик.
                if hero {
                    best = Some((0, key, rest));
                } else if !foreign {
                    best = Some((0, key, Vec::new()));
                }
            }
            if best.is_none() && !hero {
                let abbreviates = |base: &String| {
                    let base = words(base);
                    !base.is_empty() && base.len() <= file.len() && base.iter().zip(&file).all(|(b, f)| f.len() >= 3 && b.starts_with(f))
                };
                let hits: BTreeSet<&str> = candidates.iter().filter(|c| c.bases.iter().any(abbreviates)).map(|c| c.key.as_str()).collect();
                if let [key] = hits.iter().copied().collect::<Vec<_>>()[..] {
                    best = Some((0, key, Vec::new()));
                }
            }
            let (group, rest, check) = match best {
                Some((_, key, rest)) => (key.to_owned(), rest, true),
                None if hero && matches!(file.first(), Some(&("attack" | "preattack"))) => (ATTACK.to_owned(), file[1..].to_vec(), true),
                None => (OTHER.to_owned(), file.clone(), hero),
            };
            // Звук личности или автоматона — не облик: он целиком в их карточке.
            let variant = if hero { crate::voice::sound_variant(name) } else { None };
            // Облики — у способностей; у предметов хвост — подробность звука (`mael_lightning`).
            let skin = hero && variant.is_none() && check && rest.iter().any(|w| !PLAIN.contains(w));
            SoundFile { name: name.clone(), group, skin, variant }
        })
        .collect()
}

/// Папка звуков героя: обычно это его системное имя, иногда — короче или слитно
/// (`nyx` у `nyx_assassin`, `nightstalker` у `night_stalker`, `underlord` у `abyssal_underlord`).
pub fn hero_folder<'a>(id: &str, vo: &str, folders: &'a BTreeSet<String>) -> Option<&'a String> {
    let compact = |s: &str| s.replace('_', "");
    let (cid, cvo) = (compact(id), compact(vo));
    folders
        .iter()
        .find(|f| *f == id || *f == vo)
        .or_else(|| folders.iter().find(|f| compact(f) == cid || compact(f) == cvo))
        .or_else(|| folders.iter().filter(|f| f.len() >= 3 && cid.starts_with(&compact(f))).max_by_key(|f| f.len()))
        .or_else(|| folders.iter().filter(|f| f.len() >= 6 && cid.contains(&compact(f))).max_by_key(|f| f.len()))
        // `bristlebog` у `bristleback`: общее начало хотя бы в 7 букв.
        .or_else(|| {
            let common = |f: &str| compact(f).chars().zip(cid.chars()).take_while(|(a, b)| a == b).count();
            folders.iter().filter(|f| common(f) >= 7).max_by_key(|f| common(f))
        })
}

/// Подписи способностей и предметов: `"DOTA_Tooltip_ability_axe_berserkers_call" "Berserker's Call"`.
pub(crate) fn tooltip_names(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split('"').skip(1).step_by(2);
            let key = parts.next()?;
            let id = key.get(..21).filter(|p| p.eq_ignore_ascii_case("DOTA_Tooltip_ability_")).map(|_| &key[21..])?;
            // У новых предметов подпись — с `:n` (`item_kobold_cup:n`), как имена героев.
            let id = id.strip_suffix(":n").unwrap_or(id);
            let value = parts.next()?;
            let value = value.strip_prefix("#|").and_then(|v| v.split_once("|#")).map_or(value, |(_, v)| v).trim();
            (!value.is_empty()).then(|| (id.to_ascii_lowercase(), value.to_owned()))
        })
        .collect()
}

/// Способности героя из его скрипта: `"Ability1" "axe_berserkers_call"`, по порядку, без служебных.
pub(crate) fn hero_abilities(script: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in script.lines() {
        let mut parts = line.split('"').skip(1).step_by(2);
        let (Some(key), Some(value)) = (parts.next(), parts.next()) else { continue };
        let numbered = key.strip_prefix("Ability").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        let service = value.is_empty() || value.starts_with("generic_") || value.starts_with("special_bonus") || value == "dota_empty_ability";
        if numbered && !service && !out.iter().any(|a| a == value) {
            out.push(value.to_owned());
        }
    }
    out
}

/// Иконку из игры — в кеш PNG; `None`, если её нет или формат не знаем.
pub(crate) fn cache_icon(main: &VpkReader, texture: &str, cache_dir: &Path, id: &str) -> Option<String> {
    let path = cache_dir.join("icons").join(format!("{id}.png"));
    if !path.is_file() {
        let vtex = main.read(texture).ok()?;
        std::fs::create_dir_all(path.parent()?).ok()?;
        match heroes::png_of(&vtex) {
            Some(png) => std::fs::write(&path, png).ok()?,
            None => {
                let (w, h, rgba) = heroes::texture_of(&vtex)?;
                heroes::write_png(&path, w, h, &rgba).ok()?;
            }
        }
    }
    Some(id.to_owned())
}

pub fn icon_path(cache_dir: &Path, id: &str) -> std::path::PathBuf {
    cache_dir.join("icons").join(format!("{id}.png"))
}

/// Файлы-звуки прямо в папке `folder` пака (без вложенных), без расширения.
pub(crate) fn sounds_in(main: &VpkReader, folder: &str) -> Vec<String> {
    let prefix = format!("{folder}/");
    let mut names: Vec<String> = main
        .entries()
        .keys()
        .filter_map(|p| p.strip_prefix(&prefix)?.strip_suffix(".vsnd_c").filter(|n| !n.contains('/')).map(str::to_owned))
        .collect();
    names.sort();
    names
}

/// Звуки героя по способностям. `names` — подписи на языке интерфейса, потом английские.
pub(crate) fn hero_set(
    main: &VpkReader,
    folder: &str,
    hero_id: &str,
    script: &str,
    names: &[HashMap<String, String>],
    cache_dir: &Path,
) -> Option<SoundSet> {
    let path = format!("{HERO_SOUNDS}/{folder}");
    let files = sounds_in(main, &path);
    if files.is_empty() {
        return None;
    }
    let abilities = hero_abilities(script);
    let english = names.last();
    let candidates: Vec<Candidate> = abilities
        .iter()
        .map(|a| {
            let base = [hero_id, folder].iter().find_map(|p| a.strip_prefix(&format!("{p}_"))).unwrap_or(a);
            let mut bases = vec![base.to_owned()];
            bases.extend(english.and_then(|n| n.get(a)).map(|l| label_base(l)));
            Candidate { key: a.clone(), bases }
        })
        .collect();
    let files = classify(&files, &candidates, true);
    let label = |id: &str| names.iter().find_map(|n| n.get(id).cloned()).unwrap_or_else(|| heroes::title_case(id));
    let mut groups: Vec<SoundGroup> = abilities
        .iter()
        .filter(|a| files.iter().any(|f| &f.group == *a))
        .map(|a| SoundGroup { key: a.clone(), label: label(a), icon: cache_icon(main, &format!("panorama/images/spellicons/{a}_png.vtex_c"), cache_dir, a) })
        .collect();
    for (key, text) in [(ATTACK, tr!("Атака", "Attack")), (OTHER, tr!("Прочее", "Other"))] {
        if files.iter().any(|f| f.group == key) {
            groups.push(SoundGroup { key: key.to_owned(), label: text.to_owned(), icon: None });
        }
    }
    Some(SoundSet { folder: path, groups, files })
}

/// Иконка предмета: обычно с тем же именем, но бывает названа чуть иначе — `bottle_invisibility`
/// у `bottle_invisible`, `bottle_doubledamage` у `bottle_double_damage`. Иконки других
/// предметов (`taken`) не берём; описания (`aeon_disk_description`) слишком далеки от иконок.
fn item_icon(id: &str, icons: &BTreeSet<&str>, taken: &BTreeSet<&str>) -> Option<String> {
    if icons.contains(id) {
        return Some(id.to_owned());
    }
    let compact = |s: &str| s.replace('_', "");
    let want = compact(id);
    icons
        .iter()
        .filter(|i| !taken.contains(*i))
        .filter_map(|i| {
            let have = compact(i);
            let common = want.chars().zip(have.chars()).take_while(|(a, b)| a == b).count();
            let shorter = want.len().min(have.len());
            let close = have == want || (want.len().abs_diff(have.len()) <= 3 && common >= 6 && common + 3 >= shorter);
            close.then_some((common, *i))
        })
        .max()
        .map(|(_, i)| i.to_owned())
}

/// Какие события играют звук предмета: имя файла → имена событий в виде имени звука
/// (`DOTA_Item.BattleFury` → `battle_fury`). События — в `game_sounds_items.vsndevts_c`.
fn item_events(main: &VpkReader) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let Ok(res) = main.read("soundevents/game_sounds_items.vsndevts_c") else { return out };
    let Some(kv) = crate::vsnd::block(&res, b"DATA").and_then(crate::kv3::decode) else { return out };
    for (event, files) in kv.sound_events() {
        for file in files {
            if let Some(stem) = file.strip_prefix(&format!("{ITEM_SOUNDS}/")).and_then(|f| f.strip_suffix(".vsnd")) {
                out.entry(stem.to_owned()).or_default().push(event_base(&event));
            }
        }
    }
    out
}

/// Имя события как имя звука: `DOTA_Item.HurricanePike.Activate` → `hurricane_pike_activate`.
fn event_base(event: &str) -> String {
    let lower = event.to_ascii_lowercase();
    let start = ["dota_item.", "item.", "item_"].iter().find(|p| lower.starts_with(*p)).map_or(0, |p| p.len());
    let mut out = String::new();
    let mut prev = '_';
    for c in event[start..].chars() {
        let c = if c == '.' { '_' } else { c };
        if c.is_ascii_uppercase() && (prev.is_ascii_lowercase() || prev.is_ascii_digit()) {
            out.push('_');
        }
        if !(c == '_' && out.ends_with('_')) {
            out.push(c.to_ascii_lowercase());
        }
        prev = c;
    }
    out.trim_matches('_').to_owned()
}

/// Способности предмета из описания: `<h1>Passive: Combo Breaker</h1>` → `Combo Breaker`.
fn item_abilities(description: &str) -> Vec<String> {
    description
        .split("<h1>")
        .skip(1)
        .filter_map(|part| part.split("</h1>").next()?.split_once(':').map(|(_, name)| name.trim().to_owned()))
        .filter(|name| !name.is_empty())
        .collect()
}

/// Звуки предметов, названные сокращением или рабочим именем, которое по имени предмета
/// не угадать и по имени события: начало имени звука → предмет. Таблицы «предмет → звук»
/// в файлах игры нет — событие вызывает код (`server.dll`); рабочие имена сверены по
/// соседним строкам там же (`DOTA_Item.FaerieSpark` — рядом с `CDOTA_Item_Faerie_Fire`).
const ITEM_ALIASES: &[(&str, &str)] = &[
    ("combo_breaker", "aeon_disk"),
    ("star_emblem", "solar_crest"),
    ("wraith_totem", "wraith_pact"),
    ("deso", "desolator"),
    ("mek", "mekansm"),
    ("mael", "maelstrom"),
    ("mjoll", "mjollnir"),
    ("mkb", "monkey_king_bar"),
    ("hotd", "helm_of_the_dominator"),
    ("daedelus", "greater_crit"),
    ("necro", "necronomicon"),
    ("mango", "enchanted_mango"),
    ("lotus_activate", "lotus_orb"),
    ("lotus_cast", "lotus_orb"),
    ("lotus_end", "lotus_orb"),
    ("faerie_spark", "faerie_fire"),
    ("rune_dd", "bottle_double_damage"),
    ("rune_xp", "bottle_xp"),
    ("black_powder", "black_powder_bag"),
    ("forager", "foragers_kit"),
    ("stonefeather", "stonefeather_satchel"),
];

/// Звуки предметов по предметам (по алфавиту), непривязанные — в «Прочие звуки предметов».
pub(crate) fn item_set(main: &VpkReader, names: &[HashMap<String, String>], cache_dir: &Path) -> Option<SoundSet> {
    let files = sounds_in(main, ITEM_SOUNDS);
    if files.is_empty() {
        return None;
    }
    // Предмет — то, у чего есть подпись `item_…` и иконка (так отсеиваются описания и рецепты).
    let icons: BTreeSet<&str> = main
        .entries()
        .keys()
        .filter_map(|p| p.strip_prefix("panorama/images/items/")?.strip_suffix("_png.vtex_c").filter(|i| !i.contains('/')))
        .collect();
    let mut tips: Vec<&str> = names.iter().flat_map(|n| n.keys()).filter_map(|k| k.strip_prefix("item_")).filter(|id| !id.starts_with("recipe_")).collect();
    tips.sort();
    tips.dedup();
    let taken: BTreeSet<&str> = tips.iter().copied().filter(|id| icons.contains(id)).collect();
    let items: Vec<(String, String)> = tips.iter().filter_map(|id| Some((id.to_string(), item_icon(id, &icons, &taken)?))).collect();
    let english = names.last();
    // Имена способностей из описаний («Passive: Combo Breaker» у Aeon Disk) — звуки часто
    // названы по ним. Общие для нескольких предметов («Passive: Bash») не берём.
    let abilities: Vec<(&str, String)> = items
        .iter()
        .flat_map(|(id, _)| {
            let text = english.and_then(|n| n.get(&format!("item_{id}_description"))).map_or("", String::as_str);
            item_abilities(text).into_iter().map(move |a| (id.as_str(), label_base(&a)))
        })
        .collect();
    let candidates: Vec<Candidate> = items
        .iter()
        .map(|(id, _)| {
            let mut bases = vec![id.to_string()];
            bases.extend(english.and_then(|n| n.get(&format!("item_{id}"))).map(|l| label_base(l)));
            bases.extend(ITEM_ALIASES.iter().filter(|(_, item)| item == id).map(|(alias, _)| alias.to_string()));
            let unique = |base: &String| abilities.iter().filter(|(_, b)| b == base).count() == 1;
            bases.extend(abilities.iter().filter(|(owner, b)| owner == id && unique(b)).map(|(_, b)| b.clone()));
            Candidate { key: id.to_string(), bases }
        })
        .collect();
    let mut files = classify(&files, &candidates, false);
    // Не узнанный по имени файла звук — по имени события, которое его играет:
    // `blade_fury` играет `DOTA_Item.BattleFury`, `lotus_grant` — `Item.RoshansBanner.Cast`.
    let events = item_events(main);
    for f in files.iter_mut().filter(|f| f.group == OTHER) {
        let Some(names) = events.get(&f.name) else { continue };
        if let Some(found) = classify(names, &candidates, false).into_iter().find(|g| g.group != OTHER) {
            f.group = found.group;
        }
    }
    let label = |id: &str| names.iter().find_map(|n| n.get(&format!("item_{id}")).cloned()).unwrap_or_else(|| heroes::title_case(id));
    let mut groups: Vec<SoundGroup> = items
        .iter()
        .filter(|(id, _)| files.iter().any(|f| &f.group == id))
        .map(|(id, icon)| SoundGroup {
            key: id.clone(),
            label: label(id),
            icon: cache_icon(main, &format!("panorama/images/items/{icon}_png.vtex_c"), cache_dir, &format!("item_{id}")),
        })
        .collect();
    groups.sort_by_key(|g| g.label.to_lowercase());
    if files.iter().any(|f| f.group == OTHER) {
        groups.push(SoundGroup { key: OTHER.to_owned(), label: tr!("Прочие звуки предметов", "Other item sounds").to_owned(), icon: None });
    }
    Some(SoundSet { folder: ITEM_SOUNDS.to_owned(), groups, files })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn cands(list: &[(&str, &[&str])]) -> Vec<Candidate> {
        list.iter().map(|(k, b)| Candidate { key: k.to_string(), bases: b.iter().map(|s| s.to_string()).collect() }).collect()
    }

    #[test]
    fn axe_sounds_by_ability() {
        let files = names(&[
            "attack01", "attack_automaton01", "battle_hunger", "berserkers_call", "berserkers_shoutmask", "counterhelix",
            "counterhelix_blood_chaser", "culling_blade_fail", "culling_blade_success", "jungle_dunk_success", "preattack02",
        ]);
        let abilities = cands(&[
            ("axe_berserkers_call", &["berserkers_call"]),
            ("axe_battle_hunger", &["battle_hunger"]),
            ("axe_counter_helix", &["counter_helix"]),
            ("axe_culling_blade", &["culling_blade"]),
        ]);
        let got: Vec<(String, String, bool)> = classify(&files, &abilities, true).into_iter().map(|f| (f.name, f.group, f.skin)).collect();
        let want = [
            ("attack01", "attack", false),
            // Удар автоматона — не облик, а звук его карточки.
            ("attack_automaton01", "attack", false),
            ("battle_hunger", "axe_battle_hunger", false),
            ("berserkers_call", "axe_berserkers_call", false),
            // Уникальное слово «berserkers» — Berserker's Call, «shoutmask» — облик.
            ("berserkers_shoutmask", "axe_berserkers_call", true),
            ("counterhelix", "axe_counter_helix", false),
            ("counterhelix_blood_chaser", "axe_counter_helix", true),
            ("culling_blade_fail", "axe_culling_blade", false),
            ("culling_blade_success", "axe_culling_blade", false),
            ("jungle_dunk_success", "other", true),
            ("preattack02", "attack", false),
        ];
        for ((name, group, skin), (wn, wg, ws)) in got.iter().zip(want) {
            assert_eq!((name.as_str(), group.as_str(), *skin), (wn, wg, ws), "{name}");
        }
        let auto = classify(&files, &abilities, true).into_iter().filter(|f| f.variant.is_some()).map(|f| f.name).collect::<Vec<_>>();
        assert_eq!(auto, ["attack_automaton01"]);
    }

    #[test]
    fn items_prefer_the_most_specific_name() {
        let files = names(&["blink_arcane", "blink_nailed", "dota_item_cyclone", "item_armlet_activate", "black_king_bar", "gale_guard"]);
        let items = cands(&[("blink", &["blink"]), ("arcane_blink", &["arcane_blink"]), ("cyclone", &["cyclone"]), ("armlet", &["armlet"]), ("black_king_bar", &["black_king_bar"])]);
        let got: Vec<(String, bool)> = classify(&files, &items, false).into_iter().map(|f| (f.group, f.skin)).collect();
        assert_eq!(got[0], ("arcane_blink".into(), false));
        assert_eq!(got[1], ("blink".into(), false), "у предметов обликов нет");
        assert_eq!(got[2], ("cyclone".into(), false));
        assert_eq!(got[3], ("armlet".into(), false));
        assert_eq!(got[4], ("black_king_bar".into(), false));
        assert_eq!(got[5], ("other".into(), false), "непривязанный звук предмета — не облик");

        // Хвост из слов самого предмета (`totem`) не мешает, слово другого предмета — мешает.
        let files = names(&["seal_totem_cast", "meteor_fall", "giants_maul_target"]);
        let items = cands(&[("ogre_seal_totem", &["ogre_seal_totem"]), ("meteor_hammer", &["meteor_hammer"]), ("giants_ring", &["giants_ring"]), ("giant_maul", &["giant_maul", "giants_maul"])]);
        let got: Vec<String> = classify(&files, &items, false).into_iter().map(|f| f.group).collect();
        assert_eq!(got, ["ogre_seal_totem", "meteor_hammer", "giant_maul"]);

        // Сокращения: слово звука — начало слова предмета; несколько подходящих — не гадаем.
        let files = names(&["rune_regen", "rune_invis", "mek_cast", "river_paint"]);
        let items = cands(&[
            ("bottle_regeneration", &["bottle_regeneration", "rune_regeneration"]),
            ("bottle_invisible", &["bottle_invisible", "rune_invisibility"]),
            ("mekansm", &["mekansm"]),
            ("river_painter", &["river_painter"]),
            ("river_painter2", &["river_painter2"]),
        ]);
        let got: Vec<String> = classify(&files, &items, false).into_iter().map(|f| f.group).collect();
        assert_eq!(got, ["bottle_regeneration", "bottle_invisible", "mekansm", "other"]);
    }

    #[test]
    fn item_icons_and_abilities() {
        let icons: BTreeSet<&str> = ["bottle_invisibility", "bottle_doubledamage", "aeon_disk", "blink"].into();
        let taken: BTreeSet<&str> = ["aeon_disk", "blink"].into();
        assert_eq!(item_icon("aeon_disk", &icons, &taken).as_deref(), Some("aeon_disk"));
        assert_eq!(item_icon("bottle_invisible", &icons, &taken).as_deref(), Some("bottle_invisibility"));
        assert_eq!(item_icon("bottle_double_damage", &icons, &taken).as_deref(), Some("bottle_doubledamage"));
        assert_eq!(item_icon("aeon_disk_description", &icons, &taken), None);
        assert_eq!(item_icon("blink_2", &icons, &taken), None, "иконка другого предмета");
        assert_eq!(event_base("DOTA_Item.BattleFury"), "battle_fury");
        assert_eq!(event_base("Item.RoshansBanner.Cast"), "roshans_banner_cast");
        assert_eq!(event_base("DOTA_Item.Force_Boots.Cast"), "force_boots_cast");
        assert_eq!(event_base("DOTA_Item.MKB.melee"), "mkb_melee");
        assert_eq!(event_base("item_crozier.cast"), "crozier_cast");
        let text = "<h1>Active: Static Charge</h1> text <br><h1>Passive: Chain Lightning</h1>";
        assert_eq!(item_abilities(text), ["Static Charge", "Chain Lightning"]);
    }

    /// Внутреннее имя не похоже на звук — выручает английская подпись или одно уникальное слово.
    #[test]
    fn matching_by_label_and_unique_word() {
        let files = names(&["hook_cast", "hook_retract_stop", "hook_target_persona", "rot", "dispose_cast", "healing_salve", "meteor_impact"]);
        let groups = cands(&[
            ("pudge_meat_hook", &["meat_hook", &label_base("Meat Hook")]),
            ("pudge_rot", &["rot"]),
            ("marci_grapple", &["grapple", &label_base("Dispose")]),
            ("flask", &["flask", &label_base("Healing Salve")]),
            ("invoker_chaos_meteor", &["chaos_meteor"]),
        ]);
        let got: Vec<(String, bool)> = classify(&files, &groups, true).into_iter().map(|f| (f.group, f.skin)).collect();
        assert_eq!(got[0], ("pudge_meat_hook".into(), false));
        assert_eq!(got[1], ("pudge_meat_hook".into(), false));
        // Звук личности — не облик основного героя, а звук её карточки.
        assert_eq!(got[2], ("pudge_meat_hook".into(), false));
        assert_eq!(got[3], ("pudge_rot".into(), false));
        assert_eq!(got[4], ("marci_grapple".into(), false));
        assert_eq!(got[5], ("flask".into(), false));
        assert_eq!(got[6], ("invoker_chaos_meteor".into(), false));
        assert_eq!(label_base("Revenant's Brooch"), "revenants_brooch");
    }

    #[test]
    fn hero_folders() {
        let folders: BTreeSet<String> = ["axe", "nyx", "nightstalker", "underlord", "shadowshaman", "keeper", "vengeful_spirit", "shared", "bristlebog"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let f = |id: &str, vo: &str| hero_folder(id, vo, &folders).map(String::as_str);
        assert_eq!(f("axe", "axe"), Some("axe"));
        assert_eq!(f("nyx_assassin", "nyx_assassin"), Some("nyx"));
        assert_eq!(f("night_stalker", "nightstalker"), Some("nightstalker"));
        assert_eq!(f("abyssal_underlord", "abyssal_underlord"), Some("underlord"));
        assert_eq!(f("shadow_shaman", "shadowshaman"), Some("shadowshaman"));
        assert_eq!(f("keeper_of_the_light", "keeper_of_the_light"), Some("keeper"));
        assert_eq!(f("vengefulspirit", "vengefulspirit"), Some("vengeful_spirit"));
        assert_eq!(f("bristleback", "bristleback"), Some("bristlebog"));
        assert_eq!(f("lina", "lina"), None);
    }

    #[test]
    fn script_and_tooltips() {
        let script = r#"
            "Ability1"  "axe_berserkers_call"
            "Ability2"  "axe_battle_hunger"
            "Ability4"  "generic_hidden"
            "Ability10" "special_bonus_unique_axe"
            "AbilityTalentStart" "10"
        "#;
        assert_eq!(hero_abilities(script), ["axe_berserkers_call", "axe_battle_hunger"]);
        let loc = r##"
            "DOTA_Tooltip_ability_axe_berserkers_call"   "Berserker's Call"
            "DOTA_Tooltip_Ability_item_black_king_bar"   "#|m|#Black King Bar"
            "DOTA_Tooltip_ability_item_kobold_cup:n"     "Kobold Cup"
        "##;
        let names = tooltip_names(loc);
        assert_eq!(names["axe_berserkers_call"], "Berserker's Call");
        assert_eq!(names["item_black_king_bar"], "Black King Bar");
        assert_eq!(names["item_kobold_cup"], "Kobold Cup");
    }
}
