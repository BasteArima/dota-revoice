//! Экран «Запись»: список фраз слева, оригинал и моя версия справа.

use dota_revoice::audio::{self, Track};
use dota_revoice::project::{self, Status};
use eframe::egui::{self, Align, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, pos2, vec2};

use super::theme::{self, AMBER, BG_DEEP, CARD_HOVER, FAINT, GOLD, GREEN, LINE, MUTED, ON_GOLD, RED, TEXT, WAVE, icon};
use super::{App, Handle, MIN_TRIM_SAMPLES, TrimDrag, index_to_secs, prefix_of};

const WAVE_HEIGHT: f32 = 84.0;

impl App {
    pub(super) fn line_list(&mut self, ui: &mut egui::Ui) {
        let Some(p) = &self.project else {
            ui.add_space(8.0);
            ui.label(theme::small("Открой папку с фразами героя"));
            return;
        };
        egui::TopBottomPanel::bottom("legend").frame(egui::Frame::default()).show_inside(ui, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (status, text) in [(Status::Recorded, "записано"), (Status::Redo, "переделать"), (Status::Empty, "пусто")] {
                    let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 3.5, status_color(status));
                    ui.label(theme::small(text));
                    ui.add_space(2.0);
                }
            });
        });

        egui::Frame::default().fill(BG_DEEP).stroke(Stroke::new(1.0_f32, LINE)).corner_radius(8).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::MAGNIFYING_GLASS).color(MUTED));
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .frame(false)
                        .hint_text(RichText::new("Имя, категория или заметка").color(FAINT))
                        .desired_width(f32::INFINITY),
                );
            });
        });
        ui.add_space(8.0);
        let (done, total) = (p.recorded_count(), p.lines.len());
        ui.horizontal(|ui| {
            ui.label(theme::small("Записано"));
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
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            for (cat, idx) in &self.groups {
                let shown: Vec<usize> = idx.iter().copied().filter(|&i| self.matches(i, &q)).collect();
                if shown.is_empty() {
                    continue;
                }
                let rec = shown.iter().filter(|&&i| p.status(&p.lines[i]) != Status::Empty).count();
                // При поиске группы всегда раскрыты; выбранную фразу не прячем.
                let open = !q.is_empty() || !self.collapsed.contains(cat) || (scroll && shown.contains(&self.selected));
                ui.add_space(6.0);
                if group_header(ui, &project::category_label(cat), rec, shown.len(), open).clicked() {
                    toggle_group = Some(cat.clone());
                }
                if !open {
                    continue;
                }
                for i in shown {
                    let line = &p.lines[i];
                    let short = line.name.strip_prefix(&format!("{}_", prefix_of(&line.name))).unwrap_or(&line.name);
                    let resp = list_row(ui, short, p.note(line), p.status(line), i == self.selected);
                    if resp.clicked() {
                        clicked = Some(i);
                    }
                    if scroll && i == self.selected {
                        resp.scroll_to_me(Some(Align::Center));
                    }
                }
            }
        });
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
                ui.label(theme::title("Открой папку с фразами героя", 22.0));
                ui.label(theme::small("Выгрузка из Source 2 Viewer, например …/sounds/vo/bounty_hunter"));
                ui.add_space(14.0);
                if ui.add(theme::primary(format!("{}  Открыть папку", icon::FOLDER_OPEN))).clicked() {
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
                Status::Empty => theme::pill(ui, "пусто", MUTED),
                Status::Recorded => theme::pill(ui, "записано", GREEN),
                Status::Redo => theme::pill(ui, "переделать", AMBER),
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if ui.add(theme::ghost(format!("Следующая пустая  {}", icon::SKIP_FORWARD))).on_hover_text("N").clicked() {
                    self.next_unrecorded();
                }
                if ui.add(egui::Button::new(icon::CARET_RIGHT).min_size(vec2(30.0, 28.0))).on_hover_text("Следующая (↓)").clicked() {
                    self.step(1);
                }
                if ui.add(egui::Button::new(icon::CARET_LEFT).min_size(vec2(30.0, 28.0))).on_hover_text("Предыдущая (↑)").clicked() {
                    self.step(-1);
                }
            });
        });
        let orig_secs = self.original.as_ref().map_or(0.0, |c| c.duration_secs());
        ui.horizontal(|ui| {
            ui.label(theme::small(format!("{} · {orig_secs:.1} с ·", project::category_label(&line.category))));
            ui.label(RichText::new(icon::PENCIL_SIMPLE).size(12.0).color(FAINT));
            // Заметка: тихая строка, которую можно поправить в любой момент.
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.note)
                    .frame(false)
                    .font(FontId::proportional(12.0))
                    .text_color(MUTED)
                    .hint_text(RichText::new("заметка для себя: что это за фраза").size(12.0).color(FAINT))
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
            None => self.mine_raw.as_ref().map_or(0.0, |c| c.duration_secs()),
        };
        let timeline = orig_secs.max(mine_secs).max(0.1);

        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::small("Оригинал"));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if let Some(c) = &self.original {
                        ui.label(theme::small(format!("{:.1} dB", c.rms_db())));
                    }
                });
            });
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), WAVE_HEIGHT), Sense::click());
            let inner = theme::paint_bars(ui, rect, self.original.as_ref().map(|c| c.peaks.as_slice()), orig_secs / timeline, WAVE, "Оригинал не загружен");
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
                ui.label(RichText::new("Моя версия").size(12.0).color(TEXT));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if recording {
                        ui.label(RichText::new(format!("●  запись {mine_secs:.1} с")).size(12.0).color(RED));
                    } else if let (Some(m), Some(o)) = (&self.mine, &self.original) {
                        let diff = m.rms_db() - o.rms_db();
                        let (hint, color) = if diff.abs() < 2.0 {
                            ("≈ как оригинал".to_owned(), GREEN)
                        } else if diff < 0.0 {
                            (format!("тише на {:.1} dB", -diff), AMBER)
                        } else {
                            (format!("громче на {diff:.1} dB"), AMBER)
                        };
                        ui.label(RichText::new(hint).size(12.0).color(color));
                        ui.label(theme::small(format!("{:.1} с ·", m.duration_secs())));
                        if let Some(raw) = &self.mine_raw
                            && m.samples.len() < raw.samples.len()
                        {
                            ui.label(theme::small(format!("{} из {:.1} с ·", icon::SCISSORS, raw.duration_secs())));
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
        });
        ui.add_space(16.0);

        self.transport(ui, head(Track::Original).is_some(), head(Track::Mine).is_some(), recording);
        ui.add_space(10.0);

        // Второстепенные действия — тихие кнопки по центру.
        let can_edit = self.mine_raw.is_some() && !recording;
        let trimmed = self.mine_raw.as_ref().is_some_and(|r| self.trim != (0, r.samples.len()));
        centered(ui, 640.0, |ui| {
            if ui.add_enabled(self.mine.is_some() && self.original.is_some(), theme::ghost(format!("{}  Сравнить", icon::ARROWS_LEFT_RIGHT))).on_hover_text("Оригинал, потом моя (C)").clicked() {
                self.toggle_play(Track::Compare);
            }
            if ui.add_enabled(can_edit, theme::ghost(format!("{}  Тишина", icon::SCISSORS))).on_hover_text("Обрезать тишину по краям (T)").clicked() {
                self.trim_silence();
            }
            if ui.add_enabled(can_edit && trimmed, theme::ghost("Без обрезки")).clicked() {
                self.reset_trim();
            }
            if ui.add_enabled(!recording, theme::ghost(format!("{}  Из файла", icon::FILE_ARROW_DOWN))).clicked() {
                self.import_file();
            }
            let redo = status == Status::Redo;
            let redo_btn = egui::Button::new(RichText::new(format!("{}  Переделать", icon::FLAG)).color(if redo { AMBER } else { MUTED })).frame(false);
            if ui.add_enabled(status != Status::Empty && !recording, redo_btn).clicked() {
                self.set_redo(!redo);
            }
            if ui.add_enabled(status != Status::Empty && !recording, theme::ghost(format!("{}  В корзину", icon::TRASH))).on_hover_text("Del").clicked() {
                self.trash_current();
            }
        });
        ui.add_space(4.0);
        centered(ui, 640.0, |ui| {
            let auto = self.config.auto_trim;
            let text = format!("{}  Автообрезка тишины после записи", if auto { icon::CHECK_SQUARE } else { icon::SQUARE });
            if ui.add(egui::Button::new(RichText::new(text).size(12.0).color(if auto { GOLD } else { FAINT })).frame(false)).clicked() {
                self.config.auto_trim = !auto;
                self.config.save();
            }
            if self.mine_raw.is_some() {
                ui.label(RichText::new("· края волны можно тянуть мышкой").size(12.0).color(FAINT));
            }
        });

        ui.with_layout(egui::Layout::bottom_up(Align::Center), |ui| {
            ui.label(RichText::new("Space — оригинал · R — запись · M — моя · C — сравнить · T — тишина · ↑↓ — фразы · N — пустая · Esc — отмена").size(11.0).color(FAINT));
        });
    }

    /// Большая красная кнопка записи и кнопки прослушивания по бокам.
    fn transport(&mut self, ui: &mut egui::Ui, playing_orig: bool, playing_mine: bool, recording: bool) {
        let side = vec2(150.0, 34.0);
        let total = side.x * 2.0 + 72.0 + 40.0;
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
            // Высоту ряда задаём заранее, иначе первая кнопка прилипает к верху.
            ui.allocate_ui_with_layout(vec2(total, 76.0), egui::Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 20.0;
                let orig = format!("{}  Оригинал", if playing_orig { icon::STOP } else { icon::PLAY });
                if ui.add_sized(side, egui::Button::new(orig).shortcut_text("Space")).clicked() {
                    self.toggle_play(Track::Original);
                }
                if self.record_button(ui, recording).clicked() {
                    self.toggle_record();
                }
                let mine = format!("{}  Моя", if playing_mine { icon::STOP } else { icon::PLAY });
                let can_play = self.mine.is_some() && !recording;
                if ui.add_enabled_ui(can_play, |ui| ui.add_sized(side, egui::Button::new(mine).shortcut_text("M"))).inner.clicked() {
                    self.toggle_play(Track::Mine);
                }
            });
        });
    }

    fn record_button(&self, ui: &mut egui::Ui, recording: bool) -> egui::Response {
        let (rect, resp) = ui.allocate_exact_size(vec2(72.0, 72.0), Sense::click());
        let enabled = self.recorder.is_some();
        let c = rect.center();
        let p = ui.painter();
        if recording {
            // Пульсирующее кольцо, пока идёт запись.
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
        let glyph = if recording { icon::STOP } else { icon::MICROPHONE };
        p.text(c, Align2::CENTER_CENTER, glyph, FontId::proportional(26.0), Color32::WHITE);
        resp.on_hover_text(if recording { "Остановить запись (R)" } else { "Записать (R)" })
            .on_hover_cursor(egui::CursorIcon::PointingHand)
    }

    /// Волна моей записи целиком: затемнённые края — обрезано, золотые ручки можно тянуть.
    fn trim_wave(&mut self, ui: &mut egui::Ui, span: f32, head: Option<f32>) -> egui::Response {
        let Some(raw) = self.mine_raw.clone() else {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), WAVE_HEIGHT), Sense::hover());
            theme::paint_bars(ui, rect, None, span, GOLD, "Нажми R или красную кнопку, чтобы записать");
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
                Ok(()) => self.info(format!("Обрезка: {:.2}–{:.2} с", index_to_secs(d.range.0), index_to_secs(d.range.1))),
                Err(e) => self.error(format!("Обрезка не сохранилась: {e:#}")),
            }
        }
        resp
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
            let text = if db <= -60.0 { "тихо".to_owned() } else { format!("{db:.0} dB") };
            ui.label(RichText::new(text).size(11.0).color(FAINT));
        });
        if self.clipped {
            ui.label(RichText::new("Перегруз микрофона — отодвинься или убавь усиление и перезапиши").size(12.0).color(RED));
        }
    }
}

/// Ряд виджетов по центру доступной ширины (примерная ширина ряда — `width`).
fn centered(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
        add(ui);
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
fn group_header(ui: &mut egui::Ui, label: &str, done: usize, total: usize, open: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
    let p = ui.painter();
    let color = if resp.hovered() { TEXT } else { MUTED };
    let caret = if open { icon::CARET_DOWN } else { icon::CARET_RIGHT };
    p.text(pos2(rect.left() + 4.0, rect.center().y), Align2::LEFT_CENTER, caret, FontId::proportional(11.0), FAINT);
    p.text(pos2(rect.left() + 18.0, rect.center().y), Align2::LEFT_CENTER, label, FontId::proportional(12.0), color);
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
