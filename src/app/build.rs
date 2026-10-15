//! Вкладка «Сборка и установка»: три шага с понятным статусом у каждого.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime};

use dota_revoice::{install, pack};
use eframe::egui::{self, Align, Align2, Color32, FontId, RichText, Sense, Stroke, vec2};

use super::theme::{self, AMBER, BG_DEEP, FAINT, GOLD, GREEN, LINE, MUTED, ON_GOLD, RED, TEXT, icon};
use super::{App, reveal};

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
    valve_enabled: bool,
    has_changes: bool,
}

impl Snapshot {
    fn needs_rebuild(&self) -> bool {
        match (self.built, self.newest_change) {
            (None, _) => true,
            (Some((_, built)), Some(changed)) => changed > built,
            (Some(_), None) => false,
        }
    }

    fn installed_ok(&self) -> bool {
        self.installed == Installed::Same && !self.valve_enabled
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
        Some(self.project.as_ref()?.export_root.join("build").join(install::PAK_NAME))
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
        let lang = self.dota.as_ref().and_then(|d| d.language_dir());
        let installed = match (lang.as_ref().and_then(|d| std::fs::metadata(d.join(install::PAK_NAME)).ok()), built) {
            (None, _) => Installed::No,
            (Some(m), Some((len, at))) if m.len() == len && m.modified().is_ok_and(|t| t >= at) => Installed::Same,
            (Some(_), _) => Installed::Other,
        };
        self.build.snapshot = Some(Snapshot {
            mine: mine.len(),
            newest_change: newest_change(&p.export_root, mine.values()),
            built,
            installed,
            valve_enabled: self.dota.as_ref().and_then(|d| d.voice_pak()).is_some_and(|v| v.enabled),
            has_changes: self.dota.as_ref().is_some_and(install::has_changes),
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
        } else if !snap.installed_ok() {
            2
        } else {
            3
        };
        let state = |n: u8| match n.cmp(&current) {
            std::cmp::Ordering::Less => StepState::Done,
            std::cmp::Ordering::Equal if n == 3 => StepState::Done,
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
                step_card(ui, 3, state(3), |ui| {
                    ui.label(theme::title("Запусти Доту", 15.0));
                    let text = if current == 3 {
                        "Готово! Новые фразы подхватятся при старте игры. Если Дота открыта — перезапусти её."
                    } else {
                        "Новые фразы подхватятся при старте игры."
                    };
                    ui.label(theme::small(text));
                });
                ui.add_space(14.0);
                self.build_details(ui);
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
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if snap.installed_ok() {
                        theme::pill(ui, "установлено", GREEN);
                    } else if snap.installed == Installed::Other || (snap.installed == Installed::Same && snap.valve_enabled) {
                        theme::pill(ui, "в игре старая версия", AMBER);
                    } else {
                        theme::pill(ui, "не установлено", MUTED);
                    }
                });
            });
            let Some(dota) = self.dota.clone() else {
                ui.label(theme::small("Дота не найдена в библиотеках Steam."));
                return;
            };
            let lang = dota.audio_language.clone().unwrap_or_else(|| "английская".into());
            ui.label(theme::small(format!("Dota 2 · озвучка {lang}. Перед установкой закрой Доту.")));
            ui.add_space(4.0);

            if self.build.game_task.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(theme::small("Меняю файлы игры…"));
                });
                return;
            }
            match self.build.confirm {
                None => {
                    ui.horizontal(|ui| {
                        let install_btn = theme::primary(format!("{}  Установить", icon::DOWNLOAD_SIMPLE));
                        let can_install = snap.built.is_some() && !snap.installed_ok();
                        if ui.add_enabled(can_install, install_btn).clicked() {
                            self.build.confirm = Some(GameAction::Install);
                            self.build.game_result = None;
                            self.build.dota_running = install::is_dota_running();
                        }
                        let rollback = theme::ghost(ui, snap.has_changes, format!("{}  Откатить", icon::ARROW_COUNTER_CLOCKWISE));
                        if rollback.on_hover_text("Убрать наш пак и вернуть озвучку Valve").clicked() {
                            self.build.confirm = Some(GameAction::Rollback);
                            self.build.game_result = None;
                            self.build.dota_running = install::is_dota_running();
                        }
                    });
                }
                Some(action) => {
                    let steps = match action {
                        GameAction::Install => install::Plan::for_install(&dota).map(|p| p.describe()),
                        GameAction::Rollback => Ok(vec![
                            format!("Удалить {} из папки языка", install::PAK_NAME),
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
                                    let label = if action == GameAction::Install { "Установить" } else { "Откатить" };
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
            }
            result_line(ui, self.build.game_result.as_ref());
        });
    }

    fn build_details(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(theme::small("Подробнее: где лежат файлы")).id_salt("build_details").show(ui, |ui| {
            let mut rows: Vec<(&str, PathBuf)> = Vec::new();
            if let Some(p) = &self.project {
                rows.push(("Твои фразы", p.export_root.join(&p.resource_dir)));
            }
            if let Some(b) = self.built_pak() {
                rows.push(("Собранный пак", b));
            }
            if let Some(d) = &self.dota {
                if let Some(v) = d.voice_pak() {
                    rows.push((if v.enabled { "Озвучка Valve (включена)" } else { "Озвучка Valve (отключена)" }, v.path));
                }
                if let Some(l) = d.language_dir() {
                    rows.push(("Папка модов в игре", l));
                }
            }
            for (label, path) in rows {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(label).size(12.0).color(FAINT));
                    if ui.add(egui::Button::new(RichText::new(path.display().to_string()).size(12.0).color(MUTED)).frame(false)).on_hover_text("Показать в проводнике").clicked() {
                        reveal(&path);
                    }
                });
            }
        });
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
