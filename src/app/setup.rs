//! Мастер первого запуска: нашли Доту → какую озвучку переделываем → где хранить фразы.

use dota_revoice::game;
use eframe::egui::{self, Align, Layout, RichText, Sense, vec2};

use super::settings::{choice_chip, path_cell};
use super::theme::{self, AMBER, BG_DEEP, GOLD, GREEN, LINE, ON_GOLD, RED, TEXT, icon};
use super::App;
use crate::config::UiLanguage;

impl App {
    pub(super) fn setup_screen(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width().min(560.0);
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
            ui.vertical(|ui| {
                ui.set_width(width);
                egui::Frame::default()
                    .fill(theme::PANEL)
                    .stroke(egui::Stroke::new(1.0_f32, LINE))
                    .corner_radius(14)
                    .inner_margin(egui::Margin::symmetric(24, 22))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        self.setup_header(ui);
                        ui.add_space(8.0);
                        self.setup_game(ui);
                        ui.add_space(10.0);
                        self.setup_voice(ui);
                        ui.add_space(10.0);
                        self.setup_library(ui);
                        ui.add_space(16.0);
                        self.setup_footer(ui);
                    });
            });
        });
    }

    fn setup_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(38.0, 38.0), Sense::hover());
            ui.painter().rect_filled(rect, 10.0, GOLD);
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icon::MICROPHONE, egui::FontId::proportional(22.0), ON_GOLD);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(theme::title("Добро пожаловать в Dota Revoice", 18.0));
                ui.label(theme::small("Три вопроса — и можно записывать. Всё это потом меняется в настройках."));
            });
        });
    }

    fn setup_game(&mut self, ui: &mut egui::Ui) {
        let badge = match (&self.dota, &self.config.dota_dir) {
            (None, _) => RichText::new("не найдена").color(AMBER),
            (Some(_), Some(_)) => RichText::new(format!("{}  указана", icon::CHECK)).color(GREEN),
            (Some(_), None) => RichText::new(format!("{}  найдена", icon::CHECK)).color(GREEN),
        };
        let badge = |ui: &mut egui::Ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(badge.size(12.0)));
        };
        step(ui, 1, "Dota 2", badge, |ui| {
            let shown = self.dota.as_ref().map(|d| d.game_dir.parent().unwrap_or(&d.game_dir).to_path_buf());
            ui.horizontal(|ui| {
                path_cell(ui, shown.as_deref(), 120.0);
                let label = if shown.is_some() { "Изменить" } else { "Указать папку" };
                if ui.add(theme::tool(label)).clicked() {
                    self.pick_dota_dir();
                }
            });
            if shown.is_none() {
                ui.label(theme::small("Программа сама ищет Доту в библиотеках Steam. Не нашла — укажи папку «dota 2 beta». Можно и пропустить: записывать получится, а установить в игру — нет."));
            }
            if let Some(Err(e)) = &self.settings.result {
                ui.label(RichText::new(e.as_str()).size(12.0).color(RED));
            }
        });
    }

    fn setup_voice(&mut self, ui: &mut egui::Ui) {
        step(ui, 2, "Какую озвучку переделываем", |_| {}, |ui| {
            let Some(dota) = self.dota.clone() else {
                ui.label(theme::small("Выберется, когда найдётся Дота."));
                return;
            };
            if let Some(lang) = self.voice_language_chips(ui, &dota) {
                self.config.voice_language = (Some(&lang) != dota.audio_language.as_ref()).then_some(lang);
                self.dota = super::find_dota(&self.config);
            }
            let game_lang = game::language_name(dota.audio_language.as_deref());
            ui.label(theme::small(format!("Сейчас в игре включена {game_lang}. Видны только языки, скачанные в Steam.")));
        });
    }

    fn setup_library(&mut self, ui: &mut egui::Ui) {
        step(ui, 3, "Где хранить фразы", |_| {}, |ui| {
            let path = self.config.library_dir();
            ui.horizontal(|ui| {
                path_cell(ui, Some(&path), 120.0);
                if ui.add(theme::tool("Изменить")).clicked()
                    && let Some(dir) = rfd::FileDialog::new().set_title("Рабочая папка").pick_folder()
                {
                    self.config.library = Some(dir);
                }
            });
            ui.label(theme::small("Здесь будут твои записи и собранный пак. Каждую папку можно потом поменять отдельно."));
        });
    }

    fn setup_footer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(theme::small("Язык интерфейса"));
            choice_chip(ui, self.config.ui_language == UiLanguage::Russian, true, "Русский");
            choice_chip(ui, false, false, "English").on_disabled_hover_text("Английский интерфейс — в следующем обновлении");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(theme::primary(format!("Начать  {}", icon::ARROW_RIGHT)).min_size(vec2(120.0, 34.0))).clicked() {
                    self.finish_setup();
                }
            });
        });
    }

    fn finish_setup(&mut self) {
        let library = self.config.library_dir();
        if let Err(e) = std::fs::create_dir_all(&library) {
            self.settings.result = Some(Err(format!("Не создаётся {}: {e}", library.display())));
            return;
        }
        self.config.setup_done = true;
        self.config.save();
        self.settings.result = None;
        self.info(format!("Готово. Фразы будут храниться в {}", self.config.export_root().display()));
    }
}

/// Шаг мастера: кружок с номером, заголовок (справа — `badge`) и содержимое.
fn step(ui: &mut egui::Ui, n: u8, title: &str, badge: impl FnOnce(&mut egui::Ui), body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::default()
        .fill(theme::CARD)
        .stroke(egui::Stroke::new(1.0_f32, LINE))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
                ui.painter().circle_filled(rect.center(), 11.0, GOLD);
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, n.to_string(), egui::FontId::new(12.0, theme::bold()), ON_GOLD);
                ui.label(theme::title(title, 14.0).color(TEXT));
                badge(ui);
            });
            ui.add_space(4.0);
            egui::Frame::default().fill(egui::Color32::TRANSPARENT).show(ui, |ui| {
                ui.visuals_mut().extreme_bg_color = BG_DEEP;
                body(ui);
            });
        });
}
