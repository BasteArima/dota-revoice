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
use dota_revoice::{tr, trf};

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
    /// Не по порядку, но пользоваться уже можно (переключатель мода, пока пак пересобирается).
    Ready,
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
                    let mut text = trf!(
                        "Собрано: твоих фраз {}, из озвучки игры {}, {:.0} МБ", "Built: {} of your lines, {} from the game's voice-over, {:.0} MB",
                        s.replaced + s.not_in_base.len(),
                        s.copied,
                        s.size_bytes as f64 / 1_048_576.0
                    );
                    if !s.not_in_base.is_empty() {
                        // Обычно это герои без озвучки на этом языке: их фразы лягут поверх английских.
                        let first: Vec<&str> = s.not_in_base.iter().take(3).map(String::as_str).collect();
                        let more = if s.not_in_base.len() > 3 { " …" } else { "" };
                        text += &trf!(
                            ". Не из этой озвучки: {} ({}{more}) — у таких героев твои фразы заменят английские", ". Not from this voice-over: {} ({}{more}) — for these heroes your lines will replace the English ones",
                            s.not_in_base.len(),
                            first.join(", ")
                        );
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
                    .map(|()| tr!("Установлено. Запусти Доту — новые фразы подхватятся при старте", "Installed. Launch Dota — new lines load at startup").to_owned()),
                GameAction::Enable => install::set_enabled(&dota, true)
                    .map(|_| tr!("Мод включён — перезапусти Доту, чтобы услышать свои фразы", "Mod on — restart Dota to hear your lines").to_owned()),
                GameAction::Disable => install::set_enabled(&dota, false)
                    .map(|_| tr!("Мод выключен — в игре озвучка Valve. Включить обратно можно в любой момент", "Mod off — the game uses Valve's voice-over. You can turn it back on anytime").to_owned()),
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
                ui.label(theme::title(tr!("Сборка и установка", "Build & install"), 22.0));
                ui.label(theme::small(tr!("Три шага — и твой голос в игре", "Three steps and your voice is in the game")));
                ui.add_space(14.0);

                self.step_build(ui, &ctx, &snap, state(1));
                ui.add_space(10.0);
                // Мод уже в игре — переключатель и «Убрать из игры» доступны без пересборки.
                let install_state = match state(2) {
                    StepState::Later if snap.mod_state != ModState::Absent => StepState::Ready,
                    s => s,
                };
                self.step_install(ui, &ctx, &snap, install_state);
                ui.add_space(10.0);
                // Запустить Доту можно в любой момент — шаг не тускнеет, даже если до него не дошли.
                let launch_state = match state(3) {
                    StepState::Later => StepState::Ready,
                    s => s,
                };
                self.step_launch(ui, &snap, current, launch_state);
                ui.add_space(22.0);
                self.exchange_card(ui);
            });
        });
        self.build.snapshot = Some(snap);
    }

    fn step_build(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, snap: &Snapshot, state: StepState) {
        step_card(ui, 1, state, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::title(tr!("Собрать пак", "Build the pack"), 15.0));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| match (snap.built.is_some(), snap.needs_rebuild()) {
                    (true, false) => theme::pill(ui, tr!("собран", "built"), GREEN),
                    (true, true) if snap.valve_newer => theme::pill(ui, tr!("озвучка игры обновилась", "game voice-over updated"), AMBER),
                    (true, true) => theme::pill(ui, tr!("есть новые записи", "new recordings"), AMBER),
                    _ => theme::pill(ui, tr!("не собран", "not built"), MUTED),
                });
            });
            let mut info = if snap.mine == 0 {
                tr!("Пока нет записанных фраз — запиши хотя бы одну на вкладке «Запись».", "No recorded lines yet — record at least one on the Record tab.").to_owned()
            } else {
                trf!("Твоих фраз: {}", "Your lines: {}", snap.mine)
            };
            if let Some((size, at)) = snap.built {
                info += &trf!(" · пак {:.0} МБ, собран {}", " · pack {:.0} MB, built {}", size as f64 / 1_048_576.0, ago(at));
            }
            ui.label(theme::small(info));
            if snap.valve_newer {
                ui.label(RichText::new(tr!("Steam обновил озвучку Valve после сборки — пересобери, чтобы в паке были свежие фразы игры.", "Steam updated Valve's voice-over after the build — rebuild to get the game's fresh lines into the pack.")).size(12.0).color(AMBER));
            }

            if self.dota.as_ref().and_then(|d| d.voice_pak()).is_some() {
                let text = trf!(
                    "{}  Остальные фразы взять из озвучки игры", "{}  Take the remaining lines from the game's voice-over",
                    if self.build.use_base { icon::CHECK_SQUARE } else { icon::SQUARE }
                );
                let color = if self.build.use_base { TEXT } else { MUTED };
                if ui.add(egui::Button::new(RichText::new(text).size(12.0).color(color)).frame(false)).clicked() {
                    self.build.use_base = !self.build.use_base;
                }
                if !self.build.use_base {
                    ui.label(RichText::new(tr!("Без этого все незаписанные фразы в игре станут английскими", "Without this, every unrecorded line in the game becomes English")).size(12.0).color(AMBER));
                }
            }
            ui.add_space(4.0);
            if let Some(job) = &self.build.job {
                let frac = job.progress.load(Ordering::Relaxed) as f32 / 1000.0;
                ui.add(egui::ProgressBar::new(frac).fill(GOLD).text(RichText::new(tr!("Собираю…", "Building…")).color(ON_GOLD)).desired_height(22.0));
            } else {
                let label = format!("{}  {}", icon::PACKAGE, if snap.built.is_some() { tr!("Пересобрать", "Rebuild") } else { tr!("Собрать пак", "Build the pack") });
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
                ui.label(theme::title(tr!("Установить в игру", "Install into the game"), 15.0));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| match snap.mod_state {
                    ModState::Absent => theme::pill(ui, tr!("не установлено", "not installed"), MUTED),
                    ModState::Disabled => theme::pill(ui, tr!("мод выключен", "mod off"), MUTED),
                    ModState::Enabled if snap.valve_enabled => theme::pill(ui, tr!("Steam вернул озвучку Valve", "Steam restored Valve's voice-over"), AMBER),
                    ModState::Enabled if snap.installed == Installed::Other => theme::pill(ui, tr!("в игре старая версия", "old version in the game"), AMBER),
                    ModState::Enabled => theme::pill(ui, tr!("мод включён", "mod on"), GREEN),
                });
            });
            let Some(dota) = self.dota.clone() else {
                ui.label(theme::small(tr!("Дота не найдена в библиотеках Steam — укажи её папку в настройках.", "Dota wasn't found in Steam libraries — set its folder in the settings.")));
                return;
            };
            let lang = game::language_name(dota.voice_language.as_deref());
            ui.label(theme::small(trf!("Dota 2 · озвучка {lang}. Перед изменениями закрой Доту.", "Dota 2 · {lang} voice-over. Close Dota before making changes.")));
            ui.add_space(4.0);

            if self.build.game_task.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(theme::small(tr!("Меняю файлы игры…", "Changing game files…")));
                });
                return;
            }
            match self.build.confirm {
                None if snap.mod_state == ModState::Absent => {
                    let can_install = snap.built.is_some();
                    if ui.add_enabled(can_install, theme::primary(trf!("{}  Установить", "{}  Install", icon::DOWNLOAD_SIMPLE))).clicked() {
                        self.ask(GameAction::Install);
                    }
                    if snap.has_changes {
                        // Нашего пака нет, но пак Valve отключён — например, его удалили руками.
                        if theme::ghost(ui, true, trf!("{}  Вернуть озвучку Valve", "{}  Restore Valve's voice-over", icon::ARROW_COUNTER_CLOCKWISE)).clicked() {
                            self.ask(GameAction::Rollback);
                        }
                    }
                }
                None => {
                    if let Some(action) = mod_switch(ui, snap.mine_audible()) {
                        self.ask(action);
                    }
                    let text = if snap.mine_audible() {
                        tr!("Слышны твои фразы, остальное — оригинал.", "Your lines play, everything else is original.")
                    } else if snap.mod_state == ModState::Disabled {
                        tr!("Пак лежит в игре выключенным — включится мгновенно, без пересборки.", "The pack sits in the game turned off — it turns on instantly, no rebuild needed.")
                    } else {
                        tr!("Пак Valve снова включён и перекрывает твой — включи мод обратно.", "Valve's pack is on again and overrides yours — turn the mod back on.")
                    };
                    ui.label(theme::small(text));
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        // Свежая сборка ещё не в игре. Пока пак надо пересобрать — сначала шаг 1.
                        let stale = snap.built.is_some() && snap.installed != Installed::Same && !snap.needs_rebuild();
                        if stale && ui.add(theme::primary(trf!("{}  Обновить пак", "{}  Update pack", icon::ARROWS_CLOCKWISE))).on_hover_text(tr!("Положить в игру свежую сборку", "Put the fresh build into the game")).clicked() {
                            self.ask(GameAction::Install);
                        }
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            let remove = theme::ghost(ui, true, trf!("{}  Убрать из игры", "{}  Remove from game", icon::TRASH));
                            if remove.on_hover_text(tr!("Удалить наш пак из игры совсем и вернуть озвучку Valve", "Delete our pack from the game and restore Valve's voice-over")).clicked() {
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
                    steps.push(trf!("Переименовать {} в {} — включить мой пак", "Rename {} to {} — turn my pack on", install::PAK_OFF, install::PAK_NAME));
                }
                if valve_on {
                    steps.push(tr!("Переименовать pak01_dir.vpk в pak01_dir.vpk.off — отключить озвучку Valve", "Rename pak01_dir.vpk to pak01_dir.vpk.off — turn off Valve's voice-over").to_owned());
                }
                Ok(steps)
            }
            GameAction::Disable => Ok(vec![
                trf!("Переименовать {} в {} — мой пак остаётся в игре, но не грузится", "Rename {} to {} — my pack stays in the game but doesn't load", install::PAK_NAME, install::PAK_OFF),
                tr!("Вернуть pak01_dir.vpk — озвучку Valve", "Restore pak01_dir.vpk — Valve's voice-over").to_owned(),
            ]),
            GameAction::Rollback => Ok(vec![
                trf!("Удалить наш пак ({}) из папки языка", "Delete our pack ({}) from the language folder", install::PAK_NAME),
                tr!("Вернуть pak01_dir.vpk — озвучку Valve", "Restore pak01_dir.vpk — Valve's voice-over").to_owned(),
            ]),
        };
        egui::Frame::default().fill(BG_DEEP).corner_radius(8).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            match steps {
                Ok(steps) => {
                    ui.label(RichText::new(tr!("Что изменится в папке игры:", "What changes in the game folder:")).size(12.0).color(TEXT));
                    for step in steps {
                        ui.label(theme::small(format!("•  {step}")));
                    }
                    if self.build.dota_running {
                        ui.label(RichText::new(trf!("{}  Дота запущена — закрой её, иначе файлы заняты", "{}  Dota is running — close it, otherwise the files are locked", icon::WARNING)).size(12.0).color(RED));
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let label = match action {
                            GameAction::Install => tr!("Установить", "Install"),
                            GameAction::Enable => tr!("Включить", "Turn on"),
                            GameAction::Disable => tr!("Выключить", "Turn off"),
                            GameAction::Rollback => tr!("Убрать из игры", "Remove from game"),
                        };
                        if ui.add(theme::primary(label)).clicked() {
                            self.start_game_action(ctx, action);
                        }
                        if theme::ghost(ui, true, tr!("Отмена", "Cancel")).clicked() {
                            self.build.confirm = None;
                        }
                    });
                }
                Err(e) => {
                    ui.label(RichText::new(format!("{e:#}")).size(12.0).color(RED));
                    if theme::ghost(ui, true, tr!("Закрыть", "Close")).clicked() {
                        self.build.confirm = None;
                    }
                }
            }
        });
    }

    fn step_launch(&mut self, ui: &mut egui::Ui, snap: &Snapshot, current: u8, state: StepState) {
        step_card(ui, 3, state, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::title(tr!("Запустить Доту", "Launch Dota"), 15.0));
                if snap.dota_running {
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| theme::pill(ui, tr!("Дота запущена", "Dota is running"), GREEN));
                }
            });
            let (text, color) = match (current, snap.dota_running) {
                _ if snap.mod_state == ModState::Disabled => (tr!("Мод выключен — в игре будет озвучка Valve. Включи его на шаге 2, чтобы слышать свои фразы.", "Mod is off — the game will use Valve's voice-over. Turn it on in step 2 to hear your lines."), MUTED),
                (3, true) => (tr!("Дота уже открыта. Игра читает паки только при старте — закрой её и запусти заново, чтобы услышать новые фразы.", "Dota is already open. The game reads packs only at startup — close it and launch again to hear new lines."), AMBER),
                (3, false) => (tr!("Всё готово — запускай. Новые фразы подхватятся при старте игры.", "All set — launch it. New lines load when the game starts."), MUTED),
                _ => (tr!("Сначала собери и установи пак — иначе в игре будет прежняя озвучка.", "Build and install the pack first — otherwise the game keeps the old voice-over."), MUTED),
            };
            ui.label(RichText::new(text).size(12.0).color(color));
            ui.add_space(4.0);
            let label = trf!("{}  Запустить Доту", "{}  Launch Dota", icon::PLAY);
            let btn = if current == 3 && !snap.dota_running { theme::primary(label) } else { egui::Button::new(label) };
            if ui.add_enabled(self.dota.is_some(), btn).on_hover_text(tr!("Через Steam, как обычный ярлык игры", "Through Steam, like the regular game shortcut")).clicked() {
                match install::launch_dota() {
                    Ok(()) => self.info(tr!("Запускаю Доту через Steam…", "Launching Dota through Steam…")),
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
                ui.label(theme::title(tr!("Обмен с друзьями", "Share with friends"), 15.0));
            });
            ui.label(theme::small(
                tr!("Собери все свои фразы в один файл и отправь другу — или добавь фразы, которые прислали тебе. Потом пересобери и установи пак.", "Pack all your lines into one file and send it to a friend — or add lines someone sent you. Then rebuild and install the pack."),
            ));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.add(theme::tool(trf!("{}  Экспорт пачки…", "{}  Export bundle…", icon::EXPORT))).on_hover_text(tr!("Все записанные фразы всех героев + заметки, в zip", "All recorded lines of all heroes + notes, in a zip")).clicked() {
                    self.export_pack();
                }
                if ui.add(theme::tool(trf!("{}  Импорт пачки…", "{}  Import bundle…", icon::DOWNLOAD_SIMPLE))).on_hover_text(tr!("zip, который прислал друг", "A zip a friend sent you")).clicked() {
                    self.import_pack();
                }
                if theme::chip(ui, self.build.import_replace, tr!("заменять мои версии", "replace my versions")).on_hover_text(tr!("Твоя версия уйдёт в дубли — её можно вернуть", "Your version goes to takes — you can bring it back")).clicked() {
                    self.build.import_replace = !self.build.import_replace;
                }
            });
            result_line(ui, self.build.exchange_result.as_ref());
        });
    }

    fn export_pack(&mut self) {
        let Some(p) = &self.project else { return };
        let Some(out) = rfd::FileDialog::new()
            .set_title(tr!("Куда сохранить пачку фраз", "Where to save the line bundle"))
            .set_file_name(tr!("dota_revoice_фразы.zip", "dota_revoice_lines.zip"))
            .add_filter(tr!("Пачка фраз", "Line bundle"), &["zip"])
            .save_file()
        else {
            return;
        };
        self.build.exchange_result = Some(match exchange::export_pack(&p.export_root, &out) {
            Ok(n) => Ok(trf!("В пачке {n} фраз: {}", "{n} lines in the bundle: {}", out.display())),
            Err(e) => Err(format!("{e:#}")),
        });
    }

    fn import_pack(&mut self) {
        let Some(root) = self.project.as_ref().map(|p| p.export_root.clone()) else { return };
        let Some(file) = rfd::FileDialog::new().set_title(tr!("Пачка фраз от друга", "Line bundle from a friend")).add_filter(tr!("Пачка фраз", "Line bundle"), &["zip"]).pick_file() else {
            return;
        };
        let result = exchange::import_pack(&root, &file, self.build.import_replace);
        self.build.exchange_result = Some(match result {
            Ok(s) => {
                let mut parts = vec![trf!("Добавлено {}", "Added {}", s.added)];
                if s.replaced > 0 {
                    parts.push(trf!("заменено {} (твои версии — в дублях)", "replaced {} (your versions are in takes)", s.replaced));
                }
                if !s.kept.is_empty() {
                    parts.push(trf!("оставлены твои: {}", "kept yours: {}", s.kept.len()));
                }
                if !s.rejected.is_empty() {
                    parts.push(trf!("пропущены лишние файлы: {}", "skipped extra files: {}", s.rejected.len()));
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
                StepState::Ready => {
                    p.circle_stroke(c, 12.5, Stroke::new(1.0_f32, MUTED));
                    p.text(c, Align2::CENTER_CENTER, n.to_string(), font, TEXT);
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
                (true, trf!("{}  Моя озвучка", "{}  My voice-over", icon::MICROPHONE), GOLD, GameAction::Enable),
                (false, tr!("Озвучка Valve", "Valve voice-over").to_owned(), TEXT, GameAction::Disable),
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
        0..60 => tr!("только что", "just now").into(),
        60..3600 => trf!("{} мин назад", "{} min ago", secs / 60),
        3600..86400 => trf!("{} ч назад", "{} h ago", secs / 3600),
        _ => trf!("{} дн. назад", "{} d ago", secs / 86400),
    }
}
