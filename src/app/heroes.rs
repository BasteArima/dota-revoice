//! Экран «Герои»: сетка портретов из игры. Клик — фразы героя достаются из пака
//! озвучки в рабочую папку, и открывается «Запись». Личности и автоматоны — своими
//! карточками сразу за героем. Здесь же — каталог игры (герои, звуки способностей и
//! предметов) и распаковка звуков для «Записи» и «Предметов».

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use dota_revoice::heroes::{self, Catalog, Hero};
use dota_revoice::project::View;
use dota_revoice::sfx::{self, SoundSet};
use dota_revoice::voice::Variant;
use eframe::egui::{self, Align, Align2, Color32, FontId, Layout, RichText, Sense, Stroke, TextureHandle, vec2};

use super::settings::choice_chip;
use super::theme::{self, BG_DEEP, CARD, CARD_HOVER, FAINT, GOLD, GREEN, LINE, MUTED, RED, TEXT, icon};
use super::{App, Screen};
use crate::config::UiLanguage;
use dota_revoice::{tr, trf};

/// Как часто пересчитывать «записано», пока открыт экран.
const COUNTS_REFRESH: Duration = Duration::from_secs(3);
const CARD_MAX: f32 = 138.0;
const GAP: f32 = 10.0;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Filter {
    #[default]
    All,
    Started,
    Untouched,
}

/// Откуда брать героев: основной пак, пак озвучки, язык имён, куда класть кеш.
#[derive(Clone, PartialEq, Eq)]
struct Source {
    main_pak: PathBuf,
    voice_pak: PathBuf,
    /// Папка оригиналов этого языка озвучки.
    originals: PathBuf,
    /// Оригиналы звуков способностей и предметов — из основного пака, от языка не зависят.
    sfx_originals: PathBuf,
    name_lang: &'static str,
    cache: PathBuf,
}

/// Что открыть в «Записи».
#[derive(Clone)]
pub(super) enum Target {
    /// Фразы героя.
    Voice(Hero),
    /// Звуки его способностей.
    Abilities(Hero),
    /// Звуки предметов; `Some` — сразу на этом предмете.
    Items(Option<String>),
}

impl Target {
    /// Чем подписать карточку, пока звуки достаются: карточка героя или `items`.
    fn key(&self) -> String {
        match self {
            Target::Voice(h) | Target::Abilities(h) => h.card_key(),
            Target::Items(item) => format!("items:{}", item.as_deref().unwrap_or_default()),
        }
    }

    /// Чьи фразы или звуки открыть.
    fn view(&self) -> View {
        match self {
            Target::Voice(h) | Target::Abilities(h) => h.variant.map_or(View::Hero, View::Variant),
            Target::Items(_) => View::All,
        }
    }
}

#[derive(Default)]
pub(super) struct HeroesState {
    list: Option<Vec<Hero>>,
    pub(super) items: Option<SoundSet>,
    source: Option<Source>,
    scan: Option<mpsc::Receiver<Result<Catalog, String>>>,
    error: Option<String>,
    /// Портреты по id героя; `None` — портрета нет, рисуем заглушку.
    textures: HashMap<String, Option<TextureHandle>>,
    /// Иконки способностей и предметов по id.
    icons: HashMap<String, Option<TextureHandle>>,
    counts: HashMap<String, usize>,
    counts_at: Option<Instant>,
    search: String,
    filter: Filter,
    /// Что сейчас достаётся из пака.
    opening: Option<Opening>,
}

/// Звуки, которые достаются из пака в фоне.
struct Opening {
    /// Чья карточка подписана «достаю…»: `Hero::card_key` или `items:<предмет>`.
    key: String,
    /// Папка оригиналов и та же папка в английской озвучке (для аркан и наборов).
    rx: mpsc::Receiver<Result<(PathBuf, Option<PathBuf>), String>>,
    /// Группа, на которой встать после открытия (предмет).
    group: Option<String>,
    view: View,
    /// Id героя, личности или автоматона; у предметов — нет.
    card: Option<String>,
}

impl HeroesState {
    pub(super) fn busy(&self) -> bool {
        self.scan.is_some() || self.opening.is_some()
    }

    /// Пересчитать «записано» на следующем кадре (после импорта пачки).
    pub(super) fn refresh_counts(&mut self) {
        self.counts_at = None;
    }

    /// Что сейчас достаётся (для подписи «достаю…» на карточке).
    pub(super) fn opening_key(&self) -> Option<&str> {
        self.opening.as_ref().map(|o| o.key.as_str())
    }

    pub(super) fn list(&self) -> Option<&[Hero]> {
        self.list.as_deref()
    }

}

impl App {
    fn hero_source(&self) -> Option<Source> {
        let dota = self.dota.as_ref()?;
        let main_pak = dota.game_dir.join("dota").join("pak01_dir.vpk");
        // Английская озвучка лежит в основном паке.
        let voice_pak = match dota.voice_language {
            Some(_) => dota.voice_pak()?.path,
            None => main_pak.clone(),
        };
        let library = self.config.library_dir();
        let lang = dota.voice_language.as_deref().unwrap_or("english");
        Some(Source {
            main_pak,
            voice_pak,
            originals: library.join("originals").join(lang),
            sfx_originals: library.join("originals").join("sfx"),
            name_lang: match self.config.ui_language {
                UiLanguage::Russian => "russian",
                UiLanguage::English => "english",
            },
            cache: library.join("cache"),
        })
    }

    /// Каждый кадр: запустить разбор паков, если игра или озвучка сменились, и забрать результаты.
    pub(super) fn poll_heroes(&mut self, ctx: &egui::Context) {
        let source = self.hero_source();
        if source != self.heroes.source {
            self.heroes.source = source.clone();
            self.heroes.list = None;
            self.heroes.items = None;
            self.heroes.error = None;
            self.heroes.textures.clear();
            self.heroes.icons.clear();
            self.heroes.scan = source.map(|s| {
                let (tx, rx) = mpsc::channel();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let result = heroes::load_or_scan(&s.main_pak, &s.voice_pak, s.name_lang, &s.cache);
                    let _ = tx.send(result.map_err(|e| format!("{e:#}")));
                    ctx.request_repaint();
                });
                rx
            });
        }
        if let Some(rx) = &self.heroes.scan
            && let Ok(result) = rx.try_recv()
        {
            self.heroes.scan = None;
            match result {
                Ok(catalog) => {
                    self.heroes.list = Some(catalog.heroes);
                    self.heroes.items = catalog.items;
                    // Звуки, открытые до разбора (при запуске), — по способностям и предметам.
                    self.apply_catalog_to_project();
                }
                Err(e) => self.heroes.error = Some(e),
            }
        }
        if let Some(o) = &self.heroes.opening
            && let Ok(result) = o.rx.try_recv()
        {
            let Some(Opening { group, view, card, .. }) = self.heroes.opening.take() else { return };
            match result {
                Ok((dir, english)) => {
                    self.open_project_view(&dir, english.as_deref(), view, card);
                    if self.project.as_ref().is_some_and(|p| p.source_dir == dir) {
                        if let Some(group) = group
                            && !self.select_group(&group)
                        {
                            let name = self.heroes.items.as_ref().and_then(|s| s.group(&group)).map_or(group.clone(), |g| g.label.clone());
                            self.error(trf!("Звуки «{name}» не достались из игры", "Couldn't extract the sounds of \"{name}\" from the game"));
                        }
                        self.screen = Screen::Record;
                    }
                }
                Err(e) => self.error(trf!("Звуки не достались из игры: {e}", "Couldn't extract the sounds from the game: {e}")),
            }
        }
    }

    /// Имя героя по папке озвучки: из списка игры, иначе из самой папки.
    pub(super) fn hero_name(&self, vo: &str) -> String {
        self.heroes
            .list
            .as_ref()
            .and_then(|l| l.iter().find(|h| h.vo == vo && h.variant.is_none()))
            .map(|h| h.name.clone())
            .unwrap_or_else(|| heroes::title_case(vo))
    }

    /// Карточка открытого проекта: та, с которой его открыли, иначе — основной герой
    /// по папке фраз или звуков способностей.
    pub(super) fn project_hero(&self) -> Option<&Hero> {
        let p = self.project.as_ref()?;
        let list = self.heroes.list.as_ref()?;
        if let Some(card) = &self.config.last_card {
            return list.iter().find(|h| &h.id == card);
        }
        list.iter().find(|h| {
            h.variant.is_none() && (p.resource_dir == format!("sounds/vo/{}", h.vo) || h.sounds.as_ref().is_some_and(|s| s.folder == p.resource_dir))
        })
    }

    /// Раскладка звуков из каталога для папки ресурса: способности героя или предметы.
    pub(super) fn sound_set_for(&self, resource_dir: &str) -> Option<&SoundSet> {
        if resource_dir == sfx::ITEM_SOUNDS {
            return self.heroes.items.as_ref();
        }
        self.heroes.list.as_ref()?.iter().find_map(|h| h.sounds.as_ref().filter(|s| s.folder == resource_dir))
    }

    /// Каталог пришёл позже, чем открылся проект со звуками, — разложить его по группам.
    fn apply_catalog_to_project(&mut self) {
        let Some(dir) = self.project.as_ref().filter(|p| p.is_sounds()).map(|p| p.resource_dir.clone()) else { return };
        let Some(set) = self.sound_set_for(&dir).cloned() else { return };
        if let Some(p) = self.project.as_mut() {
            let view = p.view;
            p.apply_sound_set(&set, view);
        }
        self.rebuild_groups();
        // Звуки чужой карточки ушли из списка — выбранный мог пропасть.
        if self.project.as_ref().is_some_and(|p| self.selected >= p.lines.len()) {
            let first = self.visible_order().first().copied().unwrap_or(0);
            self.select(first);
        }
    }

    /// Достаёт звуки из игры в фоне и открывает их в «Записи».
    pub(super) fn open_sounds(&mut self, ctx: &egui::Context, target: Target) {
        let Some(s) = self.heroes.source.clone() else { return };
        if self.heroes.opening.is_some() {
            return;
        }
        // `english` — та же папка в английской озвучке основного пака: арканы и наборы.
        let (pak, folder, out, english, group) = match &target {
            Target::Voice(hero) => {
                let folder = format!("sounds/vo/{}", hero.vo);
                let english = s.originals.with_file_name("english").join(&folder);
                // Нет героя в выбранной озвучке, личность, автоматон — оригинал английский.
                if hero.english_only || hero.variant.is_some() || s.voice_pak == s.main_pak {
                    (s.main_pak.clone(), folder, english, None, None)
                } else {
                    (s.voice_pak.clone(), folder.clone(), s.originals.join(&folder), Some(english), None)
                }
            }
            Target::Abilities(hero) => {
                let Some(set) = &hero.sounds else { return };
                (s.main_pak.clone(), set.folder.clone(), s.sfx_originals.join(&set.folder), None, None)
            }
            Target::Items(item) => (s.main_pak.clone(), sfx::ITEM_SOUNDS.to_owned(), s.sfx_originals.join(sfx::ITEM_SOUNDS), None, item.clone()),
        };
        let main_pak = s.main_pak.clone();
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = heroes::extract_folder(&pak, &folder, &out).map(|_| {
                // Английской папки может не быть (герой только на этом языке) — тогда без аркан.
                let english = english.filter(|dir| heroes::extract_folder(&main_pak, &folder, dir).is_ok());
                (out, english)
            });
            let _ = tx.send(result.map_err(|e| format!("{e:#}")));
            ctx.request_repaint();
        });
        let card = match &target {
            Target::Voice(h) | Target::Abilities(h) => Some(h.id.clone()),
            Target::Items(_) => None,
        };
        self.heroes.opening = Some(Opening { key: target.key(), rx, group, view: target.view(), card });
    }

    /// Иконка способности или предмета из кеша; грузится в видеопамять при первом показе.
    pub(super) fn icon_texture(&mut self, ctx: &egui::Context, id: &str) -> Option<TextureHandle> {
        if let Some(t) = self.heroes.icons.get(id) {
            return t.clone();
        }
        let tex = self
            .heroes
            .source
            .as_ref()
            .and_then(|s| load_png(&sfx::icon_path(&s.cache, id)))
            .map(|img| ctx.load_texture(format!("icon_{id}"), img, egui::TextureOptions::LINEAR));
        self.heroes.icons.insert(id.to_owned(), tex.clone());
        tex
    }

    fn open_hero(&mut self, ctx: &egui::Context, hero: &Hero) {
        self.open_sounds(ctx, Target::Voice(hero.clone()));
    }

    pub(super) fn heroes_screen(&mut self, ui: &mut egui::Ui) {
        if self.heroes.counts_at.is_none_or(|t| t.elapsed() > COUNTS_REFRESH) {
            self.heroes.counts = heroes::recorded_counts(&self.config.export_root());
            self.heroes.counts_at = Some(Instant::now());
        }
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(theme::title(tr!("Герои", "Heroes"), 22.0));
                ui.label(theme::small(tr!("Выбери героя — его фразы программа достанет из игры сама", "Pick a hero — the app extracts their lines from the game")));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let custom = theme::ghost(ui, true, trf!("{}  Открыть свою папку…", "{}  Open my own folder…", icon::FOLDER_OPEN));
                if custom.on_hover_text(tr!("Папка со звуками, которых нет в списке: дикторы, курьеры, своя выгрузка", "A folder with sounds not in the list: announcers, couriers, your own export")).clicked() {
                    self.pick_folder();
                }
            });
        });
        ui.add_space(10.0);

        if self.dota.is_none() {
            notice(ui, tr!("Дота не найдена — укажи её папку в настройках.", "Dota not found — set its folder in the settings."), MUTED);
            return;
        }
        if let Some(e) = self.heroes.error.clone() {
            notice(ui, &trf!("Не получилось прочитать героев из игры: {e}", "Couldn't read heroes from the game: {e}"), RED);
            if ui.add(theme::tool(tr!("Попробовать ещё раз", "Try again"))).clicked() {
                self.heroes.source = None;
            }
            return;
        }
        let Some(list) = self.heroes.list.clone() else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(theme::small(tr!("Читаю героев из файлов игры… В первый раз это занимает несколько секунд.", "Reading heroes from the game files… The first time takes a few seconds.")));
            });
            return;
        };
        // Личности и автоматоны можно спрятать чипом.
        let list: Vec<Hero> = list.into_iter().filter(|h| self.config.show_variants || h.variant.is_none()).collect();
        self.heroes_toolbar(ui, &list);
        ui.add_space(10.0);
        let q = self.heroes.search.trim().to_lowercase();
        let shown: Vec<&Hero> = list
            .iter()
            .filter(|h| q.is_empty() || h.name.to_lowercase().contains(&q) || h.id.contains(&q) || h.vo.contains(&q))
            .filter(|h| {
                let done = self.heroes.counts.get(&h.card_key()).copied().unwrap_or(0);
                match self.heroes.filter {
                    Filter::All => true,
                    Filter::Started => done > 0,
                    Filter::Untouched => done == 0,
                }
            })
            .collect();
        if shown.is_empty() {
            notice(ui, tr!("Никого не нашлось — поменяй поиск или фильтр.", "Nobody found — change the search or the filter."), MUTED);
            return;
        }

        let width = ui.available_width();
        let cols = ((width + GAP) / (CARD_MAX + GAP)).ceil().max(1.0) as usize;
        let card_w = ((width - GAP * (cols - 1) as f32) / cols as f32).floor();
        let mut clicked = None;
        let mut share = None;
        egui::ScrollArea::vertical().id_salt("heroes").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(GAP, GAP);
            for row in shown.chunks(cols) {
                ui.horizontal(|ui| {
                    for hero in row {
                        let resp = self.hero_card(ui, hero, card_w);
                        if resp.clicked() {
                            clicked = Some((*hero).clone());
                        }
                        let recorded = self.heroes.counts.get(&hero.card_key()).copied().unwrap_or(0);
                        resp.context_menu(|ui| {
                            let text = trf!("{}  Поделиться фразами героя…", "{}  Share this hero's lines…", icon::EXPORT);
                            let item = ui.add_enabled(recorded > 0, egui::Button::new(text));
                            if item.on_disabled_hover_text(tr!("У героя пока нет записанных фраз", "This hero has no recorded lines yet")).clicked() {
                                // Фразы героя и звуки его способностей — что из этого записано.
                                let mut groups = vec![format!("sounds/vo/{}", hero.vo)];
                                groups.extend(hero.sounds.as_ref().map(|s| s.folder.clone()));
                                share = Some(groups);
                                ui.close_menu();
                            }
                        });
                    }
                });
            }
        });
        if let Some(hero) = clicked {
            let ctx = ui.ctx().clone();
            self.open_hero(&ctx, &hero);
        }
        if let Some(groups) = share {
            self.open_export(&groups);
        }
    }

    fn heroes_toolbar(&mut self, ui: &mut egui::Ui, list: &[Hero]) {
        let done = |h: &Hero| self.heroes.counts.get(&h.card_key()).copied().unwrap_or(0);
        let started = list.iter().filter(|h| done(h) > 0).count();
        let total_done: usize = list.iter().map(done).sum();
        ui.horizontal(|ui| {
            egui::Frame::default().fill(BG_DEEP).stroke(Stroke::new(1.0_f32, LINE)).corner_radius(8).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::MAGNIFYING_GLASS).color(MUTED));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.heroes.search)
                            .frame(false)
                            .hint_text(RichText::new(tr!("Имя героя", "Hero name")).color(FAINT))
                            .desired_width(200.0),
                    );
                });
            });
            ui.add_space(6.0);
            for (filter, label) in [
                (Filter::All, trf!("Все {}", "All {}", list.len())),
                (Filter::Started, trf!("Начатые {started}", "Started {started}")),
                (Filter::Untouched, tr!("Без записей", "Untouched").to_owned()),
            ] {
                if choice_chip(ui, self.heroes.filter == filter, true, &label).clicked() {
                    self.heroes.filter = filter;
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(theme::small(trf!("Записано всего: {total_done}", "Recorded in total: {total_done}")));
                ui.add_space(10.0);
                let hint = tr!(
                    "Личности (Anti-Mage «Вэй», Pudge «плюшевый») и автоматоны — у них своя озвучка и своя карточка",
                    "Personas (Anti-Mage \"Wei\", Toy Butcher Pudge) and automatons — they have their own voice-over and card"
                );
                if theme::chip(ui, self.config.show_variants, tr!("личности и автоматоны", "personas and automatons")).on_hover_text(hint).clicked() {
                    self.config.show_variants = !self.config.show_variants;
                    self.config.save();
                }
            });
        });
    }

    fn hero_card(&mut self, ui: &mut egui::Ui, hero: &Hero, w: f32) -> egui::Response {
        let img_h = (w * 188.0 / 142.0).round();
        let (rect, resp) = ui.allocate_exact_size(vec2(w, img_h + 48.0), Sense::click());
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let key = hero.card_key();
        let opening = self.heroes.opening_key() == Some(key.as_str());
        let current = match &self.config.last_card {
            Some(card) => self.project.is_some() && *card == hero.id,
            None => hero.variant.is_none() && self.project.as_ref().is_some_and(|p| p.hero == hero.vo),
        };
        let hovered = resp.hovered() && self.heroes.opening.is_none();
        let texture = self.portrait(ui.ctx(), &hero.id);
        let p = ui.painter();
        p.rect_filled(rect, 9.0, if hovered { CARD_HOVER } else { CARD });

        let img = egui::Rect::from_min_size(rect.min, vec2(w, img_h));
        let top = egui::CornerRadius { nw: 9, ne: 9, sw: 0, se: 0 };
        match texture {
            Some(tex) => {
                egui::Image::from_texture(egui::load::SizedTexture::from_handle(&tex)).corner_radius(top).paint_at(ui, img);
            }
            None => {
                p.rect_filled(img, top, placeholder_color(&hero.id));
                p.text(img.center(), Align2::CENTER_CENTER, initials(&hero.name), FontId::new(30.0, theme::bold()), Color32::from_white_alpha(90));
            }
        }
        if hero.english_only {
            let badge = egui::Rect::from_min_size(img.right_top() + vec2(-34.0, 6.0), vec2(28.0, 18.0));
            p.rect_filled(badge, 6.0, Color32::from_black_alpha(190));
            p.text(badge.center(), Align2::CENTER_CENTER, "EN", FontId::new(11.0, theme::bold()), TEXT);
        }
        // Личность или автоматон — плашка слева сверху.
        if let Some(v) = hero.variant {
            let font = FontId::new(10.5, theme::bold());
            let galley = p.layout_no_wrap(v.label().to_owned(), font, Color32::WHITE);
            let badge = egui::Rect::from_min_size(img.left_top() + vec2(6.0, 6.0), galley.size() + vec2(12.0, 4.0));
            p.rect_filled(badge, 6.0, variant_color(v));
            p.galley(badge.min + vec2(6.0, 2.0), galley, Color32::WHITE);
        }
        if opening {
            p.rect_filled(img, top, Color32::from_black_alpha(170));
            p.text(img.center(), Align2::CENTER_CENTER, tr!("достаю фразы…", "extracting lines…"), FontId::proportional(12.0), GOLD);
            ui.ctx().request_repaint();
        }

        let done = self.heroes.counts.get(&key).copied().unwrap_or(0);
        let x = rect.left() + 8.0;
        let name_y = img.bottom() + 8.0;
        let mut job = egui::text::LayoutJob::simple_singleline(hero.name.clone(), FontId::proportional(13.0), TEXT);
        job.wrap = egui::text::TextWrapping { max_width: w - 16.0, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        p.galley(egui::pos2(x, name_y), ui.painter().layout_job(job), TEXT);
        let count = if done > 0 { format!("{done} / {}", hero.lines) } else { format!("— / {}", hero.lines) };
        p.text(egui::pos2(x, name_y + 19.0), Align2::LEFT_TOP, count, FontId::proportional(11.0), MUTED);
        let bar = egui::Rect::from_min_size(egui::pos2(x, rect.bottom() - 7.0), vec2(w - 16.0, 3.0));
        p.rect_filled(bar, 1.5, BG_DEEP);
        if done > 0 {
            let mut fill = bar;
            fill.set_width((bar.width() * done as f32 / hero.lines.max(1) as f32).clamp(2.0, bar.width()));
            p.rect_filled(fill, 1.5, if done >= hero.lines { GREEN } else { GOLD });
        }
        if hovered || current {
            p.rect_stroke(rect, 9.0, Stroke::new(1.5_f32, if hovered { GOLD } else { GOLD.gamma_multiply(0.6) }), egui::StrokeKind::Inside);
        }
        let hint = if let Some(v) = hero.variant {
            trf!(
                "{} — {}: своя озвучка (только английская), фразы лежат в папке героя {}",
                "{} — {}: own voice-over (English only), the lines live in the hero folder {}",
                hero.name,
                v.label(),
                hero.vo
            )
        } else if hero.english_only {
            trf!("{}: в этой озвучке героя нет, оригинал — английский. Твои фразы заменят его в игре", "{}: not in this voice-over, the original is English. Your lines will replace it in the game", hero.name)
        } else {
            trf!("{} · папка озвучки {}", "{} · voice folder {}", hero.name, hero.vo)
        };
        let resp = resp.on_hover_text(hint);
        if self.heroes.opening.is_none() { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
    }

    /// Портрет из кеша; грузится в видеопамять при первом показе карточки.
    fn portrait(&mut self, ctx: &egui::Context, id: &str) -> Option<TextureHandle> {
        if let Some(t) = self.heroes.textures.get(id) {
            return t.clone();
        }
        let tex = self
            .heroes
            .source
            .as_ref()
            .and_then(|s| load_png(&heroes::portrait_path(&s.cache, id)))
            .map(|img| ctx.load_texture(format!("hero_{id}"), img, egui::TextureOptions::LINEAR));
        self.heroes.textures.insert(id.to_owned(), tex.clone());
        tex
    }
}

pub(super) fn notice(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.add_space(30.0);
    ui.vertical_centered(|ui| ui.label(RichText::new(text).size(14.0).color(color)));
}

fn load_png(path: &Path) -> Option<egui::ColorImage> {
    let mut decoder = png::Decoder::new(std::fs::File::open(path).ok()?);
    decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let bytes = &buf[..info.buffer_size()];
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::GrayscaleAlpha => bytes.as_chunks::<2>().0.iter().flat_map(|&[l, a]| [l, l, l, a]).collect(),
        _ => return None,
    };
    Some(egui::ColorImage::from_rgba_unmultiplied([info.width as usize, info.height as usize], &rgba))
}

/// Цвет плашки личности и автоматона.
fn variant_color(v: Variant) -> Color32 {
    match v {
        Variant::Persona => Color32::from_rgb(112, 80, 190),
        Variant::Automaton => Color32::from_rgb(52, 120, 152),
    }
}

/// Тёмный цвет заглушки, свой у каждого героя.
pub(super) fn placeholder_color(id: &str) -> Color32 {
    let h = id.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    let pick = |shift: u32| 30 + ((h >> shift) % 34) as u8;
    Color32::from_rgb(pick(0), pick(8), pick(16))
}

pub(super) fn initials(name: &str) -> String {
    name.split([' ', '-']).filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase()
}
