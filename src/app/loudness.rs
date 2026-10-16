//! «Подогнать громкость всех фраз героя»: каждая записанная фраза — как её оригинал.
//! Считается в фоне; в проект (обработка в `dota_revoice.json`) пишем по готовности.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

use dota_revoice::audio;
use dota_revoice::encode;
use dota_revoice::process::{self, Settings};
use dota_revoice::project::Line;
use dota_revoice::{tr, trf};
use eframe::egui::{self, RichText};

use super::theme::{self, AMBER, icon};
use super::{App, secs_to_index};

/// Что сейчас с подгонкой: ждём «да» или считаем.
pub(super) enum MatchAll {
    Ask(usize),
    Running(Job),
}

impl MatchAll {
    pub(super) fn running(&self) -> bool {
        matches!(self, MatchAll::Running(_))
    }
}

pub(super) struct Job {
    done: Arc<AtomicUsize>,
    total: usize,
    rx: mpsc::Receiver<Outcome>,
}

struct Task {
    line: Line,
    mp3: PathBuf,
    raw: PathBuf,
    fx: Settings,
    trim: Option<(f32, f32)>,
}

struct Outcome {
    changed: Vec<(Line, Settings)>,
    skipped: usize,
    error: Option<String>,
}

impl App {
    /// Фразы героя, у которых есть моя версия.
    fn recorded_lines(&self) -> Vec<Line> {
        let Some(p) = &self.project else { return Vec::new() };
        p.lines.iter().filter(|l| p.export_mp3(l).is_file()).cloned().collect()
    }

    fn start_match_all(&mut self, ctx: &egui::Context) {
        let Some(p) = &self.project else { return };
        let tasks: Vec<Task> = self
            .recorded_lines()
            .into_iter()
            .map(|line| Task { mp3: p.export_mp3(&line), raw: p.raw_wav(&line), fx: p.processing(&line), trim: p.trim(&line), line })
            .collect();
        let done = Arc::new(AtomicUsize::new(0));
        let total = tasks.len();
        let (tx, rx) = mpsc::channel();
        let (done2, ctx) = (done.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut out = Outcome { changed: Vec::new(), skipped: 0, error: None };
            for t in tasks {
                match match_one(&t) {
                    Ok(Some(fx)) => out.changed.push((t.line, fx)),
                    Ok(None) => out.skipped += 1,
                    Err(e) => {
                        out.skipped += 1;
                        out.error.get_or_insert_with(|| format!("{}: {e:#}", t.line.name));
                    }
                }
                done2.fetch_add(1, Ordering::Relaxed);
                ctx.request_repaint();
            }
            let _ = tx.send(out);
            ctx.request_repaint();
        });
        if let Some(pl) = &self.player {
            pl.stop();
        }
        self.match_all = Some(MatchAll::Running(Job { done, total, rx }));
    }

    /// Каждый кадр: забрать результат фоновой подгонки.
    pub(super) fn poll_match_all(&mut self) {
        let Some(MatchAll::Running(job)) = &self.match_all else { return };
        let Ok(out) = job.rx.try_recv() else { return };
        self.match_all = None;
        let current = self.current_line();
        let mut error = out.error;
        if let Some(p) = self.project.as_mut() {
            for (line, fx) in &out.changed {
                if let Err(e) = p.set_processing(line, *fx) {
                    error.get_or_insert_with(|| format!("{}: {e:#}", line.name));
                }
            }
        }
        if let Some(line) = current
            && out.changed.iter().any(|(l, _)| l.original == line.original)
        {
            self.reload_mine(&line);
        }
        let mut text = trf!("Громкость подогнана под оригинал, фраз: {}", "Loudness matched to the original, lines: {}", out.changed.len());
        if out.skipped > 0 {
            text += &trf!(" · не получилось: {} (слишком тихо или коротко)", " · failed: {} (too quiet or too short)", out.skipped);
        }
        match error {
            Some(e) => self.error(format!("{text} · {e}")),
            None => self.info(text),
        }
        self.build.refresh_now();
    }

    /// Кнопка «для всех фраз героя» — с подтверждением, потом ход подгонки.
    pub(super) fn match_all_controls(&mut self, ui: &mut egui::Ui) {
        match &self.match_all {
            None => {
                let btn = theme::ghost(ui, true, trf!("{}  Для всех фраз героя…", "{}  For all of the hero's lines…", icon::USERS_THREE));
                if btn.on_hover_text(tr!("Подогнать громкость каждой записанной фразы под её оригинал", "Match every recorded line's loudness to its original")).clicked() {
                    self.match_all = Some(MatchAll::Ask(self.recorded_lines().len()));
                }
            }
            Some(MatchAll::Ask(n)) => {
                let n = *n;
                ui.label(RichText::new(trf!("Подогнать все фразы героя ({n})?", "Match all of the hero's lines ({n})?")).size(12.0).color(AMBER));
                if ui.add_enabled(n > 0, theme::primary(tr!("Да", "Yes"))).clicked() {
                    let ctx = ui.ctx().clone();
                    self.start_match_all(&ctx);
                }
                if theme::ghost(ui, true, tr!("Отмена", "Cancel")).clicked() {
                    self.match_all = None;
                }
            }
            Some(MatchAll::Running(job)) => {
                ui.spinner();
                let (done, total) = (job.done.load(Ordering::Relaxed), job.total);
                ui.label(theme::small(trf!("подгоняю {done} из {total}…", "matching {done} of {total}…")));
            }
        }
    }
}

/// Подгоняет одну фразу: новая громкость и пересобранный mp3. `None` — не измерить.
fn match_one(t: &Task) -> anyhow::Result<Option<Settings>> {
    let original = audio::decode_file(&t.line.original)?;
    // Без исходника (фраза из пачки друга) источником служит сам mp3 — как в редакторе.
    let (raw, mut fx, trim) = if t.raw.is_file() {
        (audio::decode_file(&t.raw)?, t.fx, t.trim)
    } else {
        let samples = audio::decode_file(&t.mp3)?;
        encode::save_raw(&t.raw, &samples)?;
        (samples, Settings::default(), None)
    };
    let len = raw.len();
    let range = trim
        .map(|(a, b)| (secs_to_index(a, len), secs_to_index(b, len)))
        .filter(|(a, b)| b > a)
        .unwrap_or((0, len));
    let shaped = process::shape(&raw, fx);
    let Some(gain) = process::match_gain_shaped(&original, &shaped, range) else { return Ok(None) };
    fx.gain_db = gain;
    encode::write_mp3(&t.mp3, &process::cut(&process::amplify(&shaped, gain), range))?;
    Ok(Some(fx))
}
