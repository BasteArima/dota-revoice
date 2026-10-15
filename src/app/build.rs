//! Вкладка «Сборка и установка»: три шага с понятным статусом у каждого.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime};

use dota_revoice::install::ModState;
use dota_revoice::{exchange, game, install, pack};
use eframe::egui::{self, Align, Align2, Color32, FontId, RichText, Sense, Stroke, vec2};

use super::theme::{self, AMBER, BG_DEEP, FAINT, GOLD, GREEN, LINE, MUTED, ON_GOLD, RED, TEXT, icon};
use super::App;

/// Как часто перечитывать состояние файлов, пока открыта вкладка.
const REFRESH: Duration = Duration::from_secs(2);

struct PackJob {
    progress: Arc<AtomicU32>,
    rx: mpsc::Receiver<anyhow::Result<pack::Summary>>,
}

/// Действие с папкой игры, ждущее подтверждения.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GameAction {
    Install,
    Enable,
    Disable,
    Rollback,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Installed {
    No,
    Same,
    Other,
}

/// Снимок состояния файлов: что собрано, что лежит в игре.
struct Snapshot {
    mine: usize,
    /// Время самого свежего изменения в экспорте (запись, обрезка, корзина).
    newest_change: Option<SystemTime>,
    built: Option<(u64, SystemTime)>,
    installed: Installed,
    mod_state: ModState,
    valve_enabled: bool,
    has_changes: bool,
    dota_running: bool,
    /// Steam обновил озвучку Valve после нашей сборки — в паке старые фразы Valve.
    valve_newer: bool,
}

impl Snapshot {
    fn needs_rebuild(&self) -> bool {
        if self.valve_newer {
            return true;
        }
        match (self.built, self.newest_change) {
            (None, _) => true,
            (Some((_, built)), Some(changed)) => changed > built,
            (Some(_), None) => false,
        }
    }

    fn installed_ok(&self) -> bool {
        self.installed == Installed::Same && self.mod_state == ModState::Enabled && !self.valve_enabled
    }

    /// В игре звучат мои фразы (мод включён и не перекрыт паком Valve).
    fn mine_audible(&self) -> bool {
        self.mod_state == ModState::Enabled && !self.valve_enabled
    }
}

pub(super) struct BuildState {
    use_base: bool,
    job: Option<PackJob>,
    summary: Option<Result<String, String>>,
    confirm: Option<GameAction>,
    dota_running: bool,
    game_task: Option<mpsc::Receiver<Result<String, String>>>,
    game_result: Option<Result<String, String>>,
    snapshot: Option<Snapshot>,
    refreshed: Option<Instant>,
    /// При импорте пачки заменять фразы, которые у меня уже есть.
    import_replace: bool,
    exchange_result: Option<Result<String, String>>,
}

impl Default for BuildState {
    fn default() -> Self {
        Self {
            use_base: true,
            job: None,
            summary: None,
            confirm: None,
            dota_running: false,
            game_task: None,
            game_result: None,
            snapshot: None,
            refreshed: None,
            import_replace: false,
            exchange_result: None,
        }
    }
}

impl BuildState {
    pub(super) fn refresh_now(&mut self) {
        self.refreshed = None;
    }

    pub(super) fn busy(&self) -> bool {
        self.job.is_some() || self.game_task.is_some()
    }
}

#[derive(Clone, Copy)]
enum StepState {
    Done,
    Current,
    Later,
}

impl App {
    fn built_pak(&self) -> Option<PathBuf> {
        Some(self.config.build_root().join(install::PAK_NAME))
    }

    /// Забирает результаты фоновой сборки и установки.
    pub(super) fn poll_build_tasks(&mut self) {
        if let Some(job) = &self.build.job
            && let Ok(result) = job.rx.try_recv()
        {
            self.build.summary = Some(match result {
                Ok(s) => {
                    let mut text = format!(
                        "Собрано: твоих фраз {}, из озвучки игры {}, {:.0} МБ",
                        s.replaced + s.not_in_base.len(),
                        s.copied,
                        s.size_bytes as f64 / 1_048_576.0
                    );
                    if !s.not_in_base.is_empty() {
                        text += &format!(". В озвучке игры нет: {} — проверь имена", s.not_in_base.join(", "));
                    }
                    Ok(text)
                }
                Err(e) => Err(format!("{e:#}")),
            });
            self.build.job = None;
            self.build.refresh_now();
        }
        if let Some(rx) = &self.build.game_task
            && let Ok(result) = rx.try_recv()
        {
            self.build.game_result = Some(result);
            self.build.game_task = None;
            self.build.refresh_now();
        }
    }

    fn refresh_snapshot(&mut self) {
        if self.build.refreshed.is_some_and(|t| t.elapsed() < REFRESH) {
            return;
        }
        self.build.refreshed = Some(Instant::now());
        let (Some(p), Some(built_path)) = (&self.project, self.built_pak()) else { return };
        let mine = pack::collect_export(&p.export_root).unwrap_or_default();
        let built = std::fs::metadata(&built_path).ok().and_then(|m| Some((m.len(), m.modified().ok()?)));
        let in_game = self.dota.as_ref().and_then(install::installed_pak).and_then(|p| std::fs::metadata(p).ok());
        // Выключение и включение — переименование, время файла сохраняется.
        let installed = match (in_game, built) {
            (None, _) => Installed::No,
            (Some(m), Some((len, at))) if m.len() == len && m.modified().is_ok_and(|t| t >= at) => Installed::Same,
            (Some(_), _) => Installed::Other,
        };
        self.build.snapshot = Some(Snapshot {
            mine: mine.len(),
            newest_change: newest_change(&p.export_root, mine.values()),
            built,
            installed,
            mod_state: self.dota.as_ref().map_or(ModState::Absent, install::mod_state),
            valve_enabled: self.dota.as_ref().and_then(|d| d.voice_pak()).is_some_and(|v| v.enabled),
            has_changes: self.dota.as_ref().is_some_and(install::has_changes),
            dota_running: install::is_dota_running(),
            valve_newer: self.build.use_base
                && match (built, self.dota.as_ref().and_then(install::valve_pak_modified)) {
                    (Some((_, built)), Some(valve)) => valve > built,
                    _ => false,
                },
        });
    }

    fn start_build(&mut self, ctx: &egui::Context) {
        let (Some(p), Some(out)) = (&self.project, self.built_pak()) else { return };
        let base = self.dota.as_ref().and_then(|d| d.voice_pak()).filter(|_| self.build.use_base).map(|v| v.path);
        let progress = Arc::new(AtomicU32::new(0));
        let (tx, rx) = mpsc::channel();
        let export_root = p.export_root.clone();
        let (prog2, ctx2) = (progress.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = pack::collect_export(&export_root).and_then(|mine| {
                pack::build(base.as_deref(), &mine, &out, |f| {
                    prog2.store((f * 1000.0) as u32, Ordering::Relaxed);
                    ctx2.request_repaint();
                })
            });
            let _ = tx.send(result);
            ctx2.request_repaint();
        });
        self.build.summary = None;
        self.build.job = Some(PackJob { progress, rx });
    }

    fn start_game_action(&mut self, ctx: &egui::Context, action: GameAction) {
        let (Some(dota), Some(built)) = (self.dota.clone(), self.built_pak()) else { return };
        let (tx, rx) = mpsc::channel();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let result = match action {
                GameAction::Install => install::Plan::for_install(&dota)
                    .and_then(|plan| install::install(&plan, &built))
                    .map(|()| "Установлено. Запусти Доту — новые фразы подхватятся при старте".to_owned()),
                GameAction::Enable => install::set_enabled(&dota, true)
                    .map(|_| "Мод включён — перезапусти Доту, чтобы услышать свои фразы".to_owned()),
                GameAction::Disable => install::set_enabled(&dota, false)
                    .map(|_| "Мод выключен — в игре озвучка Valve. Включить обратно можно в любой момент".to_owned()),
                GameAction::Rollback => install::rollback(&dota).map(|done| done.join(". ")),
            };
            let _ = tx.send(result.map_err(|e| format!("{e:#}")));
            ctx2.request_repaint();
        });
        self.build.confirm = None;
        self.build.game_result = None;
        self.build.game_task = Some(rx);
    }

    pub(super) fn build_screen(&mut self, ui: &mut egui::Ui) {
        self.refresh_snapshot();
        let Some(snap) = self.build.snapshot.take() else { return };
        let ctx = ui.ctx().clone();
        let width = ui.available_width().min(680.0);
        let current = if snap.mine == 0 || snap.needs_rebuild() {
            1
        } else if !snap.installed_ok() || snap.mod_state == ModState::Disabled {
            2
        } else {
            3
        };
        let state = |n: u8| match n.cmp(&current) {
            std::cmp::Ordering::Less => StepState::Done,
            std::cmp::Ordering::Equal => StepState::Current,
            std::cmp::Ordering::Greater => StepState::Later,
        };

        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
            ui.vertical(|ui| {
                ui.set_width(width);
                ui.label(theme::title("Сборка и установка", 22.0));
                ui.label(theme::small("Три шага — и твой голос в игре"));
                ui.add_space(14.0);

                self.step_build(ui, &ctx, &snap, state(1));
                ui.add_space(10.0);
                self.step_install(ui, &ctx, &snap, state(2));
                ui.add_space(10.0);
                self.step_launch(ui, &snap, current, state(3));
                ui.add_space(22.0);
                self.exchange_card(ui);
            });
        });
        self.build.snapshot = Some(snap);
    }

    fn step_build(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, snap: &Snapshot, state: StepState) {
        step_card(ui, 1, state, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::title("Собрать пак", 15.0));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| match (snap.built.is_some(), snap.needs_rebuild()) {
                    (true, false) => theme::pill(ui, "собран", GREEN),
                    (true, true) if snap.valve_newer => theme::pill(ui, "озвучка игры обновилась", AMBER),
                    (true, true) => theme::pill(ui, "есть новые записи", AMBER),
                    _ => theme::pill(ui, "не собран", MUTED),
                });
            });
            let mut info = if snap.mine == 0 {
                "Пока нет записанных фраз — запиши хотя бы одну на вкладке «Запись».".to_owned()
            } else {
                format!("Твоих фраз: {}", snap.mine)
            };
            if let Some((size, at)) = snap.built {
                info += &format!(" · пак {:.0} МБ, собран {}", size as f64 / 1_048_576.0, ago(at));
            }
            ui.label(theme::small(info));
            if snap.valve_newer {
                ui.label(RichText::new("Steam обновил озвучку Valve после сборки — пересобери, чтобы в паке были свежие фразы игры.").size(12.0).color(AMBER));
            }

            if self.dota.as_ref().and_then(|d| d.voice_pak()).is_some() {
                let text = format!(
                    "{}  Остальные фразы взять из озвучки игры",
                    if self.build.use_base { icon::CHECK_SQUARE } else { icon::SQUARE }
                );
                let color = if self.build.use_base { TEXT } else { MUTED };
                if ui.add(egui::Button::new(RichText::new(text).size(12.0).color(color)).frame(false)).clicked() {
                    self.build.use_base = !self.build.use_base;
                }
                if !self.build.use_base {
                    ui.label(RichText::new("Без этого все незаписанные фразы в игре станут английскими").size(12.0).color(AMBER));
                }
            }
            ui.add_space(4.0);
            if let Some(job) = &self.build.job {
                let frac = job.progress.load(Ordering::Relaxed) as f32 / 1000.0;
                ui.add(egui::ProgressBar::new(frac).fill(GOLD).text(RichText::new("Собираю…").color(ON_GOLD)).desired_height(22.0));
            } else {
                let label = format!("{}  {}", icon::PACKAGE, if snap.built.is_some() { "Пересобрать" } else { "Собрать пак" });
                let btn = if snap.needs_rebuild() { theme::primary(label) } else { egui::Button::new(label) };
                if ui.add_enabled(snap.mine > 0, btn).clicked() {
                    self.start_build(ctx);
                }
            }
            result_line(ui, self.build.summary.as_ref());
        });
    }

    fn step_install(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, snap: &Snapshot, state: StepState) {
        step_card(ui, 2, state, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::title("Установить в игру", 15.0));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| match snap.mod_state {
                    ModState::Absent => theme::pill(ui, "не установлено", MUTED),
                    ModState::Disabled => theme::pill(ui, "мод выключен", MUTED),
                    ModState::Enabled if snap.valve_enabled => theme::pill(ui, "Steam вернул озвучку Valve", AMBER),
                    ModState::Enabled if snap.installed == Installed::Other => theme::pill(ui, "в игре старая версия", AMBER),
                    ModState::Enabled => theme::pill(ui, "мод включён", GREEN),
                });
            });
            let Some(dota) = self.dota.clone() else {
                ui.label(theme::small("Дота не найдена в библиотеках Steam — укажи её папку в настройках."));
                return;
            };
            let lang = game::language_name(dota.voice_language.as_deref());
            ui.label(theme::small(format!("Dota 2 · озвучка {lang}. Перед изменениями закрой Доту.")));
            ui.add_space(4.0);

            if self.build.game_task.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(theme::small("Меняю файлы игры…"));
                });
                return;
            }
            match self.build.confirm {
                None if snap.mod_state == ModState::Absent => {
                    let can_install = snap.built.is_some();
                    if ui.add_enabled(can_install, theme::primary(format!("{}  Установить", icon::DOWNLOAD_SIMPLE))).clicked() {
                        self.ask(GameAction::Install);
                    }
                    if snap.has_changes {
                        // Нашего пака нет, но пак Valve отключён — например, его удалили руками.
                        if theme::ghost(ui, true, format!("{}  Вернуть озвучку Valve", icon::ARROW_COUNTER_CLOCKWISE)).clicked() {
                            self.ask(GameAction::Rollback);
                        }
                    }
                }
                None => {
                    if let Some(action) = mod_switch(ui, snap.mine_audible()) {
                        self.ask(action);
                    }
                    let text = if snap.mine_audible() {
                        "Слышны твои фразы, остальное — оригинал."
                    } else if snap.mod_state == ModState::Disabled {
                        "Пак лежит в игре выключенным — включится мгновенно, без пересборки."
                    } else {
                        "Пак Valve снова включён и перекрывает твой — включи мод обратно."
                    };
                    ui.label(theme::small(text));
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        let stale = snap.built.is_some() && snap.installed != Installed::Same;
                        if stale && ui.add(theme::primary(format!("{}  Обновить пак", icon::ARROWS_CLOCKWISE))).on_hover_text("Положить в игру свежую сборку").clicked() {
                            self.ask(GameAction::Install);
                        }
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            let remove = theme::ghost(ui, true, format!("{}  Убрать из игры", icon::TRASH));
                            if remove.on_hover_text("Удалить наш пак из игры совсем и вернуть озвучку Valve").clicked() {
                                self.ask(GameAction::Rollback);
                            }
                        });
                    });
                }
                Some(action) => self.confirm_box(ui, ctx, &dota, action),
            }
            result_line(ui, self.build.game_result.as_ref());
        });
    }

    fn ask(&mut self, action: GameAction) {
        self.build.confirm = Some(action);
        self.build.game_result = None;
        self.build.dota_running = install::is_dota_running();
    }

    /// Что изменится в папке игры — и кнопки «сделать» / «отмена».
    fn confirm_box(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, dota: &game::DotaInstall, action: GameAction) {
        let valve_on = dota.voice_pak().is_some_and(|v| v.enabled);
        let steps = match action {
            GameAction::Install => install::Plan::for_install(dota).map(|p| p.describe()),
            GameAction::Enable => {
                let mut steps = Vec::new();
                if install::mod_state(dota) == ModState::Disabled {
                    steps.push(format!("Переименовать {} в {} — включить мой пак", install::PAK_OFF, install::PAK_NAME));
                }
                if valve_on {
                    steps.push("Переименовать pak01_dir.vpk в pak01_dir.vpk.off — отключить озвучку Valve".to_owned());
                }
                Ok(steps)
            }
            GameAction::Disable => Ok(vec![
                format!("Переименовать {} в {} — мой пак остаётся в игре, но не грузится", install::PAK_NAME, install::PAK_OFF),
                "Вернуть pak01_dir.vpk — озвучку Valve".to_owned(),
            ]),
            GameAction::Rollback => Ok(vec![
                format!("Удалить наш пак ({}) из папки языка", install::PAK_NAME),
                "Вернуть pak01_dir.vpk — озвучку Valve".to_owned(),
            ]),
        };
        egui::Frame::default().fill(BG_DEEP).corner_radius(8).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            match steps {
                Ok(steps) => {
                    ui.label(RichText::new("Что изменится в папке игры:").size(12.0).color(TEXT));
                    for step in steps {
                        ui.label(theme::small(format!("•  {step}")));
                    }
                    if self.build.dota_running {
                        ui.label(RichText::new(format!("{}  Дота запущена — закрой её, иначе файлы заняты", icon::WARNING)).size(12.0).color(RED));
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let label = match action {
                            GameAction::Install => "Установить",
                            GameAction::Enable => "Включить",
                            GameAction::Disable => "Выключить",
                            GameAction::Rollback => "Убрать из игры",
                        };
                        if ui.add(theme::primary(label)).clicked() {
                            self.start_game_action(ctx, action);
                        }
                        if theme::ghost(ui, true, "Отмена").clicked() {
                            self.build.confirm = None;
                        }
                    });
                }
                Err(e) => {
                    ui.label(RichText::new(format!("{e:#}")).size(12.0).color(RED));
                    if theme::ghost(ui, true, "Закрыть").clicked() {
                        self.build.confirm = None;
                    }
                }
            }
        });
    }

    fn step_launch(&mut self, ui: &mut egui::Ui, snap: &Snapshot, current: u8, state: StepState) {
        step_card(ui, 3, state, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::title("Запустить Доту", 15.0));
                if snap.dota_running {
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| theme::pill(ui, "Дота запущена", GREEN));
                }
            });
            let (text, color) = match (current, snap.dota_running) {
                _ if snap.mod_state == ModState::Disabled => ("Мод выключен — в игре будет озвучка Valve. Включи его на шаге 2, чтобы слышать свои фразы.", MUTED),
                (3, true) => ("Дота уже открыта. Игра читает паки только при старте — закрой её и запусти заново, чтобы услышать новые фразы.", AMBER),
                (3, false) => ("Всё готово — запускай. Новые фразы подхватятся при старте игры.", MUTED),
                _ => ("Сначала собери и установи пак — иначе в игре будет прежняя озвучка.", MUTED),
            };
            ui.label(RichText::new(text).size(12.0).color(color));
            ui.add_space(4.0);
            let label = format!("{}  Запустить Доту", icon::PLAY);
            let btn = if current == 3 && !snap.dota_running { theme::primary(label) } else { egui::Button::new(label) };
            if ui.add_enabled(self.dota.is_some(), btn).on_hover_text("Через Steam, как обычный ярлык игры").clicked() {
                match install::launch_dota() {
                    Ok(()) => self.info("Запускаю Доту через Steam…"),
                    Err(e) => self.error(format!("{e:#}")),
                }
            }
        });
    }

    /// Обмен фразами: собрать свои в один zip или добавить присланные.
    fn exchange_card(&mut self, ui: &mut egui::Ui) {
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::USERS_THREE).size(18.0).color(GOLD));
                ui.label(theme::title("Обмен с друзьями", 15.0));
            });
            ui.label(theme::small(
                "Собери все свои фразы в один файл и отправь другу — или добавь фразы, которые прислали тебе. Потом пересобери и установи пак.",
            ));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.add(theme::tool(format!("{}  Экспорт пачки…", icon::EXPORT))).on_hover_text("Все записанные фразы всех героев + заметки, в zip").clicked() {
                    self.export_pack();
                }
                if ui.add(theme::tool(format!("{}  Импорт пачки…", icon::DOWNLOAD_SIMPLE))).on_hover_text("zip, который прислал друг").clicked() {
                    self.import_pack();
                }
                if theme::chip(ui, self.build.import_replace, "заменять мои версии").on_hover_text("Твоя версия уйдёт в дубли — её можно вернуть").clicked() {
                    self.build.import_replace = !self.build.import_replace;
                }
            });
            result_line(ui, self.build.exchange_result.as_ref());
        });
    }

    fn export_pack(&mut self) {
        let Some(p) = &self.project else { return };
        let Some(out) = rfd::FileDialog::new()
            .set_title("Куда сохранить пачку фраз")
            .set_file_name("dota_revoice_фразы.zip")
            .add_filter("Пачка фраз", &["zip"])
            .save_file()
        else {
            return;
        };
        self.build.exchange_result = Some(match exchange::export_pack(&p.export_root, &out) {
            Ok(n) => Ok(format!("В пачке {n} фраз: {}", out.display())),
            Err(e) => Err(format!("{e:#}")),
        });
    }

    fn import_pack(&mut self) {
        let Some(root) = self.project.as_ref().map(|p| p.export_root.clone()) else { return };
        let Some(file) = rfd::FileDialog::new().set_title("Пачка фраз от друга").add_filter("Пачка фраз", &["zip"]).pick_file() else {
            return;
        };
        let result = exchange::import_pack(&root, &file, self.build.import_replace);
        self.build.exchange_result = Some(match result {
            Ok(s) => {
                let mut parts = vec![format!("Добавлено {}", s.added)];
                if s.replaced > 0 {
                    parts.push(format!("заменено {} (твои версии — в дублях)", s.replaced));
                }
                if !s.kept.is_empty() {
                    parts.push(format!("оставлены твои: {}", s.kept.len()));
                }
                if !s.rejected.is_empty() {
                    parts.push(format!("пропущены лишние файлы: {}", s.rejected.len()));
                }
                Ok(parts.join(" · "))
            }
            Err(e) => Err(format!("{e:#}")),
        });
        // Новые фразы могут быть и у открытого героя — перечитываем.
        if let Some(p) = self.project.as_mut() {
            p.rescan();
        }
        self.select(self.selected);
        self.build.refresh_now();
    }
}

/// Карточка шага с кружком-номером слева.
fn step_card(ui: &mut egui::Ui, n: u8, state: StepState, add: impl FnOnce(&mut egui::Ui)) {
    let border = if matches!(state, StepState::Current) { GOLD } else { LINE };
    theme::card_with(ui, border, |ui| {
        ui.horizontal_top(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
            let p = ui.painter();
            let c = rect.center();
            let font = FontId::new(13.0, theme::bold());
            match state {
                StepState::Done => {
                    p.circle_filled(c, 13.0, GREEN);
                    p.text(c, Align2::CENTER_CENTER, icon::CHECK, FontId::proportional(15.0), Color32::from_rgb(11, 36, 16));
                }
                StepState::Current => {
                    p.circle_filled(c, 13.0, GOLD);
                    p.text(c, Align2::CENTER_CENTER, n.to_string(), font, ON_GOLD);
                }
                StepState::Later => {
                    p.circle_stroke(c, 12.5, Stroke::new(1.0_f32, LINE));
                    p.text(c, Align2::CENTER_CENTER, n.to_string(), font, FAINT);
                }
            }
            ui.add_space(4.0);
            ui.vertical(|ui| {
                if matches!(state, StepState::Later) {
                    ui.multiply_opacity(0.6);
                }
                add(ui);
            });
        });
    });
}

/// Переключатель «Моя озвучка / Озвучка Valve». Возвращает действие, если выбрали другое.
fn mod_switch(ui: &mut egui::Ui, mine: bool) -> Option<GameAction> {
    let mut action = None;
    egui::Frame::default().fill(BG_DEEP).stroke(Stroke::new(1.0_f32, LINE)).corner_radius(10).inner_margin(egui::Margin::same(3)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.horizontal(|ui| {
            let half = (ui.available_width() - 2.0) / 2.0;
            let options = [
                (true, format!("{}  Моя озвучка", icon::MICROPHONE), GOLD, GameAction::Enable),
                (false, "Озвучка Valve".to_owned(), TEXT, GameAction::Disable),
            ];
            for (is_mine, label, color, act) in options {
                let on = is_mine == mine;
                let text = RichText::new(label).size(13.0).color(if on { color } else { MUTED });
                let fill = match (on, is_mine) {
                    (true, true) => GOLD.gamma_multiply(0.14),
                    (true, false) => theme::CARD,
                    _ => Color32::TRANSPARENT,
                };
                let btn = egui::Button::new(text).fill(fill).stroke(Stroke::NONE).corner_radius(8).min_size(vec2(half, 30.0));
                if ui.add(btn).clicked() && !on {
                    action = Some(act);
                }
            }
        });
    });
    action
}

fn result_line(ui: &mut egui::Ui, result: Option<&Result<String, String>>) {
    match result {
        Some(Ok(text)) => {
            ui.label(RichText::new(format!("{}  {text}", icon::CHECK_CIRCLE)).size(12.0).color(GREEN));
        }
        Some(Err(text)) => {
            ui.label(RichText::new(format!("{}  {text}", icon::WARNING)).size(12.0).color(RED));
        }
        None => {}
    }
}

/// Самое свежее изменение экспорта: mp3-файлы и папки корзины (удаление меняет их время).
fn newest_change<'a>(export_root: &Path, mp3s: impl Iterator<Item = &'a PathBuf>) -> Option<SystemTime> {
    let mut newest = mp3s.filter_map(|p| std::fs::metadata(p).ok()?.modified().ok()).max();
    let mut stack = vec![export_root.join(".trash")];
    while let Some(dir) = stack.pop() {
        let Ok(meta) = std::fs::metadata(&dir) else { continue };
        newest = newest.max(meta.modified().ok());
        if let Ok(entries) = std::fs::read_dir(&dir) {
            stack.extend(entries.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()));
        }
    }
    newest
}

/// «только что», «5 мин назад», «2 ч назад».
pub(super) fn ago(t: SystemTime) -> String {
    let secs = SystemTime::now().duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..60 => "только что".into(),
        60..3600 => format!("{} мин назад", secs / 60),
        3600..86400 => format!("{} ч назад", secs / 3600),
        _ => format!("{} дн. назад", secs / 86400),
    }
}
