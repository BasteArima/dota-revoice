//! Вкладка «Настройки»: игра и язык озвучки, папки, интерфейс, микрофон, «О программе».

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use dota_revoice::{game, update};
use eframe::egui::{self, Align, Layout, RichText, vec2};

use super::theme::{self, AMBER, BG_DEEP, FAINT, GOLD, GREEN, MUTED, RED, TEXT, icon};
use super::{App, dota_status, find_dota, open_folder, open_url, reveal};
use crate::config::{Config, UiLanguage};
use dota_revoice::{tr, trf};

/// Какая папка меняется.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Folder {
    Library,
    Export,
    Build,
}

/// Смена папки «Мои фразы» ждёт ответа: переносить ли туда записи.
struct PendingMove {
    from: PathBuf,
    to: PathBuf,
    /// Настройки, которые применятся после ответа.
    folder: Folder,
    value: Option<PathBuf>,
}

#[derive(Default)]
pub(super) struct SettingsState {
    pending_move: Option<PendingMove>,
    pub(super) result: Option<Result<String, String>>,
}

/// Что нажали в строке с папкой.
enum RowAction {
    Change,
    Show,
    Reset,
}

impl App {
    pub(super) fn settings_screen(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width().min(760.0);
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
            ui.vertical(|ui| {
                ui.set_width(width);
                ui.label(theme::title(tr!("Настройки", "Settings"), 22.0));
                ui.label(theme::small(tr!("Где игра, какую озвучку переделываем и где лежат файлы", "Where the game is, which voice-over you're remaking and where the files live")));
                ui.add_space(14.0);
                self.game_card(ui);
                ui.add_space(10.0);
                self.folders_card(ui);
                ui.add_space(10.0);
                ui.columns(2, |cols| {
                    self.interface_card(&mut cols[0]);
                    self.recording_card(&mut cols[1]);
                });
                ui.add_space(10.0);
                self.about_card(ui);
            });
        });
    }

    fn game_card(&mut self, ui: &mut egui::Ui) {
        theme::card(ui, |ui| {
            card_title(ui, icon::GAME_CONTROLLER, tr!("Игра", "Game"));
            let dota = self.dota.clone();
            let shown = dota.as_ref().map(|d| d.game_dir.parent().unwrap_or(&d.game_dir).to_path_buf());
            let tag = match (&dota, &self.config.dota_dir) {
                (None, _) => Some((tr!("не найдена", "not found"), AMBER)),
                (Some(_), Some(_)) => Some((tr!("указана вручную", "set manually"), MUTED)),
                (Some(_), None) => Some((tr!("найдена сама", "found automatically"), GREEN)),
            };
            let action = path_row(ui, tr!("Папка Dota 2", "Dota 2 folder"), tag, shown.as_deref(), self.config.dota_dir.is_some());
            match action {
                Some(RowAction::Change) => self.pick_dota_dir(),
                Some(RowAction::Show) => {
                    if let Some(p) = &shown {
                        open_folder(p);
                    }
                }
                Some(RowAction::Reset) => {
                    self.config.dota_dir = None;
                    self.after_game_change();
                }
                None => {}
            }

            let Some(dota) = dota else {
                ui.label(theme::small(tr!("Укажи папку «dota 2 beta» — обычно она в steamapps/common библиотеки Steam.", "Choose the “dota 2 beta” folder — it's usually in steamapps/common of a Steam library.")));
                return;
            };
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                label_cell(ui, tr!("Озвучка", "Voice-over"));
                if let Some(lang) = self.voice_language_chips(ui, &dota) {
                    // Совпадает с игрой — не запоминаем: тогда программа идёт за игрой и дальше.
                    self.config.voice_language = (Some(&lang) != dota.audio_language.as_ref()).then_some(lang);
                    self.after_game_change();
                }
            });
            ui.horizontal(|ui| {
                label_cell(ui, "");
                let game_lang = game::language_name(dota.audio_language.as_deref());
                if dota.audio_mismatch() {
                    ui.label(RichText::new(trf!(
                        "{}  В игре сейчас включена {game_lang} — мод будет слышно, только если переключить на неё озвучку игры", "{}  The game currently uses {game_lang} — you'll hear the mod only after switching the game's voice-over to this language",
                        icon::WARNING
                    )).size(12.0).color(AMBER));
                } else {
                    ui.label(theme::small(trf!("В игре сейчас включена {game_lang} — совпадает", "The game currently uses {game_lang} — it matches")));
                }
            });

            ui.add_space(4.0);
            if let Some(v) = dota.voice_pak() {
                let label = if v.enabled { tr!("Озвучка Valve", "Valve voice-over") } else { tr!("Озвучка Valve (откл.)", "Valve voice-over (off)") };
                if let Some(RowAction::Show) = info_row(ui, label, &v.path) {
                    reveal(&v.path);
                }
            }
            if let Some(dir) = dota.language_dir()
                && let Some(RowAction::Show) = info_row(ui, tr!("Папка модов", "Mods folder"), &dir)
            {
                open_folder(&dir);
            }
            ui.horizontal(|ui| {
                label_cell(ui, tr!("Сейчас в игре", "In the game now"));
                ui.label(RichText::new(dota_status(Some(&dota))).size(12.0).color(TEXT));
            });
        });
    }

    /// Чипы скачанных языков озвучки. Возвращает выбранный кликом.
    pub(super) fn voice_language_chips(&self, ui: &mut egui::Ui, dota: &game::DotaInstall) -> Option<String> {
        let mut picked = None;
        ui.horizontal_wrapped(|ui| {
            let langs = dota.voice_languages();
            if langs.is_empty() {
                ui.label(RichText::new(tr!("Скачанной озвучки нет — включи язык озвучки в Доте, Steam докачает пак", "No voice-over downloaded — pick a voice-over language in Dota and Steam will download the pack")).size(12.0).color(AMBER));
            }
            for lang in langs {
                let on = dota.voice_language.as_deref() == Some(lang.as_str());
                if choice_chip(ui, on, true, &capitalize(&game::language_name(Some(&lang)))).clicked() && !on {
                    picked = Some(lang);
                }
            }
            choice_chip(ui, false, false, tr!("Английская β", "English β"))
                .on_disabled_hover_text(tr!("Английская озвучка лежит в основном паке игры — появится после проверки в игре", "English voice-over lives in the game's main pack — coming once it's tested in game"));
        });
        picked
    }

    /// Чипы языка интерфейса: переключают сразу, без перезапуска.
    pub(super) fn ui_language_chips(&mut self, ui: &mut egui::Ui) {
        for (lang, label) in [(UiLanguage::Russian, "Русский"), (UiLanguage::English, "English")] {
            let on = self.config.ui_language == lang;
            if choice_chip(ui, on, true, label).clicked() && !on {
                self.config.ui_language = lang;
                self.config.apply_language();
                self.config.save();
            }
        }
    }

    pub(super) fn pick_dota_dir(&mut self) {
        let Some(dir) = rfd::FileDialog::new().set_title(tr!("Папка «dota 2 beta»", "The “dota 2 beta” folder")).pick_folder() else { return };
        match game::game_dir_of(&dir) {
            Some(game_dir) => {
                self.config.dota_dir = Some(game_dir.parent().unwrap_or(&game_dir).to_path_buf());
                self.after_game_change();
            }
            None => self.settings.result = Some(Err(trf!("В {} нет Доты: ищу папку game/dota внутри «dota 2 beta»", "No Dota in {}: looking for game/dota inside “dota 2 beta”", dir.display()))),
        }
    }

    /// Поменялась игра или язык озвучки: перечитываем и сохраняем.
    fn after_game_change(&mut self) {
        self.config.save();
        self.dota = find_dota(&self.config);
        self.alert = self.dota.as_ref().and_then(dota_revoice::install::check);
        self.build = Default::default();
        self.settings.result = None;
    }

    fn folders_card(&mut self, ui: &mut egui::Ui) {
        theme::card(ui, |ui| {
            card_title(ui, icon::FOLDERS, tr!("Папки", "Folders"));
            let c = &self.config;
            let rows = [
                (Folder::Library, tr!("Рабочая папка", "Workspace folder"), c.library_dir(), c.library.is_some()),
                (Folder::Export, tr!("Мои фразы", "My lines"), c.export_root(), c.export_dir.is_some()),
                (Folder::Build, tr!("Собранный пак", "Built pack"), c.build_root(), c.build_dir.is_some()),
            ];
            for (folder, label, path, custom) in rows {
                let tag = (!custom).then_some((tr!("по умолчанию", "default"), FAINT));
                match path_row(ui, label, tag, Some(&path), custom) {
                    Some(RowAction::Change) => {
                        if let Some(dir) = rfd::FileDialog::new().set_title(label).set_directory(&path).pick_folder() {
                            self.change_folder(folder, Some(dir));
                        }
                    }
                    Some(RowAction::Show) => open_folder(&path),
                    Some(RowAction::Reset) => self.change_folder(folder, None),
                    None => {}
                }
            }
            let originals = self.config.library_dir().join("originals");
            if let Some(RowAction::Show) = info_row(ui, tr!("Оригиналы из игры", "Originals from the game"), &originals) {
                open_folder(&originals);
            }
            ui.label(theme::small(
                tr!("Мои фразы по умолчанию лежат в рабочей папке, собранный пак — в моих фразах. Оригиналы программа достаёт из игры сама — их можно удалить, достанутся заново. При смене папки с фразами программа предложит перенести записи.", "By default “My lines” lives in the workspace folder and the built pack inside “My lines”. Originals are extracted from the game automatically — delete them and they'll be extracted again. When you change the lines folder, the app offers to move your recordings."),
            ));
            self.pending_move_box(ui);
            match &self.settings.result {
                Some(Ok(t)) => {
                    ui.label(RichText::new(format!("{}  {t}", icon::CHECK_CIRCLE)).size(12.0).color(GREEN));
                }
                Some(Err(t)) => {
                    ui.label(RichText::new(format!("{}  {t}", icon::WARNING)).size(12.0).color(RED));
                }
                None => {}
            }
        });
    }

    /// Меняет папку. Если от этого переезжают мои фразы — сначала спрашивает про перенос.
    fn change_folder(&mut self, folder: Folder, value: Option<PathBuf>) {
        self.settings.result = None;
        let mut next = self.config_snapshot();
        set_folder(&mut next, folder, value.clone());
        let (from, to) = (self.config.export_root(), next.export_root());
        if from != to && has_files(&from) {
            self.settings.pending_move = Some(PendingMove { from, to, folder, value });
            return;
        }
        self.apply_folder(folder, value);
    }

    fn apply_folder(&mut self, folder: Folder, value: Option<PathBuf>) {
        set_folder(&mut self.config, folder, value);
        self.config.save();
        self.reopen_project();
        self.build = Default::default();
    }

    /// Свои фразы переехали — открытого героя перечитываем из новой папки.
    fn reopen_project(&mut self) {
        if self.project.is_none() {
            return;
        }
        let selected = self.selected;
        let message = self.message.take();
        self.reopen_last();
        self.message = message;
        if self.project.as_ref().is_some_and(|p| selected < p.lines.len()) {
            self.select(selected);
        }
    }

    fn pending_move_box(&mut self, ui: &mut egui::Ui) {
        let Some(m) = &self.settings.pending_move else { return };
        let (from, to) = (m.from.clone(), m.to.clone());
        let mut answer = None;
        egui::Frame::default().fill(BG_DEEP).corner_radius(8).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(tr!("Перенести мои фразы в новую папку?", "Move my lines to the new folder?")).size(13.0).color(TEXT));
            ui.label(theme::small(trf!("Из  {}", "From  {}", from.display())));
            ui.label(theme::small(trf!("В    {}", "To      {}", to.display())));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.add(theme::primary(tr!("Перенести", "Move"))).clicked() {
                    answer = Some(true);
                }
                if theme::ghost(ui, true, tr!("Не переносить, начать с пустой", "Don't move, start empty")).clicked() {
                    answer = Some(false);
                }
                if theme::ghost(ui, true, tr!("Отмена", "Cancel")).clicked() {
                    self.settings.pending_move = None;
                }
            });
        });
        let Some(do_move) = answer else { return };
        let Some(m) = self.settings.pending_move.take() else { return };
        if do_move {
            match move_dir(&m.from, &m.to) {
                Ok(text) => self.settings.result = Some(Ok(text)),
                Err(e) => {
                    self.settings.result = Some(Err(format!("{e:#}")));
                    return;
                }
            }
        }
        self.apply_folder(m.folder, m.value);
    }

    fn interface_card(&mut self, ui: &mut egui::Ui) {
        theme::card(ui, |ui| {
            card_title(ui, icon::TRANSLATE, tr!("Интерфейс", "Interface"));
            ui.horizontal(|ui| {
                self.ui_language_chips(ui);
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if theme::chip(ui, self.config.check_updates, tr!("Проверять новые версии", "Check for new versions")).on_hover_text(tr!("Раз при запуске, через GitHub", "Once at startup, via GitHub")).clicked() {
                    self.config.check_updates = !self.config.check_updates;
                    self.config.save();
                }
            });
        });
    }

    fn recording_card(&mut self, ui: &mut egui::Ui) {
        theme::card(ui, |ui| {
            card_title(ui, icon::MICROPHONE, tr!("Запись", "Recording"));
            let current = self.recorder.as_ref().map(|r| r.device_name.clone()).unwrap_or_else(|| tr!("нет микрофона", "no microphone").into());
            let mut chosen = None;
            egui::ComboBox::from_id_salt("mic")
                .selected_text(current.as_str())
                .width(ui.available_width() - 8.0)
                .show_ui(ui, |ui| {
                    for name in &self.input_devices {
                        if ui.selectable_label(*name == current, name.as_str()).clicked() {
                            chosen = Some(name.clone());
                        }
                    }
                });
            if let Some(name) = chosen {
                self.open_recorder(Some(name));
                self.config.save();
            }
            ui.label(theme::small(tr!("Обрезка, громкость, шумодав и отсчёт — чипами на экране записи.", "Trim, loudness, noise reduction and countdown are chips on the Record screen.")));
        });
    }

    /// «О программе»: версия и обновления, автор, лицензия, ссылки на GitHub.
    fn about_card(&mut self, ui: &mut egui::Ui) {
        theme::card(ui, |ui| {
            card_title(ui, icon::INFO, tr!("О программе", "About"));
            ui.horizontal(|ui| {
                ui.label(theme::title(format!("Dota Revoice {}", update::current_version()), 15.0));
                ui.add_space(4.0);
                match (&self.latest, self.update_rx.is_some()) {
                    (_, true) => {
                        ui.spinner();
                        ui.label(theme::small(tr!("проверяю обновления…", "checking for updates…")));
                    }
                    (Some(Some(release)), false) => {
                        theme::pill(ui, &trf!("вышла {}", "{} is out", release.version), GOLD);
                        if release.installable {
                            if ui.add(theme::primary(trf!("{}  Обновить", "{}  Update", icon::ARROWS_CLOCKWISE))).clicked() {
                                let (ctx, release) = (ui.ctx().clone(), release.clone());
                                self.open_updater(&ctx, release, true);
                            }
                        } else if ui.add(theme::primary(trf!("{}  Скачать", "{}  Download", icon::DOWNLOAD_SIMPLE))).clicked() {
                            open_url(&release.url);
                        }
                    }
                    (Some(None), false) => theme::pill(ui, tr!("последняя версия", "up to date"), GREEN),
                    (None, false) => {
                        if theme::ghost(ui, true, trf!("{}  Проверить обновления", "{}  Check for updates", icon::ARROWS_CLOCKWISE)).clicked() {
                            let ctx = ui.ctx().clone();
                            self.start_update_check(&ctx);
                        }
                    }
                }
            });
            ui.label(theme::small(tr!(
                "Любительская переозвучка героев Dota 2. Фанатский проект, не связан с Valve.",
                "Amateur re-voicing of Dota 2 heroes. A fan project, not affiliated with Valve."
            )));
            ui.label(theme::small(tr!("Автор — BasteArima · лицензия GPL-3.0, исходный код открыт", "By BasteArima · GPL-3.0 license, open source")));
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let repo = format!("https://github.com/{}", update::REPO);
                let links = [
                    (icon::GITHUB_LOGO, format!("github.com/{}", update::REPO), repo.clone()),
                    (icon::BUG, tr!("Сообщить о проблеме", "Report a problem").to_owned(), format!("{repo}/issues")),
                    (icon::TAG, tr!("Все версии", "All releases").to_owned(), format!("{repo}/releases")),
                ];
                for (glyph, text, url) in links {
                    if theme::ghost_colored(ui, true, format!("{glyph}  {text}"), GOLD).on_hover_text(&url).clicked() {
                        open_url(&url);
                    }
                }
            });
        });
    }

    /// Копия настроек для расчёта «что будет, если поменять».
    fn config_snapshot(&self) -> Config {
        Config {
            library: self.config.library.clone(),
            export_dir: self.config.export_dir.clone(),
            build_dir: self.config.build_dir.clone(),
            ..Config::default()
        }
    }
}

fn set_folder(config: &mut Config, folder: Folder, value: Option<PathBuf>) {
    match folder {
        Folder::Library => config.library = value,
        Folder::Export => config.export_dir = value,
        Folder::Build => config.build_dir = value,
    }
}

fn has_files(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut it| it.next().is_some())
}

/// Переносит папку целиком. На тот же диск — переименованием, на другой — копией
/// (старая папка остаётся: удалять записи мы не берёмся).
fn move_dir(from: &Path, to: &Path) -> Result<String> {
    ensure!(!has_files(to), trf!("в {} уже есть файлы — выбери пустую папку или не переноси", "{} already has files — pick an empty folder or don't move", to.display()));
    ensure!(!to.starts_with(from), tr!("новая папка внутри старой — так перенести нельзя", "the new folder is inside the old one — can't move like that"));
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).with_context(|| trf!("не создаётся {}", "can't create {}", parent.display()))?;
    }
    let _ = std::fs::remove_dir(to); // пустая папка мешает переименованию
    if std::fs::rename(from, to).is_ok() {
        return Ok(trf!("Мои фразы перенесены в {}", "My lines moved to {}", to.display()));
    }
    copy_dir(from, to)?;
    Ok(trf!("Мои фразы скопированы в {} (на другой диск). Старая папка осталась: {}", "My lines copied to {} (another drive). The old folder is still there: {}", to.display(), from.display()))
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| trf!("не создаётся {}", "can't create {}", to.display()))?;
    for entry in std::fs::read_dir(from).with_context(|| trf!("не читается {}", "can't read {}", from.display()))? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if src.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).with_context(|| trf!("не скопировался {}", "couldn't copy {}", src.display()))?;
        }
    }
    Ok(())
}

pub(super) fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn card_title(ui: &mut egui::Ui, icon: &str, text: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(icon).size(17.0).color(GOLD));
        ui.label(theme::title(text, 15.0));
    });
    ui.add_space(2.0);
}

const LABEL_WIDTH: f32 = 150.0;

fn label_cell(ui: &mut egui::Ui, text: &str) {
    ui.allocate_ui_with_layout(vec2(LABEL_WIDTH, 26.0), Layout::left_to_right(Align::Center), |ui| {
        ui.set_width(LABEL_WIDTH);
        ui.label(RichText::new(text).size(13.0).color(MUTED));
    });
}

/// Сколько места справа от пути занимают кнопки: пути во всех строках одной ширины.
const BUTTONS_WIDTH: f32 = 166.0;

/// Строка «название · путь · Показать · Изменить · Сбросить».
fn path_row(ui: &mut egui::Ui, label: &str, tag: Option<(&str, egui::Color32)>, path: Option<&Path>, can_reset: bool) -> Option<RowAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(vec2(LABEL_WIDTH, 26.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(LABEL_WIDTH);
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(RichText::new(label).size(13.0).color(MUTED));
            if let Some((t, color)) = tag {
                ui.label(RichText::new(t).size(11.0).color(color));
            }
        });
        path_cell(ui, path, BUTTONS_WIDTH);
        if theme::ghost(ui, path.is_some(), icon::FOLDER_OPEN).on_hover_text(tr!("Открыть папку", "Open folder")).clicked() {
            action = Some(RowAction::Show);
        }
        if ui.add(theme::tool(tr!("Изменить", "Change"))).clicked() {
            action = Some(RowAction::Change);
        }
        if can_reset && theme::ghost(ui, true, icon::ARROW_COUNTER_CLOCKWISE).on_hover_text(tr!("Вернуть как по умолчанию", "Reset to default")).clicked() {
            action = Some(RowAction::Reset);
        }
    });
    action
}

/// Строка только для чтения: «название · путь · Показать».
fn info_row(ui: &mut egui::Ui, label: &str, path: &Path) -> Option<RowAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        label_cell(ui, label);
        path_cell(ui, Some(path), BUTTONS_WIDTH);
        if theme::ghost(ui, true, icon::FOLDER_OPEN).on_hover_text(tr!("Показать в проводнике", "Show in file manager")).clicked() {
            action = Some(RowAction::Show);
        }
    });
    action
}

/// Путь в тёмной плашке на всю ширину, кроме `reserve` справа.
pub(super) fn path_cell(ui: &mut egui::Ui, path: Option<&Path>, reserve: f32) {
    let width = (ui.available_width() - reserve).max(120.0);
    ui.allocate_ui_with_layout(vec2(width, 28.0), Layout::left_to_right(Align::Center), |ui| {
        egui::Frame::default().fill(BG_DEEP).corner_radius(6).inner_margin(egui::Margin::symmetric(8, 5)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Моноширинный шрифт: длинный путь режем слева — конец пути важнее начала.
            let fits = ((ui.available_width() / 6.7) as usize).max(8);
            let text = path.map(|p| p.display().to_string()).unwrap_or_else(|| "—".into());
            let chars = text.chars().count();
            let text = if chars > fits { format!("…{}", text.chars().skip(chars - fits + 1).collect::<String>()) } else { text };
            ui.add(egui::Label::new(RichText::new(text).monospace().size(12.0).color(MUTED)).truncate())
                .on_hover_text(path.map(|p| p.display().to_string()).unwrap_or_default());
        });
    });
}

/// Чип выбора одного из вариантов (язык): выбранный — золотой.
pub(super) fn choice_chip(ui: &mut egui::Ui, on: bool, enabled: bool, text: &str) -> egui::Response {
    let label = if on { format!("{}  {text}", icon::CHECK) } else { text.to_owned() };
    let mut btn = egui::Button::new(RichText::new(label).size(13.0).color(if on { GOLD } else { MUTED })).corner_radius(99);
    if on {
        btn = btn.fill(GOLD.gamma_multiply(0.12)).stroke(egui::Stroke::new(1.0_f32, GOLD.gamma_multiply(0.6)));
    }
    ui.add_enabled(enabled, btn)
}
