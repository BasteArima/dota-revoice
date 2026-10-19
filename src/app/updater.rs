//! Окно обновления: «Что нового», скачивание с прогрессом, установка и перезапуск.
//! Сама замена программы — в `update.rs`; здесь фоновый поток и то, что видно.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use dota_revoice::update::{self, Release};
use dota_revoice::{tr, trf};
use eframe::egui::{self, Align, Layout, RichText, Stroke, vec2};

use super::theme::{self, BG_DEEP, GOLD, GREEN, LINE, MUTED, RED, TEXT, icon};
use super::{App, open_url};

/// Сколько ждать перед перезапуском, чтобы «Готово» успели увидеть.
const RESTART_DELAY: Duration = Duration::from_millis(1200);
/// Как часто пробовать удалить прежнюю программу, пока её процесс закрывается.
const CLEANUP_EVERY: Duration = Duration::from_secs(2);
const CLEANUP_TRIES: u32 = 30;

pub(super) struct Updater {
    release: Release,
    stage: Stage,
    rx: Option<mpsc::Receiver<Event>>,
    cancel: Arc<AtomicBool>,
}

enum Stage {
    /// Что нового — до нажатия «Обновить».
    Notes,
    Downloading { done: u64, total: u64, started: Instant },
    Installing,
    Restarting { exe: PathBuf, at: Instant },
    Failed(String),
}

enum Event {
    Progress(u64, u64),
    Downloaded,
    Installed(PathBuf),
    Failed(String),
}

/// Удаление прежней программы после обновления: она могла ещё не закрыться.
#[derive(Default)]
pub(super) struct Cleanup {
    tries: u32,
    at: Option<Instant>,
}

impl Cleanup {
    /// Пробует удалить `.old`, пока не выйдет (или пока не кончатся попытки).
    pub(super) fn tick(&mut self) {
        if self.tries >= CLEANUP_TRIES || self.at.is_some_and(|t| t.elapsed() < CLEANUP_EVERY) {
            return;
        }
        self.at = Some(Instant::now());
        self.tries = if update::remove_old() { CLEANUP_TRIES } else { self.tries + 1 };
    }
}

impl App {
    /// Окно обновления; `start` — сразу качать (кнопка «Обновить»), иначе — «Что нового».
    pub(super) fn open_updater(&mut self, ctx: &egui::Context, release: Release, start: bool) {
        self.updater = Some(Updater { release, stage: Stage::Notes, rx: None, cancel: Arc::new(AtomicBool::new(false)) });
        if start {
            self.start_update(ctx);
        }
    }

    fn start_update(&mut self, ctx: &egui::Context) {
        let Some(u) = self.updater.as_mut() else { return };
        let Some(asset) = update::asset(&u.release).cloned() else { return };
        u.cancel = Arc::new(AtomicBool::new(false));
        u.stage = Stage::Downloading { done: 0, total: asset.size, started: Instant::now() };
        let (tx, rx) = mpsc::channel();
        let (cancel, ctx) = (u.cancel.clone(), ctx.clone());
        std::thread::spawn(move || {
            let progress = |done, total| {
                let _ = tx.send(Event::Progress(done, total));
                ctx.request_repaint();
            };
            let result = update::download(&asset, &cancel, progress).and_then(|program| {
                let _ = tx.send(Event::Downloaded);
                update::install(&program)
            });
            let _ = tx.send(match result {
                Ok(exe) => Event::Installed(exe),
                Err(e) => Event::Failed(format!("{e:#}")),
            });
            ctx.request_repaint();
        });
        u.rx = Some(rx);
    }

    /// Каждый кадр: забрать вести из потока, перезапуститься, когда пора, нарисовать окно.
    pub(super) fn updater_window(&mut self, ctx: &egui::Context) {
        let Some(mut u) = self.updater.take() else { return };
        while let Some(event) = u.rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            u.stage = match (event, u.stage) {
                (Event::Progress(done, total), Stage::Downloading { started, .. }) => Stage::Downloading { done, total, started },
                (Event::Progress(..), stage) => stage,
                (Event::Downloaded, _) => Stage::Installing,
                (Event::Installed(exe), _) => Stage::Restarting { exe, at: Instant::now() },
                (Event::Failed(e), _) => Stage::Failed(e),
            };
        }
        if let Stage::Restarting { exe, at } = &u.stage {
            if at.elapsed() >= RESTART_DELAY {
                // Всё несохранённое — на диск, потом новая версия и выход.
                self.save_note();
                self.config.save();
                match update::relaunch(exe) {
                    Ok(()) => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        return;
                    }
                    Err(e) => u.stage = Stage::Failed(format!("{e:#}")),
                }
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }

        let mut close = false;
        let mut start = false;
        let recording = self.is_recording();
        let current = update::current_version();
        let modal = egui::Modal::new(egui::Id::new("updater")).show(ctx, |ui| {
            ui.set_width(470.0);
            ui.label(theme::title(trf!("Обновление до {}", "Update to {}", u.release.version), 18.0));
            let size = update::asset(&u.release).map(|a| trf!(" · {:.1} МБ", " · {:.1} MB", a.size as f64 / 1_048_576.0)).unwrap_or_default();
            ui.label(theme::small(format!("Dota Revoice {current} → {}{size}", u.release.version)));
            ui.add_space(10.0);
            theme::card(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(tr!("Что нового", "What's new")).strong().color(TEXT));
                if u.release.notes.is_empty() {
                    ui.label(theme::small(tr!("Описание — на странице релиза.", "See the release page for details.")));
                }
                for note in &u.release.notes {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("•").color(GOLD));
                        ui.label(RichText::new(note).size(13.0).color(TEXT));
                    });
                }
            });
            ui.add_space(10.0);
            match &u.stage {
                Stage::Notes => {}
                Stage::Downloading { done, total, started } => {
                    steps(ui, 0);
                    progress_bar(ui, *done as f32 / (*total).max(1) as f32);
                    let secs = started.elapsed().as_secs_f32().max(0.1);
                    let speed = *done as f32 / secs;
                    let left = total.saturating_sub(*done) as f32 / speed.max(1.0);
                    ui.horizontal(|ui| {
                        let mb = |b: u64| b as f64 / 1_048_576.0;
                        ui.label(theme::small(trf!("{:.1} из {:.1} МБ · {:.1} МБ/с", "{:.1} of {:.1} MB · {:.1} MB/s", mb(*done), mb(*total), speed / 1_048_576.0)));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(theme::small(trf!("осталось ~{:.0} с", "~{:.0} s left", left.ceil())));
                        });
                    });
                }
                Stage::Installing => {
                    steps(ui, 1);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(theme::small(tr!("Сверяю с релизом и ставлю…", "Checking against the release and installing…")));
                    });
                }
                Stage::Restarting { .. } => {
                    steps(ui, 2);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon::CHECK_CIRCLE).color(GREEN));
                        ui.label(RichText::new(tr!("Готово — перезапускаю…", "Done — restarting…")).color(TEXT));
                    });
                }
                Stage::Failed(e) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(icon::WARNING).color(RED));
                        ui.label(RichText::new(trf!("Не обновилось: {e}", "Update failed: {e}")).size(13.0).color(RED));
                    });
                    ui.label(theme::small(tr!(
                        "Программа осталась прежней. Можно попробовать ещё раз или скачать со страницы релиза.",
                        "The app is unchanged. Try again or download it from the release page."
                    )));
                }
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| match &u.stage {
                    Stage::Notes | Stage::Failed(_) => {
                        if u.release.installable {
                            let label = if matches!(u.stage, Stage::Failed(_)) { tr!("Ещё раз", "Try again") } else { tr!("Обновить", "Update") };
                            let button = ui.add_enabled(!recording, theme::primary(format!("{}  {label}", icon::ARROWS_CLOCKWISE)));
                            if button.on_disabled_hover_text(tr!("Сначала закончи запись", "Finish recording first")).clicked() {
                                start = true;
                            }
                        }
                        if theme::ghost(ui, true, trf!("{}  Страница релиза", "{}  Release page", icon::ARROW_SQUARE_OUT)).clicked() {
                            open_url(&u.release.url);
                        }
                        if theme::ghost(ui, true, tr!("Закрыть", "Close")).clicked() {
                            close = true;
                        }
                    }
                    Stage::Downloading { .. } => {
                        if theme::ghost(ui, true, tr!("Отмена", "Cancel")).clicked() {
                            u.cancel.store(true, Ordering::Relaxed);
                        }
                    }
                    Stage::Installing | Stage::Restarting { .. } => {}
                });
            });
        });
        // Пока качается и ставится, окно не закрывается мимо кнопок.
        let busy = matches!(u.stage, Stage::Downloading { .. } | Stage::Installing | Stage::Restarting { .. });
        if busy {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if start {
            self.updater = Some(u);
            self.start_update(ctx);
            return;
        }
        if !(close || (!busy && modal.should_close())) {
            self.updater = Some(u);
        }
    }
}

/// Три шага: скачать → проверить → перезапустить; `now` — текущий.
fn steps(ui: &mut egui::Ui, now: usize) {
    ui.horizontal(|ui| {
        let w = (ui.available_width() - 2.0 * ui.spacing().item_spacing.x) / 3.0;
        for (i, label) in [tr!("Скачать", "Download"), tr!("Проверить", "Verify"), tr!("Перезапустить", "Restart")].into_iter().enumerate() {
            let (text, color, border) = match i.cmp(&now) {
                std::cmp::Ordering::Less => (format!("{}  {label}", icon::CHECK), GREEN, GREEN.gamma_multiply(0.4)),
                std::cmp::Ordering::Equal => (format!("●  {label}"), GOLD, GOLD.gamma_multiply(0.4)),
                std::cmp::Ordering::Greater => (label.to_owned(), MUTED, LINE),
            };
            egui::Frame::default().fill(BG_DEEP).stroke(Stroke::new(1.0_f32, border)).corner_radius(8).inner_margin(egui::Margin::symmetric(8, 5)).show(ui, |ui| {
                ui.set_width(w - 16.0);
                ui.label(RichText::new(text).size(12.0).color(color));
            });
        }
    });
    ui.add_space(6.0);
}

fn progress_bar(ui: &mut egui::Ui, fraction: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 4.0, BG_DEEP);
    let mut fill = rect;
    fill.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    p.rect_filled(fill, 4.0, GOLD);
    ui.add_space(4.0);
}
