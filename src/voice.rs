//! Чья фраза в папке героя: самого героя, его личности, автоматона, арканы или набора.
//!
//! Всё лежит вперемешку в одной папке `sounds/vo/<папка>/` и отличается началом имени:
//! `anti_kill_01` — Anti-Mage, `amp_wei_kill_01` — его личность Вэй, `auto_axe_kill_01` —
//! Axe-автоматон, `drow_arc_move_19` — аркана Drow Ranger, `jung_axe_kill_01` — набор
//! Bare Knuckle. Личность и автоматон в игре — отдельные «герои» со своим портретом и
//! именем (`npc_dota_hero_antimage_persona1`, `npc_dota_hero_axe_carnival`); арканы и
//! наборы надеваются на основного героя.
//!
//! Базовые фразы бывают с разным началом (`drag_` и `dragon_` у Dragon Knight), поэтому
//! незнакомое начало — тоже база: прячем только то, что узнали точно.

use serde::{Deserialize, Serialize};

use crate::tr;

/// Отдельная карточка героя с той же папкой озвучки.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Variant {
    /// Личность: Anti-Mage (Вэй), Pudge (плюшевый)…
    Persona,
    /// Автоматон: Axe (автоматон)…
    Automaton,
}

impl Variant {
    pub const ALL: [Variant; 2] = [Variant::Persona, Variant::Automaton];

    /// Как называется в игре: `antimage` → `antimage_persona1`, `axe` → `axe_carnival`.
    pub fn hero_id(self, base: &str) -> String {
        match self {
            Variant::Persona => format!("{base}_persona1"),
            Variant::Automaton => format!("{base}_carnival"),
        }
    }

    /// Обратно: `antimage_persona1` → (`antimage`, личность).
    pub fn of_hero_id(id: &str) -> Option<(&str, Variant)> {
        Self::ALL.into_iter().find_map(|v| Some((id.strip_suffix(v.hero_id("").as_str())?, v)))
    }

    /// Для ключей счётчиков: `axe:automaton`.
    pub fn key(self) -> &'static str {
        match self {
            Variant::Persona => "persona",
            Variant::Automaton => "automaton",
        }
    }

    /// Плашка на карточке.
    pub fn label(self) -> &'static str {
        match self {
            Variant::Persona => tr!("личность", "persona"),
            Variant::Automaton => tr!("автоматон", "automaton"),
        }
    }
}

/// Аркана или набор со своей озвучкой — на основном герое.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skin {
    Arcana,
    Set,
}

impl Skin {
    pub fn label(self) -> &'static str {
        match self {
            Skin::Arcana => tr!("аркана", "arcana"),
            Skin::Set => tr!("набор", "set"),
        }
    }
}

/// Чья фраза.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    Hero,
    Variant(Variant),
    /// Аркана или набор; `section` — общий ключ всех его фраз (`drow_arc`, `jung_axe`).
    Skin { skin: Skin, section: String },
}

/// Начала фраз личностей по папкам озвучки. Второе поле — подраздел внутри личности
/// (у Dragon Knight фразы человека и дракона), пустое — без подраздела.
const PERSONAS: &[(&str, &[(&str, &str)])] = &[
    ("antimage", &[("amp_wei_", "")]),
    ("crystalmaiden", &[("cm_wolf_", "")]),
    ("dragon_knight", &[("dk_davion_", ""), ("dk_slyrak_", "slyrak")]),
    ("invoker", &[("kidvoker_", ""), ("kidvo_la_", "")]),
    ("mirana", &[("mira_per_", "")]),
    ("phantom_assassin", &[("pa_asan_", "")]),
    ("pudge", &[("toy_pug_", "")]),
];

/// Фразы автоматона: `auto_axe_kill_01` (после `auto_` — начало героя).
const AUTOMATON: &str = "auto_";

/// Арканы и наборы, которых не узнать по `<герой>_arc_…`: начало → вид, раздел, название.
const SKINS: &[(&str, Skin, &str, &str)] = &[
    ("jung_axe_", Skin::Set, "jung_axe", "Bare Knuckle"),
    ("helmet_sylph_", Skin::Set, "helmet", ""),
    ("helmet_snapfire_", Skin::Set, "helmet", ""),
    ("helmet_snip_", Skin::Set, "helmet", ""),
    ("imperia_", Skin::Set, "imperia", ""),
    ("queen_imperia_", Skin::Set, "imperia", ""),
    ("venge_crown_", Skin::Set, "venge_crown", ""),
    ("monkey_crown_", Skin::Set, "monkey_crown", ""),
    ("skywrath_crown_", Skin::Set, "skywrath_crown", ""),
    ("eth_muerta_", Skin::Set, "eth_muerta", ""),
    ("tiny_pres_", Skin::Set, "tiny_pres", ""),
    ("fs_spectre_wwa_", Skin::Set, "fs_spectre", ""),
    ("spectre_wwa_", Skin::Set, "spectre_wwa", ""),
    ("noba_tech_", Skin::Set, "noba_tech", ""),
    ("ogr_mou_", Skin::Set, "ogr_mou", ""),
    ("legcom_econ_", Skin::Set, "legcom_econ", ""),
    ("legcom_dem_", Skin::Arcana, "legcom_dem", ""),
    ("rz_vsa_", Skin::Arcana, "rz_vsa", ""),
    ("qop_scream_", Skin::Arcana, "qop_arc", ""),
];

/// Разбор имени фразы из папки `folder`: чья она и что остаётся после её начала —
/// `amp_wei_kill_01` → (личность, `kill_01`), `drow_arc_move_19` → (аркана, `move_19`).
/// У фраз героя остаток — имя целиком (начало героя снимает уже группировка).
pub fn split<'a>(folder: &str, name: &'a str) -> (Part, &'a str) {
    if let Some((_, prefixes)) = PERSONAS.iter().find(|(f, _)| *f == folder) {
        for (prefix, _) in *prefixes {
            if let Some(rest) = name.strip_prefix(prefix) {
                return (Part::Variant(Variant::Persona), rest);
            }
        }
    }
    if let Some(rest) = name.strip_prefix(AUTOMATON) {
        // `auto_axe_kill_01` → `kill_01`.
        let rest = rest.split_once('_').map_or(rest, |(_, r)| r);
        return (Part::Variant(Variant::Automaton), rest);
    }
    for (prefix, skin, section, _) in SKINS {
        if let Some(rest) = name.strip_prefix(prefix) {
            return (Part::Skin { skin: *skin, section: (*section).to_owned() }, rest);
        }
    }
    // `drow_arc_move_19`, `earth_arcana_…`, `mk_arcana_…` — аркана по второму слову.
    let mut words = name.splitn(3, '_');
    if let (Some(first), Some(second @ ("arc" | "arcana")), Some(rest)) = (words.next(), words.next(), words.next()) {
        return (Part::Skin { skin: Skin::Arcana, section: format!("{first}_{second}") }, rest);
    }
    (Part::Hero, name)
}

/// Подраздел фраз личности (`slyrak` у дракона Dragon Knight), если он есть.
pub fn persona_section(folder: &str, name: &str) -> Option<&'static str> {
    let (_, prefixes) = PERSONAS.iter().find(|(f, _)| *f == folder)?;
    prefixes.iter().find(|(p, _)| name.starts_with(p)).map(|(_, s)| *s).filter(|s| !s.is_empty())
}

/// Чья карточка: папка героя или `папка:persona` / `папка:automaton`.
pub fn card_key(folder: &str, variant: Option<Variant>) -> String {
    match variant {
        None => folder.to_owned(),
        Some(v) => format!("{folder}:{}", v.key()),
    }
}

/// Карточка, к которой относится фраза.
pub fn card_of(folder: &str, name: &str) -> String {
    match split(folder, name).0 {
        Part::Variant(v) => card_key(folder, Some(v)),
        _ => card_key(folder, None),
    }
}

/// Заголовок раздела: «Аркана», «Набор Bare Knuckle», у личности — подраздел как есть.
pub fn section_label(section: &str) -> String {
    if let Some((_, skin, _, name)) = SKINS.iter().find(|(_, _, s, _)| *s == section) {
        let kind = match skin {
            Skin::Arcana => tr!("Аркана", "Arcana"),
            Skin::Set => tr!("Набор", "Set"),
        };
        return if name.is_empty() { format!("{kind} · {section}") } else { format!("{kind} {name}") };
    }
    if section.ends_with("_arc") || section.ends_with("_arcana") {
        return format!("{} · {section}", tr!("Аркана", "Arcana"));
    }
    match Variant::ALL.into_iter().find(|v| v.key() == section) {
        Some(Variant::Persona) => tr!("Личность", "Persona").to_owned(),
        Some(Variant::Automaton) => tr!("Автоматон", "Automaton").to_owned(),
        None => crate::heroes::title_case(section),
    }
}

/// Чей звук способности: `blink_in_persona`, `kid_taunt2025` — личности,
/// `attack_automaton01`, `culling_layer_auto` — автоматона.
pub fn sound_variant(name: &str) -> Option<Variant> {
    let words: Vec<&str> = name.split('_').map(|w| w.trim_end_matches(|c: char| c.is_ascii_digit())).collect();
    if words.contains(&"persona") || words.first() == Some(&"kid") {
        Some(Variant::Persona)
    } else if words.iter().any(|w| *w == "automaton" || *w == "auto") {
        Some(Variant::Automaton)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(folder: &str, name: &str) -> Part {
        split(folder, name).0
    }

    #[test]
    fn personas_automatons_and_skins() {
        assert_eq!(split("antimage", "amp_wei_kill_01"), (Part::Variant(Variant::Persona), "kill_01"));
        assert_eq!(split("antimage", "anti_kill_01"), (Part::Hero, "anti_kill_01"));
        assert_eq!(split("axe", "auto_axe_kill_01"), (Part::Variant(Variant::Automaton), "kill_01"));
        assert_eq!(split("axe", "jung_axe_kill_01"), (Part::Skin { skin: Skin::Set, section: "jung_axe".into() }, "kill_01"));
        assert_eq!(split("drowranger", "drow_arc_move_19"), (Part::Skin { skin: Skin::Arcana, section: "drow_arc".into() }, "move_19"));
        // База с разным началом остаётся базой, личность Dragon Knight — в двух видах.
        assert_eq!(part("dragon_knight", "dragon_kill_01"), Part::Hero);
        assert_eq!(part("dragon_knight", "dk_slyrak_kill_01"), Part::Variant(Variant::Persona));
        assert_eq!(persona_section("dragon_knight", "dk_slyrak_kill_01"), Some("slyrak"));
        assert_eq!(persona_section("dragon_knight", "dk_davion_kill_01"), None);
        // `dk_` у другого героя — не личность; `arcwar_` — не аркана.
        assert_eq!(part("pudge", "dk_davion_kill_01"), Part::Hero);
        assert_eq!(part("arc_warden", "arcwar_spark_01"), Part::Hero);
        assert_eq!(part("queenofpain", "qop_scream_count_01"), part("queenofpain", "qop_arc_kill_01"));
    }

    #[test]
    fn cards_and_ids() {
        assert_eq!(card_of("axe", "auto_axe_kill_01"), "axe:automaton");
        assert_eq!(card_of("axe", "jung_axe_kill_01"), "axe");
        assert_eq!(Variant::Persona.hero_id("antimage"), "antimage_persona1");
        assert_eq!(Variant::of_hero_id("axe_carnival"), Some(("axe", Variant::Automaton)));
        assert_eq!(Variant::of_hero_id("axe"), None);
    }

    #[test]
    fn ability_sounds() {
        assert_eq!(sound_variant("blink_in_persona"), Some(Variant::Persona));
        assert_eq!(sound_variant("dismember_target_persona01"), Some(Variant::Persona));
        assert_eq!(sound_variant("kid_taunt2025"), Some(Variant::Persona));
        assert_eq!(sound_variant("attack_automaton01"), Some(Variant::Automaton));
        assert_eq!(sound_variant("culling_layer_auto"), Some(Variant::Automaton));
        assert_eq!(sound_variant("berserkers_call"), None);
    }
}
