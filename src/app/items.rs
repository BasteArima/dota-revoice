//! Вкладка «Предметы»: сетка предметов, у которых в игре есть свои звуки. Клик —
//! звуки предметов достаются из основного пака, и «Запись» открывается на этом предмете.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use dota_revoice::sfx::{self, SoundGroup};
use dota_revoice::{tr, trf};
use eframe::egui::{self, Align, Align2, Color32, FontId, Layout, RichText, Sense, Stroke, vec2};

use super::heroes::{Target, initials, notice, placeholder_color};
use super::settings::choice_chip;
use super::theme::{self, BG_DEEP, CARD, CARD_HOVER, FAINT, GOLD, GREEN, LINE, MUTED, TEXT, icon};
use super::App;

const CARD_MAX: f32 = 132.0;
const GAP: f32 = 10.0;
const REFRESH: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Filter {
    #[default]
    All,
    Started,
    Untouched,
}

#[derive(Default)]
pub(super) struct ItemsState {
    search: String,
    filter: Filter,
    /// Какие звуки предметов уже записаны (имена файлов) — пересчитывается раз в несколько секунд.
    recorded: HashSet<String>,
    recorded_at: Option<Instant>,
}

impl ItemsState {
    pub(super) fn refresh(&mut self) {
        self.recorded_at = None;
    }
}

impl App {
    pub(super) fn items_screen(&mut self, ui: &mut egui::Ui) {
        if self.items.recorded_at.is_none_or(|t| t.elapsed() > REFRESH) {
            let dir = self.config.export_root().join(sfx::ITEM_SOUNDS);
            self.items.recorded = std::fs::read_dir(dir)
                .map(|it| {
                    it.filter_map(|e| e.ok()?.path().file_stem()?.to_str().map(str::to_owned))
                        .filter(|n| !n.starts_with('.'))
                        .collect()
                })
                .unwrap_or_default();
            self.items.recorded_at = Some(Instant::now());
        }
        ui.label(theme::title(tr!("Предметы", "Items"), 22.0));
        ui.label(theme::small(tr!(
            "Выбери предмет — его звуки программа достанет из игры сама",
            "Pick an item — the app extracts its sounds from the game"
        )));
        ui.add_space(10.0);
        if self.dota.is_none() {
            notice(ui, tr!("Дота не найдена — укажи её папку в настройках.", "Dota not found — set its folder in the settings."), MUTED);
            return;
        }
        let Some(set) = self.heroes.items.clone() else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(theme::small(tr!("Читаю предметы из файлов игры…", "Reading items from the game files…")));
            });
            return;
        };
        let recorded = self.items.recorded.clone();
        let done = |key: &str| set.files.iter().filter(|f| f.group == key && recorded.contains(&f.name)).count();
        let started = set.groups.iter().filter(|g| done(&g.key) > 0).count();
        let total_done: usize = set.groups.iter().map(|g| done(&g.key)).sum();

        ui.horizontal(|ui| {
            egui::Frame::default().fill(BG_DEEP).stroke(Stroke::new(1.0_f32, LINE)).corner_radius(8).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::MAGNIFYING_GLASS).color(MUTED));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.items.search)
                            .frame(false)
                            .hint_text(RichText::new(tr!("Название предмета", "Item name")).color(FAINT))
                            .desired_width(200.0),
                    );
                });
            });
            ui.add_space(6.0);
            for (filter, label) in [
                (Filter::All, trf!("Все {}", "All {}", set.groups.len())),
                (Filter::Started, trf!("Начатые {started}", "Started {started}")),
                (Filter::Untouched, tr!("Без записей", "Untouched").to_owned()),
            ] {
                if choice_chip(ui, self.items.filter == filter, true, &label).clicked() {
                    self.items.filter = filter;
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(theme::small(trf!("Заменено звуков: {total_done}", "Sounds replaced: {total_done}")));
            });
        });
        ui.add_space(10.0);

        let q = self.items.search.trim().to_lowercase();
        let shown: Vec<&SoundGroup> = set
            .groups
            .iter()
            .filter(|g| q.is_empty() || g.label.to_lowercase().contains(&q) || g.key.contains(&q))
            .filter(|g| match self.items.filter {
                Filter::All => true,
                Filter::Started => done(&g.key) > 0,
                Filter::Untouched => done(&g.key) == 0,
            })
            .collect();
        if shown.is_empty() {
            notice(ui, tr!("Ничего не нашлось — поменяй поиск или фильтр.", "Nothing found — change the search or the filter."), MUTED);
            return;
        }
        let width = ui.available_width();
        let cols = ((width + GAP) / (CARD_MAX + GAP)).ceil().max(1.0) as usize;
        let card_w = ((width - GAP * (cols - 1) as f32) / cols as f32).floor();
        let current = self.project.as_ref().filter(|p| p.resource_dir == sfx::ITEM_SOUNDS).and_then(|p| p.lines.get(self.selected)).map(|l| l.category.clone());
        let mut clicked = None;
        let mut share = false;
        egui::ScrollArea::vertical().id_salt("items").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(GAP, GAP);
            for row in shown.chunks(cols) {
                ui.horizontal(|ui| {
                    for g in row {
                        let resp = self.item_card(ui, g, done(&g.key), set.count(&g.key), card_w, current.as_deref() == Some(g.key.as_str()));
                        if resp.clicked() {
                            clicked = Some(g.key.clone());
                        }
                        resp.context_menu(|ui| {
                            let text = trf!("{}  Поделиться звуками предметов…", "{}  Share item sounds…", icon::EXPORT);
                            if ui.add_enabled(total_done > 0, egui::Button::new(text)).on_disabled_hover_text(tr!("Пока нет заменённых звуков", "No replaced sounds yet")).clicked() {
                                share = true;
                                ui.close_menu();
                            }
                        });
                    }
                });
            }
        });
        if let Some(key) = clicked {
            let ctx = ui.ctx().clone();
            self.open_sounds(&ctx, Target::Items(Some(key)));
        }
        if share {
            self.open_export(&[sfx::ITEM_SOUNDS.to_owned()]);
        }
    }

    /// Карточка предмета: иконка 88×64, название, «записано / всего» и полоска.
    fn item_card(&mut self, ui: &mut egui::Ui, g: &SoundGroup, done: usize, total: usize, w: f32, current: bool) -> egui::Response {
        let img_h = (w * 64.0 / 88.0).round();
        let (rect, resp) = ui.allocate_exact_size(vec2(w, img_h + 46.0), Sense::click());
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let opening = self.heroes.opening_key().is_some_and(|k| k.starts_with("items:"));
        let hovered = resp.hovered() && !opening;
        let texture = g.icon.as_deref().and_then(|id| self.icon_texture(ui.ctx(), id));
        let p = ui.painter();
        p.rect_filled(rect, 9.0, if hovered { CARD_HOVER } else { CARD });
        let img = egui::Rect::from_min_size(rect.min, vec2(w, img_h));
        let top = egui::CornerRadius { nw: 9, ne: 9, sw: 0, se: 0 };
        match texture {
            Some(tex) => {
                egui::Image::from_texture(egui::load::SizedTexture::from_handle(&tex)).corner_radius(top).paint_at(ui, img);
            }
            None if g.key == sfx::OTHER => {
                p.rect_filled(img, top, theme::PANEL);
                p.text(img.center(), Align2::CENTER_CENTER, icon::MUSIC_NOTES, FontId::proportional(26.0), FAINT);
            }
            None => {
                p.rect_filled(img, top, placeholder_color(&g.key));
                p.text(img.center(), Align2::CENTER_CENTER, initials(&g.label), FontId::new(24.0, theme::bold()), Color32::from_white_alpha(90));
            }
        }
        if opening && self.heroes.opening_key() == Some(format!("items:{}", g.key).as_str()) {
            p.rect_filled(img, top, Color32::from_black_alpha(170));
            p.text(img.center(), Align2::CENTER_CENTER, tr!("достаю звуки…", "extracting sounds…"), FontId::proportional(12.0), GOLD);
            ui.ctx().request_repaint();
        }
        let x = rect.left() + 8.0;
        let name_y = img.bottom() + 6.0;
        let mut job = egui::text::LayoutJob::simple_singleline(g.label.clone(), FontId::proportional(12.5), TEXT);
        job.wrap = egui::text::TextWrapping { max_width: w - 16.0, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        p.galley(egui::pos2(x, name_y), ui.painter().layout_job(job), TEXT);
        let count = if done > 0 { format!("{done} / {total}") } else { format!("— / {total}") };
        p.text(egui::pos2(x, name_y + 18.0), Align2::LEFT_TOP, count, FontId::proportional(11.0), MUTED);
        let bar = egui::Rect::from_min_size(egui::pos2(x, rect.bottom() - 7.0), vec2(w - 16.0, 3.0));
        p.rect_filled(bar, 1.5, BG_DEEP);
        if done > 0 {
            let mut fill = bar;
            fill.set_width((bar.width() * done as f32 / total.max(1) as f32).clamp(2.0, bar.width()));
            p.rect_filled(fill, 1.5, if done >= total { GREEN } else { GOLD });
        }
        if hovered || current || done > 0 {
            let color = if hovered { GOLD } else if current { GOLD.gamma_multiply(0.6) } else { GOLD.gamma_multiply(0.35) };
            p.rect_stroke(rect, 9.0, Stroke::new(1.5_f32, color), egui::StrokeKind::Inside);
        }
        let resp = resp.on_hover_text(trf!("{} · звуков: {total}", "{} · sounds: {total}", g.label));
        if opening { resp } else { resp.on_hover_cursor(egui::CursorIcon::PointingHand) }
    }
}
