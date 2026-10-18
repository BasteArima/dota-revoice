//! Экран «Запись»: список фраз слева, оригинал и моя версия справа.

use dota_revoice::audio::{self, Track};
use dota_revoice::process;
use dota_revoice::project::{self, Status};
use dota_revoice::voice;
use eframe::egui::{self, Align, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, pos2, vec2};

use super::theme::{self, AMBER, BG_DEEP, CARD_HOVER, FAINT, GOLD, GREEN, LINE, MUTED, ON_GOLD, RED, TEXT, WAVE, icon};
use super::build::ago;
use super::{App, Handle, MIN_TRIM_SAMPLES, Pending, TrimDrag, index_to_secs};
use crate::config::PreRoll;
use dota_revoice::{tr, trf};

const WAVE_HEIGHT: f32 = 62.0;

impl App {
    pub(super) fn line_list(&mut self, ui: &mut egui::Ui) {
        if self.project.is_none() {
            ui.add_space(8.0);
            ui.label(theme::small(tr!("Выбери героя на вкладке «Герои»", "Pick a hero on the Heroes tab")));
            return;
        }
        // Фразы героя или звуки его способностей — переключатель над списком.
        if let Some(hero) = self.project_hero().cloned()
            && hero.sound_count() > 0
        {
            let on_sounds = self.project.as_ref().is_some_and(|p| p.is_sounds());
            let opening = self.heroes.opening_key() == Some(hero.card_key().as_str());
            let mut switch = None;
            egui::Frame::default().fill(BG_DEEP).corner_radius(10).inner_margin(egui::Margin::same(3)).show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.horizontal(|ui| {
                    let half = (ui.available_width() - 2.0) / 2.0;
                    for (sounds_mode, text) in [
                        (false, trf!("{}  Фразы {}", "{}  Lines {}", icon::CHAT_TEXT, hero.lines)),
                        (true, trf!("{}  Способности {}", "{}  Abilities {}", icon::LIGHTNING, hero.sound_count())),
                    ] {
                        let on = sounds_mode == on_sounds;
                        let label = RichText::new(text).size(12.5).color(if on { TEXT } else { MUTED });
                        let btn = egui::Button::new(label).fill(if on { theme::CARD } else { Color32::TRANSPARENT }).stroke(Stroke::NONE).corner_radius(8).min_size(vec2(half, 26.0));
                        if ui.add_enabled(!opening, btn).clicked() && !on {
                            switch = Some(sounds_mode);
                        }
                    }
                });
            });
            ui.add_space(6.0);
            if let Some(sounds_mode) = switch {
                let ctx = ui.ctx().clone();
                let target = if sounds_mode { super::heroes::Target::Abilities(hero.clone()) } else { super::heroes::Target::Voice(hero.clone()) };
                self.open_sounds(&ctx, target);
            }
        }
        // Иконки способностей и предметов для заголовков групп — заранее, пока проект не занят.
        let icon_ids: Vec<(String, String)> = match &self.project {
            Some(p) => self.groups.iter().filter_map(|(c, _)| p.group_icon(c).map(|i| (c.clone(), i.to_owned()))).collect(),
            None => Vec::new(),
        };
        let ctx = ui.ctx().clone();
        let icons: std::collections::HashMap<String, egui::TextureHandle> =
            icon_ids.into_iter().filter_map(|(c, id)| self.icon_texture(&ctx, &id).map(|t| (c, t))).collect();
        let Some(p) = &self.project else { return };
        let is_sounds = p.is_sounds();
        egui::TopBottomPanel::bottom("legend").frame(egui::Frame::default()).show_inside(ui, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (status, text) in [(Status::Recorded, tr!("записано", "recorded")), (Status::Redo, tr!("переделать", "redo")), (Status::Empty, tr!("пусто", "empty"))] {
                    let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 3.5, status_color(status));
                    ui.label(theme::small(text));
                    ui.add_space(2.0);
                }
            });
        });

        let mut toggle_skins = false;
        ui.horizontal(|ui| {
            // Чип «арканы и наборы»: снят — в списке только базовые фразы и звуки.
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let hint = tr!(
                    "Фразы и звуки аркан и наборов со своей озвучкой (они надеваются на героя). Сними, чтобы оставить только базовые",
                    "Lines and sounds of arcanas and sets with their own voice-over (worn on the hero). Untick to keep only the base ones"
                );
                // Нет у этого героя ни аркан, ни наборов — нечего и прятать.
                if p.lines.iter().any(|l| l.skin.is_some())
                    && theme::chip(ui, self.config.show_skins, tr!("арканы и наборы", "arcanas and sets")).on_hover_text(hint).clicked()
                {
                    toggle_skins = true;
                }
                egui::Frame::default().fill(BG_DEEP).stroke(Stroke::new(1.0_f32, LINE)).corner_radius(8).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon::MAGNIFYING_GLASS).color(MUTED));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.search)
                                .frame(false)
                                .hint_text(RichText::new(tr!("Имя, категория или заметка", "Name, category or note")).color(FAINT))
                                .desired_width(f32::INFINITY),
                        );
                    });
                });
            });
        });
        ui.add_space(8.0);
        let (done, total) = (p.recorded_count(), p.lines.len());
        ui.horizontal(|ui| {
            ui.label(theme::small(tr!("Записано", "Recorded")));
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| ui.label(theme::small(format!("{done} / {total}"))));
        });
        let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
        ui.painter().rect_filled(bar, 2.0, BG_DEEP);
        let mut filled = bar;
        filled.set_width(bar.width() * done as f32 / total.max(1) as f32);
        ui.painter().rect_filled(filled, 2.0, GREEN);
        ui.add_space(6.0);

        let q = self.search.trim().to_lowercase();
        let scroll = self.scroll_to_selected > 0;
        self.scroll_to_selected = self.scroll_to_selected.saturating_sub(1);
        let mut clicked = None;
        let mut toggle_group = None;
        let mut selected_rect = None;
        // Разделы фраз (арканы, наборы, вид личности): ключ свёрнутого раздела в `collapsed`.
        let section_key = |s: &str| format!("§{s}");
        let out = egui::ScrollArea::vertical().id_salt("lines").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            let mut section: Option<&str> = None;
            for (cat, idx) in &self.groups {
                let shown: Vec<usize> = idx.iter().copied().filter(|&i| self.matches(i, &q)).collect();
                if shown.is_empty() {
                    continue;
                }
                let this = if is_sounds { None } else { project::section_of(cat) };
                if this != section {
                    section = this;
                    if let Some(s) = this {
                        // Сколько фраз в разделе и сколько из них записано — по всем его группам.
                        let all: Vec<usize> = self
                            .groups
                            .iter()
                            .filter(|(c, _)| project::section_of(c) == Some(s))
                            .flat_map(|(_, idx)| idx.iter().copied())
                            .filter(|&i| self.matches(i, &q))
                            .collect();
                        let rec = all.iter().filter(|&&i| p.status(&p.lines[i]) != Status::Empty).count();
                        let open = !q.is_empty() || !self.collapsed.contains(&section_key(s));
                        ui.add_space(10.0);
                        if section_header(ui, &voice::section_label(s), rec, all.len(), open).clicked() {
                            toggle_group = Some(section_key(s));
                        }
                    }
                }
                if let Some(s) = section
                    && q.is_empty()
                    && self.collapsed.contains(&section_key(s))
                    && !(scroll && shown.contains(&self.selected))
                {
                    continue;
                }
                let rec = shown.iter().filter(|&&i| p.status(&p.lines[i]) != Status::Empty).count();
                // При поиске группы всегда раскрыты; выбранную фразу не прячем.
                let open = !q.is_empty() || !self.collapsed.contains(cat) || (scroll && shown.contains(&self.selected));
                ui.add_space(6.0);
                if group_header(ui, &p.group_label(cat), icons.get(cat), rec, shown.len(), open).clicked() {
                    toggle_group = Some(cat.clone());
                }
                if !open {
                    continue;
                }
                for i in shown {
                    let line = &p.lines[i];
                    let note = match (p.note(line), line.skin) {
                        ("", Some(skin)) => skin.label(),
                        (note, _) => note,
                    };
                    let resp = list_row(ui, &line.short, note, p.status(line), i == self.selected);
                    if resp.clicked() {
                        clicked = Some(i);
                    }
                    if i == self.selected {
                        selected_rect = Some(resp.rect);
                    }
                }
            }
        });
        // Докручиваем только сам список. Общий `scroll_to_me` egui задевал и правую
        // прокручиваемую часть — она съезжала вниз при выборе фразы.
        if scroll
            && let Some(r) = selected_rect
            && !out.inner_rect.shrink2(vec2(0.0, 24.0)).contains_rect(r)
        {
            let mut state = out.state;
            let max = (out.content_size.y - out.inner_rect.height()).max(0.0);
            state.offset.y = (state.offset.y + r.center().y - out.inner_rect.center().y).clamp(0.0, max);
            state.store(ui.ctx(), out.id);
            ui.ctx().request_repaint();
        }
        if toggle_skins {
            self.config.show_skins = !self.config.show_skins;
            self.config.save();
        }
        if let Some(cat) = toggle_group
            && !self.collapsed.remove(&cat)
        {
            self.collapsed.insert(cat);
        }
        if let Some(i) = clicked {
            self.save_note();
            self.select(i);
            self.scroll_to_selected = 0;
        }
    }

    pub(super) fn editor(&mut self, ui: &mut egui::Ui) {
        let Some(line) = self.current_line() else {
            ui.vertical_centered(|ui| {
                ui.add_space(150.0);
                ui.label(theme::title(tr!("Выбери героя", "Pick a hero"), 22.0));
                ui.label(theme::small(tr!("Фразы программа достанет из игры сама", "The app extracts the lines from the game")));
                ui.add_space(14.0);
                if ui.add(theme::primary(trf!("{}  К героям", "{}  To heroes", icon::USERS_THREE))).clicked() {
                    self.screen = super::Screen::Heroes;
                }
                if theme::ghost(ui, true, tr!("или открыть свою папку со звуками…", "or open your own sound folder…")).clicked() {
                    self.pick_folder();
                }
            });
            return;
        };
        let status = self.project.as_ref().map(|p| p.status(&line)).unwrap_or(Status::Empty);
        let recording = self.is_recording();
        let now = self.player.as_ref().and_then(|p| p.now_playing());
        let head = |t: Track| now.filter(|(nt, _)| *nt == t).map(|(_, f)| f);

        // Заголовок: имя фразы, статус, навигация.
        ui.horizontal(|ui| {
            ui.label(RichText::new(line.name.as_str()).font(FontId::monospace(19.0)).color(TEXT));
            match status {
                Status::Empty => theme::pill(ui, tr!("пусто", "empty"), MUTED),
                Status::Recorded => theme::pill(ui, tr!("записано", "recorded"), GREEN),
                Status::Redo => theme::pill(ui, tr!("переделать", "redo"), AMBER),
            }
            // Версия из пачки друга: от кого и правил ли я её.
            if let Some((origin, edited)) = self.project.as_ref().and_then(|p| p.origin(&line)) {
                let who = super::share::short_author(&origin.author);
                // Имя без падежа: «от Петя» не скажешь, поэтому — значок и имя, суть — в подсказке.
                let (text, hint) = if edited {
                    (trf!("{}  {who} · с правками", "{}  {who} · edited", icon::USER), trf!("Версия из пачки, автор — {who}; ты её менял", "A version from a bundle by {who}; you've changed it"))
                } else {
                    (format!("{}  {who}", icon::USER), trf!("Версия из пачки, автор — {who}", "A version from a bundle by {who}"))
                };
                let resp = ui.scope(|ui| theme::pill(ui, &text, GOLD)).response;
                resp.on_hover_text(hint);
            }
            if status != Status::Empty && !recording {
                ui.add_space(6.0);
                let redo = status == Status::Redo;
                let (text, color) = if redo { (tr!("Готово, не переделывать", "Done, no redo"), AMBER) } else { (tr!("Пометить: переделать", "Mark: redo"), MUTED) };
                if theme::ghost_colored(ui, true, format!("{}  {text}", icon::FLAG), color).clicked() {
                    self.set_redo(!redo);
                }
                if theme::ghost(ui, true, trf!("{}  В корзину", "{}  To trash", icon::TRASH)).on_hover_text(tr!("Моя версия уйдёт в export/.trash (Del)", "My version goes to export/.trash (Del)")).clicked() {
                    self.trash_current();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if theme::ghost(ui, true, trf!("Следующая пустая  {}", "Next empty  {}", icon::SKIP_FORWARD)).on_hover_text("N").clicked() {
                    self.next_unrecorded();
                }
                if ui.add(egui::Button::new(icon::CARET_RIGHT).min_size(vec2(30.0, 28.0))).on_hover_text(tr!("Следующая (↓)", "Next (↓)")).clicked() {
                    self.step(1);
                }
                if ui.add(egui::Button::new(icon::CARET_LEFT).min_size(vec2(30.0, 28.0))).on_hover_text(tr!("Предыдущая (↑)", "Previous (↑)")).clicked() {
                    self.step(-1);
                }
            });
        });
        let orig_secs = self.original.as_ref().map_or(0.0, |c| c.duration_secs());
        ui.horizontal(|ui| {
            ui.label(theme::small(trf!("{} · {orig_secs:.1} с ·", "{} · {orig_secs:.1} s ·", self.project.as_ref().map(|p| p.category_label(&line.category)).unwrap_or_default())));
            ui.label(RichText::new(icon::PENCIL_SIMPLE).size(12.0).color(FAINT));
            // Заметка: тихая строка, которую можно поправить в любой момент.
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.note)
                    .frame(false)
                    .font(FontId::proportional(12.0))
                    .text_color(MUTED)
                    .hint_text(RichText::new(tr!("заметка для себя: что это за фраза", "a note for yourself: what this line is")).size(12.0).color(FAINT))
                    .desired_width(f32::INFINITY),
            );
            if resp.lost_focus() {
                self.save_note();
            }
        });
        ui.add_space(12.0);

        // Обе волны в одном масштабе времени — сразу видно, длиннее ли моя версия.
        let live = recording.then(|| self.recorder.as_ref().map(|r| r.live_peaks(1024))).flatten();
        let mine_secs = match &live {
            Some((_, secs)) => *secs,
            None => self.mine_full.as_ref().map_or(0.0, |c| c.duration_secs()),
        };
        let timeline = orig_secs.max(mine_secs).max(0.1);

        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::small(tr!("Оригинал", "Original")));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if let Some(c) = &self.original {
                        ui.label(theme::small(format!("{:.1} LUFS", c.loudness)));
                    }
                });
            });
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), WAVE_HEIGHT), Sense::click());
            let inner = theme::paint_bars(ui, rect, self.original.as_ref().map(|c| c.peaks.as_slice()), orig_secs / timeline, WAVE, tr!("Оригинал не загружен", "Original not loaded"));
            if let Some(f) = head(Track::Original) {
                playhead(ui, rect, inner.left() + inner.width() * (orig_secs / timeline) * f);
            }
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                self.toggle_play(Track::Original);
            }
        });
        ui.add_space(10.0);

        theme::card_with(ui, if recording { RED } else { LINE }, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr!("Моя версия", "My version")).size(12.0).color(TEXT));
                if !self.takes.is_empty() && !recording {
                    self.takes_menu(ui);
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if recording {
                        ui.label(RichText::new(trf!("●  запись {mine_secs:.1} с", "●  recording {mine_secs:.1} s")).size(12.0).color(RED));
                    } else if let Some(pending) = self.pending {
                        let text = match pending {
                            Pending::Countdown { started, .. } => {
                                let left = if started.is_nan() { 3.0 } else { 3.0 - (ui.input(|i| i.time) - started) };
                                trf!("приготовься… {}", "get ready… {}", left.ceil().max(1.0))
                            }
                            Pending::Original { .. } => tr!("слушай оригинал, запись начнётся после него", "listen to the original, recording starts after it").to_owned(),
                        };
                        ui.label(RichText::new(text).size(12.0).color(AMBER));
                    } else if let (Some(m), Some(o)) = (&self.mine, &self.original) {
                        let diff = m.loudness - o.loudness;
                        let (hint, color) = if diff.abs() < 2.0 {
                            (tr!("≈ как оригинал", "≈ like the original").to_owned(), GREEN)
                        } else if diff < 0.0 {
                            (trf!("тише на {:.1} dB", "{:.1} dB quieter", -diff), AMBER)
                        } else {
                            (trf!("громче на {diff:.1} dB", "{diff:.1} dB louder"), AMBER)
                        };
                        ui.label(RichText::new(hint).size(12.0).color(color));
                        ui.label(theme::small(format!("{:.1} LUFS ·", m.loudness)));
                        ui.label(theme::small(trf!("{:.1} с ·", "{:.1} s ·", m.duration_secs())));
                        if let Some(full) = &self.mine_full
                            && m.samples.len() < full.samples.len()
                        {
                            ui.label(theme::small(trf!("{} из {:.1} с ·", "{} of {:.1} s ·", icon::SCISSORS, full.duration_secs())));
                        }
                        if self.fx.gain_db != 0.0 {
                            ui.label(theme::small(format!("{:+.1} dB ·", self.fx.gain_db)));
                        }
                        if self.fx.denoise {
                            ui.label(theme::small(tr!("шумодав ·", "denoise ·")));
                        }
                        if self.fx.pitch != 0.0 {
                            ui.label(theme::small(trf!("тон {:+.0} ·", "pitch {:+.0} ·", self.fx.pitch)));
                        }
                    }
                });
            });
            let resp = match &live {
                Some((peaks, _)) => {
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), WAVE_HEIGHT), Sense::hover());
                    theme::paint_bars(ui, rect, Some(peaks), mine_secs / timeline, RED, "");
                    resp
                }
                None => self.trim_wave(ui, mine_secs / timeline, head(Track::Mine)),
            };
            if resp.clicked() && !recording {
                self.toggle_play(Track::Mine);
            }
            self.level_meter(ui);
            if self.mine_raw.is_some() && !recording {
                ui.add_space(4.0);
                self.edit_tools(ui);
            }
        });
        ui.add_space(8.0);

        self.transport(ui, head(Track::Original).is_some(), head(Track::Mine).is_some(), recording);
        ui.add_space(10.0);

        // Настройки новой записи — общие для всех фраз, поэтому отдельной плашкой.
        ui.add_space(4.0);
        centered(ui, "new_take_settings", |ui| {
            egui::Frame::default().fill(theme::PANEL).stroke(Stroke::new(1.0_f32, LINE)).corner_radius(12).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                // Высоту строки задаём до первой подписи — иначе она прилипает к верху.
                ui.set_min_height(30.0);
                ui.label(RichText::new(tr!("Новая запись:", "New recording:")).size(13.0).color(MUTED));
                let sounds = self.project.as_ref().is_some_and(|p| p.is_sounds());
                let mut changed = false;
                for (value, text) in [
                    // У звуков способностей и предметов — своя настройка: хвост и эхо там часть звука.
                    (if sounds { &mut self.config.auto_trim_sfx } else { &mut self.config.auto_trim }, tr!("убрать тишину", "trim silence")),
                    (&mut self.config.auto_level, tr!("подогнать громкость", "match loudness")),
                    (&mut self.config.denoise_new, tr!("шумодав", "denoise")),
                ] {
                    if theme::chip(ui, *value, text).clicked() {
                        *value = !*value;
                        changed = true;
                    }
                }
                ui.add_space(6.0);
                ui.label(RichText::new(tr!("старт:", "start:")).size(13.0).color(MUTED));
                egui::ComboBox::from_id_salt("pre_roll").selected_text(RichText::new(self.config.pre_roll.label()).size(13.0)).show_ui(ui, |ui| {
                    for mode in [PreRoll::Off, PreRoll::Countdown, PreRoll::Original] {
                        changed |= ui.selectable_value(&mut self.config.pre_roll, mode, mode.label()).changed();
                    }
                });
                if changed {
                    self.config.save();
                }
            });
        });

        ui.add_space(10.0);
        ui.vertical_centered(|ui| {
            let hints = tr!("Space оригинал · Ctrl+Space моя · R запись · C сравнить · T убрать тишину · L громкость · ↑↓ фразы · N пустая · Esc отмена", "Space original · Ctrl+Space mine · R record · C compare · T trim silence · L loudness · ↑↓ lines · N next empty · Esc cancel");
            ui.label(RichText::new(hints).size(11.0).color(FAINT));
        });
    }

    /// Ряд записи: слева слушаем оригинал, справа свою версию, по центру — запись.
    /// «Из файла» — альтернатива записи, поэтому стоит в этом же ряду.
    fn transport(&mut self, ui: &mut egui::Ui, playing_orig: bool, playing_mine: bool, recording: bool) {
        let main = vec2(150.0, 34.0);
        let extra = vec2(124.0, 34.0);
        let gap = 14.0;
        let can_compare = self.mine.is_some() && self.original.is_some() && !recording;
        let can_play = self.mine.is_some() && !recording;
        centered(ui, "transport", |ui| {
            // Высоту ряда задаём заранее, иначе первая кнопка прилипает к верху.
            ui.set_min_height(72.0);
            {
                ui.spacing_mut().item_spacing.x = gap;
                let compare = egui::Button::new(trf!("{}  Сравнить", "{}  Compare", icon::ARROWS_LEFT_RIGHT)).shortcut_text("C");
                if ui.add_enabled_ui(can_compare, |ui| ui.add_sized(extra, compare)).inner.on_hover_text(tr!("Оригинал, а сразу за ним моя версия", "The original, then my version right after")).clicked() {
                    self.toggle_play(Track::Compare);
                }
                let orig = trf!("{}  Оригинал", "{}  Original", if playing_orig { icon::STOP } else { icon::PLAY });
                if ui.add_sized(main, egui::Button::new(orig).shortcut_text("Space")).clicked() {
                    self.toggle_play(Track::Original);
                }
                if self.record_button(ui, recording).clicked() {
                    self.toggle_record();
                }
                let mine = trf!("{}  Моя", "{}  Mine", if playing_mine { icon::STOP } else { icon::PLAY });
                if ui.add_enabled_ui(can_play, |ui| ui.add_sized(main, egui::Button::new(mine).shortcut_text("Ctrl+Space"))).inner.clicked() {
                    self.toggle_play(Track::Mine);
                }
                let import = egui::Button::new(trf!("{}  Из файла", "{}  From file", icon::FILE_ARROW_DOWN));
                if ui.add_enabled_ui(!recording, |ui| ui.add_sized(extra, import)).inner.on_hover_text(tr!("mp3, wav, ogg, голосовые Telegram…", "mp3, wav, ogg, Telegram voice messages…")).clicked() {
                    self.import_file();
                }
            }
        });
    }

    fn record_button(&self, ui: &mut egui::Ui, recording: bool) -> egui::Response {
        let (rect, resp) = ui.allocate_exact_size(vec2(72.0, 72.0), Sense::click());
        let enabled = self.recorder.is_some();
        let c = rect.center();
        let p = ui.painter();
        if recording || self.pending.is_some() {
            // Пульсирующее кольцо, пока идёт запись или отсчёт.
            let t = ui.input(|i| i.time) as f32;
            let pulse = (t * 3.0).sin() * 0.5 + 0.5;
            p.circle_filled(c, 33.0 + pulse * 3.0, RED.gamma_multiply(0.18 + pulse * 0.12));
        } else {
            p.circle_filled(c, 34.0, RED.gamma_multiply(0.14));
        }
        let fill = if !enabled {
            CARD_HOVER
        } else if resp.hovered() {
            Color32::from_rgb(238, 92, 80)
        } else {
            RED
        };
        p.circle_filled(c, 27.0, fill);
        match self.pending {
            Some(Pending::Countdown { started, .. }) => {
                let left = if started.is_nan() { 3.0 } else { 3.0 - (ui.input(|i| i.time) - started) };
                p.text(c, Align2::CENTER_CENTER, format!("{}", left.ceil().max(1.0)), FontId::new(28.0, theme::bold()), Color32::WHITE);
            }
            Some(Pending::Original { .. }) => {
                p.text(c, Align2::CENTER_CENTER, icon::EAR, FontId::proportional(26.0), Color32::WHITE);
            }
            None => {
                let glyph = if recording { icon::STOP } else { icon::MICROPHONE };
                p.text(c, Align2::CENTER_CENTER, glyph, FontId::proportional(26.0), Color32::WHITE);
            }
        }
        resp.on_hover_text(if recording || self.pending.is_some() { tr!("Остановить (R)", "Stop (R)") } else { tr!("Записать (R)", "Record (R)") })
            .on_hover_cursor(egui::CursorIcon::PointingHand)
    }

    /// Волна моей записи целиком: затемнённые края — обрезано, золотые ручки можно тянуть.
    fn trim_wave(&mut self, ui: &mut egui::Ui, span: f32, head: Option<f32>) -> egui::Response {
        let Some(raw) = self.mine_full.clone() else {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), WAVE_HEIGHT), Sense::hover());
            theme::paint_bars(ui, rect, None, span, GOLD, tr!("Нажми R или красную кнопку, чтобы записать", "Press R or the red button to record"));
            return resp;
        };
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), WAVE_HEIGHT), Sense::click_and_drag());
        let inner = theme::paint_bars(ui, rect, Some(&raw.peaks), span, GOLD, "");
        let len = raw.samples.len().max(1);
        let width = (inner.width() * span.clamp(0.0, 1.0)).max(1.0);
        let to_x = |i: usize| inner.left() + width * i as f32 / len as f32;
        let to_index = |x: f32| (((x - inner.left()) / width).clamp(0.0, 1.0) * len as f32) as usize;
        let (s, e) = self.drag.map_or(self.trim, |d| d.range);

        let p = ui.painter_at(rect);
        let shade = BG_DEEP.gamma_multiply(0.82);
        p.rect_filled(Rect::from_min_max(pos2(inner.left() - 1.0, inner.top()), pos2(to_x(s), inner.bottom())), 0.0, shade);
        p.rect_filled(Rect::from_min_max(pos2(to_x(e), inner.top()), pos2(inner.left() + width + 1.0, inner.bottom())), 0.0, shade);
        for x in [to_x(s), to_x(e)] {
            p.line_segment([pos2(x, rect.top() + 3.0), pos2(x, rect.bottom() - 3.0)], Stroke::new(2.0_f32, GOLD));
            for y in [rect.top() + 10.0, rect.bottom() - 10.0] {
                p.rect_filled(Rect::from_center_size(pos2(x, y), vec2(8.0, 14.0)), 3.0, GOLD);
                p.line_segment([pos2(x, y - 3.0), pos2(x, y + 3.0)], Stroke::new(1.0_f32, ON_GOLD));
            }
        }
        if let Some(f) = head {
            playhead(ui, rect, to_x(s + ((e - s) as f32 * f.clamp(0.0, 1.0)) as usize));
        }

        let near = |pos: Pos2| {
            let (ds, de) = ((pos.x - to_x(s)).abs(), (pos.x - to_x(e)).abs());
            if ds.min(de) > 8.0 {
                None
            } else if ds <= de {
                Some(Handle::Start)
            } else {
                Some(Handle::End)
            }
        };
        if resp.hover_pos().is_some_and(|pos| near(pos).is_some()) || self.drag.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        } else if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        // Ручку ищем по точке нажатия: к моменту, когда egui признаёт перетаскивание,
        // курсор уже успевает отъехать от ручки.
        if resp.drag_started()
            && let Some(handle) = ui.input(|i| i.pointer.press_origin()).and_then(near)
        {
            self.drag = Some(TrimDrag { handle, range: (s, e) });
        }
        if let Some(d) = &mut self.drag
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let i = to_index(pos.x);
            match d.handle {
                Handle::Start => d.range.0 = i.min(d.range.1.saturating_sub(MIN_TRIM_SAMPLES)),
                Handle::End => d.range.1 = i.max(d.range.0 + MIN_TRIM_SAMPLES).min(len),
            }
        }
        if resp.drag_stopped()
            && let Some(d) = self.drag.take()
        {
            match self.apply_trim(d.range) {
                Ok(()) => self.info(trf!("Обрезка: {:.2}–{:.2} с", "Trim: {:.2}–{:.2} s", index_to_secs(d.range.0), index_to_secs(d.range.1))),
                Err(e) => self.error(trf!("Обрезка не сохранилась: {e:#}", "Trim wasn't saved: {e:#}")),
            }
        }
        resp
    }

    /// Инструменты для моей версии: подписанные строки «Обрезка», «Громкость» и «Звук».
    fn edit_tools(&mut self, ui: &mut egui::Ui) {
        // Сетка: подписи слева, в каждой колонке кнопки одной ширины, подсказки — в последней.
        const MAIN: egui::Vec2 = vec2(236.0, 30.0);
        const SECOND: egui::Vec2 = vec2(136.0, 30.0);
        let trimmed = self.trim != (0, self.full_len());
        let label = |ui: &mut egui::Ui, icon: &str, text: &str| {
            ui.label(RichText::new(format!("{icon}  {text}")).size(13.0).color(MUTED));
        };

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        // Пока подгоняется громкость всех фраз, текущую не трогаем — её файл тоже перепишется.
        if self.match_all_running() {
            ui.disable();
        }
        egui::Grid::new("edit_tools").num_columns(4).spacing([10.0, 8.0]).show(ui, |ui| {
            label(ui, icon::SCISSORS, tr!("Обрезка", "Trim"));
            if ui.add_sized(MAIN, theme::tool(tr!("Убрать тишину по краям", "Trim silence at the edges")).shortcut_text("T")).clicked() {
                self.trim_silence();
            }
            if ui.add_enabled_ui(trimmed, |ui| ui.add_sized(SECOND, theme::tool(tr!("Без обрезки", "No trim")))).inner.clicked() {
                self.reset_trim();
            }
            let hint = if trimmed {
                trf!("в игру идёт {:.2}–{:.2} с · края можно двигать", "{:.2}–{:.2} s goes into the game · drag the edges", index_to_secs(self.trim.0), index_to_secs(self.trim.1))
            } else {
                tr!("или тяни золотые края на волне", "or drag the gold edges on the wave").to_owned()
            };
            ui.label(RichText::new(hint).size(12.0).color(FAINT));
            ui.end_row();

            label(ui, icon::SPEAKER_HIGH, tr!("Громкость", "Loudness"));
            self.gain_slider(ui, MAIN);
            if ui.add_sized(SECOND, theme::tool(tr!("Как в оригинале", "Match original")).shortcut_text("L")).clicked() {
                self.match_loudness();
            }
            ui.horizontal(|ui| self.match_all_controls(ui));
            ui.end_row();

            label(ui, icon::WAVEFORM, tr!("Звук", "Sound"));
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr!("Тон", "Pitch")).size(13.0).color(MUTED));
                let pitch = self.fx.pitch;
                let step = |t: &str| egui::Button::new(RichText::new(t).size(13.0)).min_size(vec2(30.0, 30.0));
                if ui.add_enabled(pitch > -12.0, step(icon::MINUS)).on_hover_text(tr!("На полутон ниже — грубее и массивнее", "A semitone lower — rougher and heavier")).clicked() {
                    self.set_pitch(pitch - 1.0);
                }
                let value = if pitch == 0.0 { "0".to_owned() } else { format!("{pitch:+.0}") };
                ui.add_sized(vec2(26.0, 30.0), egui::Label::new(RichText::new(value).size(13.0).color(if pitch == 0.0 { MUTED } else { GOLD })));
                if ui.add_enabled(pitch < 12.0, step(icon::PLUS)).on_hover_text(tr!("На полутон выше — мельче и звонче", "A semitone higher — lighter and brighter")).clicked() {
                    self.set_pitch(pitch + 1.0);
                }
                if pitch != 0.0 && theme::ghost(ui, true, tr!("сбросить", "reset")).clicked() {
                    self.set_pitch(0.0);
                }
            });
            let denoise = self.fx.denoise;
            let mut btn = theme::tool(trf!("{}  Шумодав", "{}  Denoise", if denoise { icon::CHECK_SQUARE } else { icon::SQUARE }));
            if denoise {
                btn = btn.fill(GOLD.gamma_multiply(0.12)).stroke(Stroke::new(1.0_f32, GOLD.gamma_multiply(0.6)));
            }
            if ui.add_sized(SECOND, btn).on_hover_text(tr!("Убрать шум комнаты и фон (RNNoise)", "Remove room noise and background (RNNoise)")).clicked() {
                self.toggle_denoise();
            }
            ui.end_row();
        });
    }

    /// Ползунок усиления моей версии: пока тянут — слышно и видно сразу, файл пишется,
    /// когда отпустили. Двойной клик — как в оригинале.
    fn gain_slider(&mut self, ui: &mut egui::Ui, size: egui::Vec2) {
        const VALUE_WIDTH: f32 = 62.0;
        ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(Align::Center), |ui| {
            ui.spacing_mut().slider_width = size.x - VALUE_WIDTH - ui.spacing().item_spacing.x;
            // Дорожка темнее карточки, заполненная часть — золотом, как остальные включённые элементы.
            let v = ui.visuals_mut();
            v.widgets.inactive.bg_fill = BG_DEEP;
            v.widgets.inactive.fg_stroke = Stroke::new(1.5_f32, TEXT);
            v.selection.bg_fill = GOLD.gamma_multiply(0.75);
            let mut gain = self.fx.gain_db;
            let resp = ui
                .add(egui::Slider::new(&mut gain, process::GAIN_RANGE).step_by(0.5).show_value(false).trailing_fill(true))
                .on_hover_text(tr!("Тише ← → громче. Двойной клик — как в оригинале", "Quieter ← → louder. Double-click — match the original"));
            let value = if gain == 0.0 { "0 dB".to_owned() } else { format!("{gain:+.1} dB") };
            ui.add_sized(vec2(VALUE_WIDTH, size.y), egui::Label::new(RichText::new(value).size(13.0).color(if gain == 0.0 { MUTED } else { GOLD })));
            if resp.double_clicked() {
                self.match_loudness();
                return;
            }
            if resp.changed() {
                self.preview_gain(gain);
            }
            // Клик по дорожке или стрелки — сохраняем сразу, перетаскивание — когда отпустили.
            if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                self.commit_gain();
            }
        });
    }

    /// Выпадающий список прежних дублей: послушать, вернуть, удалить.
    fn takes_menu(&mut self, ui: &mut egui::Ui) {
        let title = RichText::new(trf!("{}  дубли: {}", "{}  takes: {}", icon::STACK, self.takes.len())).size(12.0).color(MUTED);
        let mut action = None;
        ui.menu_button(title, |ui| {
            ui.set_min_width(280.0);
            ui.label(theme::small(tr!("Прежние попытки этой фразы", "Earlier attempts at this line")));
            for take in &self.takes {
                ui.horizontal(|ui| {
                    let when = take.saved.map(ago).unwrap_or_default();
                    let mut text = trf!("№{} · {:.1} с · {when}", "#{} · {:.1} s · {when}", take.id, take.secs);
                    if let Some(o) = &take.origin {
                        text += &format!(" · {}  {}", icon::USER, super::share::short_author(&o.author));
                    }
                    ui.label(RichText::new(text).size(12.0).color(TEXT));
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(egui::Button::new(icon::TRASH).frame(false)).on_hover_text(tr!("В корзину", "To trash")).clicked() {
                            action = Some((take.clone(), TakeAction::Trash));
                        }
                        if ui.add(egui::Button::new(RichText::new(tr!("Вернуть", "Restore")).size(12.0))).on_hover_text(tr!("Сделать этот дубль текущим", "Make this take current")).clicked() {
                            action = Some((take.clone(), TakeAction::Restore));
                        }
                        if ui.add(egui::Button::new(icon::PLAY).frame(false)).on_hover_text(tr!("Послушать как есть", "Play as is")).clicked() {
                            action = Some((take.clone(), TakeAction::Play));
                        }
                    });
                });
            }
        });
        match action {
            Some((take, TakeAction::Play)) => self.play_take(&take),
            Some((take, TakeAction::Restore)) => {
                ui.close_menu();
                self.restore_take(take.id);
            }
            Some((take, TakeAction::Trash)) => self.trash_take(take.id),
            None => {}
        }
    }

    fn level_meter(&mut self, ui: &mut egui::Ui) {
        if let Some(r) = &self.recorder {
            let (peak, clipped) = r.take_level();
            self.level = peak.max(self.level * 0.85);
            self.clipped |= clipped;
        }
        let db = audio::to_db(self.level).max(-60.0);
        let frac = (db + 60.0) / 60.0;
        let color = if self.clipped || db > -1.0 {
            RED
        } else if db > -12.0 {
            AMBER
        } else {
            GREEN
        };
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(icon::MICROPHONE).size(12.0).color(FAINT));
            let width = (ui.available_width() - 52.0).max(40.0);
            let (rect, _) = ui.allocate_exact_size(vec2(width, 4.0), Sense::hover());
            let p = ui.painter_at(rect);
            p.rect_filled(rect, 2.0, BG_DEEP);
            let mut filled = rect;
            filled.set_width(rect.width() * frac.clamp(0.0, 1.0));
            p.rect_filled(filled, 2.0, color);
            let text = if db <= -60.0 { tr!("тихо", "quiet").to_owned() } else { format!("{db:.0} dB") };
            ui.label(RichText::new(text).size(11.0).color(FAINT));
        });
        if self.clipped {
            ui.label(RichText::new(tr!("Перегруз микрофона — отодвинься или убавь усиление и перезапиши", "Microphone clipping — move back or lower the gain and record again")).size(12.0).color(RED));
        }
    }
}

enum TakeAction {
    Play,
    Restore,
    Trash,
}

/// Ряд виджетов строго по центру: ширину ряда замеряем на прошлом кадре
/// и храним в памяти egui под ключом `id` (первый кадр может быть чуть смещён).
fn centered(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    let key = egui::Id::new(("centered", id));
    let width: f32 = ui.ctx().data(|d| d.get_temp(key)).unwrap_or(0.0);
    ui.horizontal(|ui| {
        ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
        let row = ui.horizontal(add).response.rect.width();
        if (row - width).abs() > 0.5 {
            ui.ctx().data_mut(|d| d.insert_temp(key, row));
            ui.ctx().request_repaint();
        }
    });
}

fn playhead(ui: &egui::Ui, rect: Rect, x: f32) {
    ui.painter_at(rect).line_segment([pos2(x, rect.top() + 2.0), pos2(x, rect.bottom() - 2.0)], Stroke::new(1.5_f32, TEXT));
}

fn status_color(status: Status) -> Color32 {
    match status {
        Status::Empty => Color32::from_rgb(70, 79, 95),
        Status::Recorded => GREEN,
        Status::Redo => AMBER,
    }
}

/// Заголовок группы: стрелка, название, счётчик справа. Клик сворачивает группу.
/// Заголовок раздела (аркана, набор, вид личности): над его группами, крупнее и с линией сверху.
fn section_header(ui: &mut egui::Ui, label: &str, done: usize, total: usize, open: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
    let p = ui.painter();
    p.hline(rect.x_range(), rect.top(), Stroke::new(1.0_f32, LINE));
    let caret = if open { icon::CARET_DOWN } else { icon::CARET_RIGHT };
    let color = if resp.hovered() { GOLD } else { TEXT };
    p.text(pos2(rect.left() + 4.0, rect.center().y + 2.0), Align2::LEFT_CENTER, caret, FontId::proportional(11.0), FAINT);
    p.text(pos2(rect.left() + 18.0, rect.center().y + 2.0), Align2::LEFT_CENTER, label, FontId::new(12.5, theme::bold()), color);
    p.text(pos2(rect.right() - 4.0, rect.center().y + 2.0), Align2::RIGHT_CENTER, format!("{done}/{total}"), FontId::proportional(11.0), MUTED);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn group_header(ui: &mut egui::Ui, label: &str, icon_tex: Option<&egui::TextureHandle>, done: usize, total: usize, open: bool) -> egui::Response {
    let height = if icon_tex.is_some() { 26.0 } else { 22.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    let color = if resp.hovered() { TEXT } else { MUTED };
    let caret = if open { icon::CARET_DOWN } else { icon::CARET_RIGHT };
    ui.painter().text(pos2(rect.left() + 4.0, rect.center().y), Align2::LEFT_CENTER, caret, FontId::proportional(11.0), FAINT);
    // Иконка способности (квадратная) или предмета (88×64) — высотой в строку.
    let mut x = rect.left() + 18.0;
    if let Some(tex) = icon_tex {
        let size = tex.size_vec2();
        let h = 20.0;
        let w = (h * size.x / size.y.max(1.0)).min(30.0);
        let r = Rect::from_min_size(pos2(x, rect.center().y - h / 2.0), vec2(w, h));
        egui::Image::from_texture(egui::load::SizedTexture::from_handle(tex)).corner_radius(4).paint_at(ui, r);
        x += w + 7.0;
    }
    let p = ui.painter();
    p.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, label, FontId::proportional(12.0), color);
    let count_color = if done == total { GREEN } else { FAINT };
    p.text(pos2(rect.right() - 6.0, rect.center().y), Align2::RIGHT_CENTER, format!("{done}/{total}"), FontId::proportional(11.0), count_color);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Строка списка: точка статуса, имя фразы и (бледно) заметка.
fn list_row(ui: &mut egui::Ui, name: &str, note: &str, status: Status, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
    let p = ui.painter_at(rect);
    if selected {
        p.rect_filled(rect, 7.0, GOLD);
    } else if resp.hovered() {
        p.rect_filled(rect, 7.0, CARD_HOVER);
    }
    p.circle_filled(pos2(rect.left() + 13.0, rect.center().y), 3.5, if selected { ON_GOLD } else { status_color(status) });
    let name_color = if selected { ON_GOLD } else { TEXT };
    let galley = p.layout_no_wrap(name.to_owned(), FontId::monospace(12.5), name_color);
    let name_w = galley.size().x;
    p.galley(pos2(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0), galley, name_color);
    if !note.is_empty() {
        let note_color = if selected { ON_GOLD.gamma_multiply(0.7) } else { FAINT };
        p.text(pos2(rect.left() + 34.0 + name_w, rect.center().y), Align2::LEFT_CENTER, note, FontId::proportional(11.5), note_color);
    }
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if note.is_empty() { resp } else { resp.on_hover_text(note) }
}
