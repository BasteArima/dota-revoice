//! Окно программы: список фраз слева, оригинал и моя версия справа.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use dota_revoice::audio::{self, Clip, Player, Recorder, Track};
use dota_revoice::game::{self, DotaInstall};
use dota_revoice::project::{self, Line, Project, Status};
use dota_revoice::{encode, pack};
use eframe::egui::{self, Align, Align2, Color32, FontId, Key, Modifiers, RichText, Sense, Stroke, pos2, vec2};

use crate::config::Config;

const MIN_TAKE_SECS: f32 = 0.2;
const COMPARE_GAP_SECS: f32 = 0.4;

const GREEN: Color32 = Color32::from_rgb(76, 175, 80);
const AMBER: Color32 = Color32::from_rgb(230, 160, 40);
const RED: Color32 = Color32::from_rgb(220, 70, 60);
const ORIGINAL_WAVE: Color32 = Color32::from_rgb(140, 140, 150);
const MINE_WAVE: Color32 = Color32::from_rgb(70, 140, 230);
const ACCENT: Color32 = Color32::from_rgb(52, 120, 220);

enum Message {
    Info(String),
    Error(String),
}

struct PackJob {
    progress: Arc<AtomicU32>,
    rx: mpsc::Receiver<anyhow::Result<pack::Summary>>,
    out: PathBuf,
}

#[derive(Default)]
struct PackDialog {
    open: bool,
    use_base: bool,
    job: Option<PackJob>,
    result: Option<Result<(String, PathBuf), String>>,
    /// Сколько моих фраз в экспорте — считается при открытии окна.
    mine_count: usize,
}

pub struct App {
    config: Config,
    dota: Option<DotaInstall>,
    project: Option<Project>,
    /// Категории в порядке показа: (категория, индексы фраз).
    groups: Vec<(String, Vec<usize>)>,
    selected: usize,
    search: String,
    /// Сколько кадров ещё докручивать список до выбранной фразы:
    /// в первом кадре группы только раскрываются и позиции неточные.
    scroll_to_selected: u8,

    original: Option<Clip>,
    mine: Option<Clip>,
    player: Option<Player>,
    recorder: Option<Recorder>,
    input_devices: Vec<String>,
    level: f32,
    clipped: bool,

    message: Option<Message>,
    pack: PackDialog,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_style(&cc.egui_ctx);
        let config = Config::load();
        let mut app = Self {
            dota: game::find_dota(),
            project: None,
            groups: Vec::new(),
            selected: 0,
            search: String::new(),
            scroll_to_selected: 0,
            original: None,
            mine: None,
            player: None,
            recorder: None,
            input_devices: Recorder::input_device_names(),
            level: 0.0,
            clipped: false,
            message: None,
            pack: PackDialog { use_base: true, ..Default::default() },
            config,
        };
        match Player::new() {
            Ok(p) => app.player = Some(p),
            Err(e) => app.error(format!("Звук не выводится: {e:#}")),
        }
        app.open_recorder(app.config.input_device.clone());
        if let Some(folder) = app.config.last_folder.clone().filter(|f| f.is_dir()) {
            app.open_project(&folder);
        }
        app
    }

    fn info(&mut self, text: impl Into<String>) {
        self.message = Some(Message::Info(text.into()));
    }

    fn error(&mut self, text: impl Into<String>) {
        self.message = Some(Message::Error(text.into()));
    }

    fn open_recorder(&mut self, name: Option<String>) {
        self.recorder = None;
        match Recorder::new(name.as_deref()) {
            Ok(r) => {
                self.config.input_device = name;
                self.recorder = Some(r);
            }
            Err(e) => self.error(format!("Микрофон не открылся: {e:#}")),
        }
    }

    fn open_project(&mut self, folder: &Path) {
        match Project::open(folder) {
            Ok(p) => {
                let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
                for (i, line) in p.lines.iter().enumerate() {
                    match groups.iter_mut().find(|(c, _)| *c == line.category) {
                        Some((_, idx)) => idx.push(i),
                        None => groups.push((line.category.clone(), vec![i])),
                    }
                }
                self.info(format!(
                    "Открыт {}: {} фраз, записано {}. Свои фразы сохраняются в {}",
                    p.hero,
                    p.lines.len(),
                    p.recorded_count(),
                    p.export_root.join(&p.resource_dir).display()
                ));
                self.groups = groups;
                self.project = Some(p);
                self.config.last_folder = Some(folder.to_path_buf());
                self.config.save();
                let first = self.visible_order().first().copied().unwrap_or(0);
                self.select(first);
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    fn current_line(&self) -> Option<Line> {
        self.project.as_ref()?.lines.get(self.selected).cloned()
    }

    fn select(&mut self, index: usize) {
        if let Some(r) = &self.recorder
            && r.is_recording()
        {
            r.cancel();
        }
        if let Some(p) = &self.player {
            p.stop();
        }
        self.selected = index;
        self.scroll_to_selected = 3;
        self.clipped = false;
        self.original = None;
        self.mine = None;
        let Some(line) = self.current_line() else { return };
        match audio::decode_file(&line.original) {
            Ok(s) => self.original = Some(Clip::new(s)),
            Err(e) => self.error(format!("Оригинал не читается: {e:#}")),
        }
        self.reload_mine(&line);
    }

    fn reload_mine(&mut self, line: &Line) {
        let Some(p) = &self.project else { return };
        let path = p.export_mp3(line);
        self.mine = None;
        if path.is_file() {
            match audio::decode_file(&path) {
                Ok(s) => self.mine = Some(Clip::new(s)),
                Err(e) => self.error(format!("Моя версия не читается: {e:#}")),
            }
        }
    }

    /// Фразы в порядке списка с учётом поиска.
    fn visible_order(&self) -> Vec<usize> {
        let Some(p) = &self.project else { return Vec::new() };
        let q = self.search.trim().to_lowercase();
        let q = q.as_str();
        self.groups
            .iter()
            .flat_map(|(cat, idx)| {
                let label = project::category_label(cat).to_lowercase();
                idx.iter().copied().filter(move |&i| q.is_empty() || p.lines[i].name.contains(q) || label.contains(q))
            })
            .collect()
    }

    fn step(&mut self, delta: isize) {
        let order = self.visible_order();
        if order.is_empty() {
            return;
        }
        let pos = order.iter().position(|&i| i == self.selected).map(|p| p as isize).unwrap_or(-1);
        let next = (pos + delta).clamp(0, order.len() as isize - 1) as usize;
        self.select(order[next]);
    }

    fn next_unrecorded(&mut self) {
        let Some(p) = &self.project else { return };
        let order = self.visible_order();
        let start = order.iter().position(|&i| i == self.selected).map(|x| x + 1).unwrap_or(0);
        let found = order[start..]
            .iter()
            .chain(&order[..start])
            .copied()
            .find(|&i| p.status(&p.lines[i]) != Status::Recorded);
        match found {
            Some(i) => self.select(i),
            None => self.info("Все фразы в списке записаны"),
        }
    }

    fn toggle_play(&mut self, track: Track) {
        let Some(player) = &self.player else { return };
        if player.is_playing(track) {
            player.stop();
            return;
        }
        let clip = match track {
            Track::Original => self.original.as_ref().map(|c| c.samples.clone()),
            Track::Mine => self.mine.as_ref().map(|c| c.samples.clone()),
            Track::Compare => {
                let (Some(o), Some(m)) = (&self.original, &self.mine) else { return };
                let gap = (COMPARE_GAP_SECS * audio::RATE as f32) as usize;
                let mut both = Vec::with_capacity(o.samples.len() + gap + m.samples.len());
                both.extend_from_slice(&o.samples);
                both.resize(both.len() + gap, 0.0);
                both.extend_from_slice(&m.samples);
                Some(Arc::new(both))
            }
        };
        if let Some(c) = clip {
            player.play(track, c);
        }
    }

    fn toggle_record(&mut self) {
        let Some(rec) = &self.recorder else {
            self.error("Микрофон не открыт — выбери его вверху");
            return;
        };
        if !rec.is_recording() {
            if let Some(p) = &self.player {
                p.stop();
            }
            self.clipped = false;
            rec.start();
            return;
        }
        match rec.stop() {
            Ok(samples) => self.store_take(samples, "Записано"),
            Err(e) => self.error(format!("Запись не сохранилась: {e:#}")),
        }
    }

    fn import_file(&mut self) {
        let Some(file) = rfd::FileDialog::new()
            .set_title("Выбери файл с фразой")
            .add_filter("Аудио", &project::AUDIO_EXTENSIONS)
            .pick_file()
        else {
            return;
        };
        match audio::decode_file(&file) {
            Ok(samples) => self.store_take(samples, "Импортировано"),
            Err(e) => self.error(format!("Файл не читается: {e:#}")),
        }
    }

    fn store_take(&mut self, samples: Vec<f32>, verb: &str) {
        let secs = samples.len() as f32 / audio::RATE as f32;
        if secs < MIN_TAKE_SECS {
            self.error("Слишком короткая запись — ничего не сохранено");
            return;
        }
        let (Some(line), Some(p)) = (self.current_line(), self.project.as_mut()) else { return };
        let mp3 = p.export_mp3(&line);
        let result = encode::save_take(&mp3, &p.raw_wav(&line), &samples).and_then(|_| p.mark_recorded(&line));
        match result {
            Ok(()) => {
                self.info(format!("{verb}: {} ({secs:.1} с)", mp3.display()));
                self.reload_mine(&line);
            }
            Err(e) => self.error(format!("Не сохранилось: {e:#}")),
        }
    }

    fn trash_current(&mut self) {
        let (Some(line), Some(p)) = (self.current_line(), self.project.as_mut()) else { return };
        if p.status(&line) == Status::Empty {
            return;
        }
        if let Some(pl) = &self.player {
            pl.stop();
        }
        let trash_dir = p.export_root.join(".trash");
        match p.trash(&line) {
            Ok(()) => {
                self.mine = None;
                self.info(format!("{} перенесена в корзину: {}", line.name, trash_dir.display()));
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if self.project.is_none() || ctx.wants_keyboard_input() {
            return;
        }
        // Забираем клавиши до виджетов, чтобы пробел не «нажимал» кнопку в фокусе.
        let pressed = |key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key));
        if pressed(Key::Space) {
            self.toggle_play(Track::Original);
        }
        if pressed(Key::R) {
            self.toggle_record();
        }
        if pressed(Key::M) {
            self.toggle_play(Track::Mine);
        }
        if pressed(Key::C) {
            self.toggle_play(Track::Compare);
        }
        if pressed(Key::ArrowDown) || pressed(Key::ArrowRight) {
            self.step(1);
        }
        if pressed(Key::ArrowUp) || pressed(Key::ArrowLeft) {
            self.step(-1);
        }
        if pressed(Key::N) {
            self.next_unrecorded();
        }
        if pressed(Key::Delete) {
            self.trash_current();
        }
        if pressed(Key::Escape) {
            if let Some(r) = &self.recorder
                && r.is_recording()
            {
                r.cancel();
                self.info("Запись отменена");
            }
            if let Some(p) = &self.player {
                p.stop();
            }
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Dota Revoice").strong().size(16.0));
            ui.add_space(6.0);
            if ui.button("Открыть папку…").clicked()
                && let Some(dir) = rfd::FileDialog::new().set_title("Папка с фразами героя").pick_folder()
            {
                self.open_project(&dir);
            }
            if let Some(p) = &self.project {
                chip(ui, &p.resource_dir);
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let can_pack = self.project.is_some() && self.pack.job.is_none();
                let pack_btn = egui::Button::new(RichText::new("Собрать пак").color(Color32::WHITE)).fill(ACCENT);
                if ui.add_enabled(can_pack, pack_btn).clicked()
                    && let Some(p) = &self.project
                {
                    self.pack.open = true;
                    self.pack.result = None;
                    self.pack.mine_count = pack::collect_export(&p.export_root).map(|m| m.len()).unwrap_or(0);
                }
                ui.add_space(8.0);
                let current = self.recorder.as_ref().map(|r| r.device_name.clone()).unwrap_or_else(|| "нет".into());
                let mut chosen = None;
                egui::ComboBox::from_id_salt("mic").selected_text(current.as_str()).width(230.0).show_ui(ui, |ui| {
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
                ui.weak("Микрофон");
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        // Статус Доты справа целиком, сообщение слева обрезается по ширине.
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(dota_status(self.dota.as_ref())).weak().small());
            ui.separator();
            ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                let text = match &self.message {
                    Some(Message::Info(t)) => RichText::new(t.as_str()).small(),
                    Some(Message::Error(t)) => RichText::new(t.as_str()).small().color(RED),
                    None => RichText::new(""),
                };
                ui.add(egui::Label::new(text).truncate());
            });
        });
    }

    fn line_list(&mut self, ui: &mut egui::Ui) {
        let Some(p) = &self.project else {
            ui.weak("Папка не открыта");
            return;
        };
        egui::TopBottomPanel::bottom("legend").frame(egui::Frame::default()).show_inside(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                for (status, text) in [(Status::Recorded, "записано"), (Status::Redo, "переделать"), (Status::Empty, "пусто")] {
                    let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.0, status_color(ui, status));
                    ui.label(RichText::new(text).small().weak());
                    ui.add_space(4.0);
                }
            });
        });

        ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Поиск фразы или категории").desired_width(f32::INFINITY));
        ui.add_space(4.0);
        let done = p.recorded_count();
        let total = p.lines.len();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Записано").small().weak());
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("{done} / {total}")).small().weak());
            });
        });
        let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 5.0), Sense::hover());
        ui.painter().rect_filled(bar, 3.0, ui.visuals().extreme_bg_color);
        let mut filled = bar;
        filled.set_width(bar.width() * done as f32 / total.max(1) as f32);
        ui.painter().rect_filled(filled, 3.0, GREEN);
        ui.add_space(6.0);

        let q = self.search.trim().to_lowercase();
        let mut clicked = None;
        let scroll = self.scroll_to_selected > 0;
        self.scroll_to_selected = self.scroll_to_selected.saturating_sub(1);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (cat, idx) in &self.groups {
                let label = project::category_label(cat);
                let label_lc = label.to_lowercase();
                let shown: Vec<usize> = idx
                    .iter()
                    .copied()
                    .filter(|&i| q.is_empty() || p.lines[i].name.contains(&q) || label_lc.contains(&q))
                    .collect();
                if shown.is_empty() {
                    continue;
                }
                let rec = shown.iter().filter(|&&i| p.status(&p.lines[i]) != Status::Empty).count();
                let has_selected = shown.contains(&self.selected);
                egui::CollapsingHeader::new(RichText::new(format!("{label}   {rec}/{}", shown.len())).strong())
                    .id_salt(cat)
                    .default_open(true)
                    .open((scroll && has_selected).then_some(true))
                    .show(ui, |ui| {
                        for i in shown {
                            let line = &p.lines[i];
                            let short = line.name.strip_prefix(&format!("{}_", prefix_of(&line.name))).unwrap_or(&line.name);
                            let resp = list_row(ui, short, p.status(line), i == self.selected);
                            if resp.clicked() {
                                clicked = Some(i);
                            }
                            if scroll && i == self.selected {
                                resp.scroll_to_me(Some(Align::Center));
                            }
                        }
                    });
            }
        });
        if let Some(i) = clicked {
            self.select(i);
            self.scroll_to_selected = 0;
        }
    }

    fn editor(&mut self, ui: &mut egui::Ui) {
        let Some(line) = self.current_line() else {
            ui.vertical_centered(|ui| {
                ui.add_space(140.0);
                ui.heading("Открой папку с фразами героя");
                ui.weak("Выгрузка из Source 2 Viewer, например …/sounds/vo/bounty_hunter");
                ui.add_space(10.0);
                let btn = egui::Button::new(RichText::new("Открыть папку…").color(Color32::WHITE)).fill(ACCENT);
                if ui.add(btn).clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    self.open_project(&dir);
                }
            });
            return;
        };
        let status = self.project.as_ref().map(|p| p.status(&line)).unwrap_or(Status::Empty);
        let recording = self.recorder.as_ref().is_some_and(|r| r.is_recording());
        let now = self.player.as_ref().and_then(|p| p.now_playing());

        ui.horizontal(|ui| {
            ui.label(RichText::new(line.name.as_str()).monospace().size(20.0).strong());
            ui.add_space(4.0);
            let (text, color) = match status {
                Status::Empty => ("пусто", ui.visuals().weak_text_color()),
                Status::Recorded => ("записано", GREEN),
                Status::Redo => ("переделать", AMBER),
            };
            badge(ui, text, color);
        });
        let orig_secs = self.original.as_ref().map_or(0.0, |c| c.duration_secs());
        ui.weak(format!("{} · {orig_secs:.1} с · 44.1 кГц, моно", project::category_label(&line.category)));
        ui.add_space(10.0);

        // Обе волны в одном масштабе времени — сразу видно, длиннее ли моя версия.
        let live = recording.then(|| self.recorder.as_ref().map(|r| r.live_peaks(1024))).flatten();
        let mine_secs = match &live {
            Some((_, secs)) => *secs,
            None => self.mine.as_ref().map_or(0.0, |c| c.duration_secs()),
        };
        let timeline = orig_secs.max(mine_secs).max(0.1);
        let head = |t: Track| now.filter(|(nt, _)| *nt == t).map(|(_, f)| f);

        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Оригинал").strong());
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if let Some(c) = &self.original {
                        ui.weak(format!("громкость {:.1} dB", c.rms_db()));
                    }
                });
            });
            let resp = waveform(ui, self.original.as_ref().map(|c| c.peaks.as_slice()), orig_secs / timeline, head(Track::Original), ORIGINAL_WAVE, "Оригинал не загружен");
            if resp.clicked() {
                self.toggle_play(Track::Original);
            }
            ui.horizontal(|ui| {
                let playing = head(Track::Original).is_some();
                if ui.add(egui::Button::new(if playing { "⏹ Стоп" } else { "▶ Играть" }).shortcut_text("Space")).clicked() {
                    self.toggle_play(Track::Original);
                }
                let can_compare = self.original.is_some() && self.mine.is_some();
                if ui.add_enabled(can_compare, egui::Button::new("Оригинал, потом моя").shortcut_text("C")).clicked() {
                    self.toggle_play(Track::Compare);
                }
            });
        });
        ui.add_space(8.0);

        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Моя версия").strong());
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if recording {
                        ui.colored_label(RED, format!("⏺ запись… {mine_secs:.1} с"));
                    } else if let (Some(m), Some(o)) = (&self.mine, &self.original) {
                        let diff = m.rms_db() - o.rms_db();
                        let (hint, color) = if diff.abs() < 2.0 {
                            ("≈ как оригинал".to_owned(), GREEN)
                        } else if diff < 0.0 {
                            (format!("тише на {:.1} dB", -diff), AMBER)
                        } else {
                            (format!("громче на {diff:.1} dB"), AMBER)
                        };
                        ui.colored_label(color, hint);
                        ui.weak(format!("{:.1} с · громкость {:.1} dB ·", m.duration_secs(), m.rms_db()));
                    }
                });
            });
            let resp = match &live {
                Some((peaks, _)) => waveform(ui, Some(peaks), mine_secs / timeline, None, RED, ""),
                None => waveform(ui, self.mine.as_ref().map(|c| c.peaks.as_slice()), mine_secs / timeline, head(Track::Mine), MINE_WAVE, "Нажми R, чтобы записать"),
            };
            if resp.clicked() && !recording {
                self.toggle_play(Track::Mine);
            }
            self.level_meter(ui);

            ui.horizontal(|ui| {
                let rec_btn = if recording {
                    egui::Button::new(RichText::new("⏹ Стоп").color(Color32::WHITE)).fill(RED)
                } else {
                    egui::Button::new(RichText::new("⏺ Запись").color(RED)).stroke(Stroke::new(1.0_f32, RED))
                };
                if ui.add_enabled(self.recorder.is_some(), rec_btn.shortcut_text("R")).clicked() {
                    self.toggle_record();
                }
                let playing = head(Track::Mine).is_some();
                let mine_btn = egui::Button::new(if playing { "⏹ Стоп" } else { "▶ Моя" }).shortcut_text("M");
                if ui.add_enabled(self.mine.is_some() && !recording, mine_btn).clicked() {
                    self.toggle_play(Track::Mine);
                }
                if ui.add_enabled(!recording, egui::Button::new("Из файла…")).clicked() {
                    self.import_file();
                }
                let redo = status == Status::Redo;
                let redo_btn = egui::SelectableLabel::new(redo, RichText::new("⚑ Переделать").color(if redo { AMBER } else { ui.visuals().text_color() }));
                if ui.add_enabled(status != Status::Empty && !recording, redo_btn).clicked()
                    && let Some(p) = self.project.as_mut()
                    && let Err(e) = p.set_redo(&line, !redo)
                {
                    self.error(format!("{e:#}"));
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    let trash = egui::Button::new("🗑 В корзину").shortcut_text("Del");
                    if ui.add_enabled(status != Status::Empty && !recording, trash).clicked() {
                        self.trash_current();
                    }
                });
            });
        });

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new("⬅ Назад").shortcut_text("⬆")).clicked() {
                self.step(-1);
            }
            if ui.add(egui::Button::new("Далее ➡").shortcut_text("⬇")).clicked() {
                self.step(1);
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if ui.add(egui::Button::new("Следующая незаписанная").shortcut_text("N")).clicked() {
                    self.next_unrecorded();
                }
            });
        });
        ui.add_space(8.0);
        ui.label(RichText::new("Esc — отменить запись или остановить звук. Клик по волне — тоже воспроизведение.").small().weak());
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
        ui.horizontal(|ui| {
            ui.label(RichText::new("Микрофон").small().weak());
            let width = (ui.available_width() - 64.0).max(40.0);
            let (rect, _) = ui.allocate_exact_size(vec2(width, 6.0), Sense::hover());
            let p = ui.painter_at(rect);
            p.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
            let mut filled = rect;
            filled.set_width(rect.width() * frac.clamp(0.0, 1.0));
            p.rect_filled(filled, 3.0, color);
            let text = if db <= -60.0 { "тишина".to_owned() } else { format!("{db:.0} dB") };
            ui.label(RichText::new(text).small().weak());
        });
        if self.clipped {
            ui.colored_label(RED, "Перегруз микрофона — отодвинься или убавь усиление и перезапиши");
        }
    }

    fn pack_window(&mut self, ctx: &egui::Context) {
        if let Some(job) = &self.pack.job
            && let Ok(result) = job.rx.try_recv()
        {
            let out = job.out.clone();
            self.pack.result = Some(match result {
                Ok(s) => {
                    let mut text = format!(
                        "Готово: твоих фраз {}, скопировано из озвучки Valve {}, размер {:.1} МБ.",
                        s.replaced + s.not_in_base.len(),
                        s.copied,
                        s.size_bytes as f64 / 1_048_576.0
                    );
                    if !s.not_in_base.is_empty() {
                        text += &format!("\nВ озвучке Valve нет: {} — проверь имена.", s.not_in_base.join(", "));
                    }
                    Ok((text, out))
                }
                Err(e) => Err(format!("{e:#}")),
            });
            self.pack.job = None;
        }
        if !self.pack.open {
            return;
        }
        let Some(p) = &self.project else { return };
        let voice = self.dota.as_ref().and_then(|d| d.voice_pak());
        let out = p.export_root.join("build").join("pak02_dir.vpk");
        let mut start = false;
        let mut open = self.pack.open;
        egui::Window::new("Сборка пака").open(&mut open).collapsible(false).resizable(false).default_width(460.0).show(ctx, |ui| {
            let mine = self.pack.mine_count;
            ui.label(format!("Твоих фраз в {}: {mine}", p.export_root.display()));
            match &voice {
                Some(v) => {
                    ui.checkbox(&mut self.pack.use_base, "Взять за основу озвучку из игры (рекомендуется)");
                    ui.weak(format!("{}{}", v.path.display(), if v.enabled { "" } else { "  (уже отключён — работает наш pak02)" }));
                    if !self.pack.use_base {
                        ui.weak("Без основы остальные фразы в игре станут английскими.");
                    }
                }
                None => {
                    ui.weak("Озвучка в папке языка не найдена — соберу только твои фразы, остальные будут английскими.");
                }
            }
            ui.weak(format!("Результат: {}", out.display()));
            ui.add_space(6.0);
            if let Some(job) = &self.pack.job {
                let frac = job.progress.load(Ordering::Relaxed) as f32 / 1000.0;
                ui.add(egui::ProgressBar::new(frac).show_percentage().animate(true));
            } else if ui.add_enabled(mine > 0, egui::Button::new(RichText::new("Собрать").color(Color32::WHITE)).fill(ACCENT)).clicked() {
                start = true;
            }
            match &self.pack.result {
                Some(Ok((text, path))) => {
                    ui.add_space(6.0);
                    ui.colored_label(GREEN, text.as_str());
                    if let Some(dir) = self.dota.as_ref().and_then(|d| d.language_dir()) {
                        ui.label(format!(
                            "Установка пока вручную: скопируй pak02_dir.vpk в {} и переименуй там pak01_dir.vpk в pak01_dir.vpk.off. Перезапусти Доту.",
                            dir.display()
                        ));
                    }
                    if ui.button("Показать в папке").clicked() {
                        reveal(path);
                    }
                }
                Some(Err(e)) => {
                    ui.colored_label(RED, e.as_str());
                }
                None => {}
            }
        });
        self.pack.open = open;

        if start {
            let base = voice.filter(|_| self.pack.use_base).map(|v| v.path);
            let progress = Arc::new(AtomicU32::new(0));
            let (tx, rx) = mpsc::channel();
            let export_root = p.export_root.clone();
            let (out2, prog2, ctx2) = (out.clone(), progress.clone(), ctx.clone());
            std::thread::spawn(move || {
                let result = pack::collect_export(&export_root).and_then(|mine| {
                    pack::build(base.as_deref(), &mine, &out2, |f| {
                        prog2.store((f * 1000.0) as u32, Ordering::Relaxed);
                        ctx2.request_repaint();
                    })
                });
                let _ = tx.send(result);
                ctx2.request_repaint();
            });
            self.pack.result = None;
            self.pack.job = Some(PackJob { progress, rx, out });
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_keys(ctx);

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(2.0);
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| self.status_bar(ui));
        egui::SidePanel::left("lines").resizable(true).default_width(270.0).show(ctx, |ui| {
            ui.add_space(6.0);
            self.line_list(ui);
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(6.0);
            self.editor(ui);
        });
        self.pack_window(ctx);

        // Анимация волны и индикатора уровня; в покое — редкая перерисовка.
        let busy = self.player.as_ref().is_some_and(|p| p.now_playing().is_some())
            || self.recorder.as_ref().is_some_and(|r| r.is_recording())
            || self.pack.job.is_some();
        ctx.request_repaint_after(if busy { Duration::from_millis(16) } else { Duration::from_millis(60) });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.config.save();
    }
}

fn setup_style(ctx: &egui::Context) {
    use egui::FontFamily::{Monospace, Proportional};
    use egui::TextStyle;
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 7.0);
        s.spacing.button_padding = vec2(10.0, 5.0);
        s.spacing.interact_size.y = 26.0;
        s.text_styles = [
            (TextStyle::Small, FontId::new(12.0, Proportional)),
            (TextStyle::Body, FontId::new(14.5, Proportional)),
            (TextStyle::Button, FontId::new(14.5, Proportional)),
            (TextStyle::Heading, FontId::new(20.0, Proportional)),
            (TextStyle::Monospace, FontId::new(13.5, Monospace)),
        ]
        .into();
        let radius = egui::CornerRadius::same(6);
        let w = &mut s.visuals.widgets;
        for state in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
            state.corner_radius = radius;
        }
        s.visuals.window_corner_radius = egui::CornerRadius::same(10);
        s.visuals.selection.bg_fill = ACCENT;
        s.visuals.selection.stroke.color = Color32::WHITE;
    });
}

/// Блок с рамкой, как карточки в макете.
fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::default()
        .fill(ui.visuals().faint_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(10)
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Цветная плашка статуса.
fn badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::default()
        .fill(color.gamma_multiply(0.18))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| ui.label(RichText::new(text).small().color(color)));
}

/// «Таблетка» с моноширинным текстом — путь внутри игры.
fn chip(ui: &mut egui::Ui, text: &str) {
    egui::Frame::default()
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| ui.label(RichText::new(text).monospace().weak()));
}

/// Строка списка во всю ширину: точка статуса и имя фразы.
fn list_row(ui: &mut egui::Ui, text: &str, status: Status, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
    let v = ui.visuals();
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 5.0, v.selection.bg_fill);
    } else if resp.hovered() {
        p.rect_filled(rect, 5.0, v.widgets.hovered.weak_bg_fill);
    }
    p.circle_filled(pos2(rect.left() + 12.0, rect.center().y), 4.0, status_color(ui, status));
    let color = if selected { v.selection.stroke.color } else { v.text_color() };
    p.text(pos2(rect.left() + 24.0, rect.center().y), Align2::LEFT_CENTER, text, FontId::monospace(13.0), color);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Рисует волну по огибающей на доле `span` ширины (общая шкала времени).
fn waveform(
    ui: &mut egui::Ui,
    peaks: Option<&[(f32, f32)]>,
    span: f32,
    playhead: Option<f32>,
    color: Color32,
    empty: &str,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 90.0), Sense::click());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 6.0, ui.visuals().extreme_bg_color);
    let inner = rect.shrink2(vec2(6.0, 6.0));
    let width = inner.width() * span.clamp(0.0, 1.0);
    match peaks.filter(|p| !p.is_empty()) {
        Some(peaks) => {
            let mid = inner.center().y;
            let half = inner.height() / 2.0;
            let cols = width.max(1.0) as usize;
            for x in 0..cols {
                let a = x * peaks.len() / cols;
                let b = ((x + 1) * peaks.len() / cols).clamp(a + 1, peaks.len());
                let (lo, hi) = peaks[a..b].iter().fold((0f32, 0f32), |(l, h), &(pl, ph)| (l.min(pl), h.max(ph)));
                let px = inner.left() + x as f32 + 0.5;
                let (top, bottom) = (mid - hi.min(1.0) * half, mid - lo.max(-1.0) * half);
                p.line_segment([pos2(px, top.min(mid - 0.5)), pos2(px, bottom.max(mid + 0.5))], Stroke::new(1.0_f32, color));
            }
        }
        None => {
            p.text(rect.center(), Align2::CENTER_CENTER, empty, FontId::proportional(13.0), ui.visuals().weak_text_color());
        }
    }
    if let Some(f) = playhead {
        let x = inner.left() + width * f.clamp(0.0, 1.0);
        p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.5_f32, ui.visuals().strong_text_color()));
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn status_color(ui: &egui::Ui, status: Status) -> Color32 {
    match status {
        Status::Empty => ui.visuals().weak_text_color(),
        Status::Recorded => GREEN,
        Status::Redo => AMBER,
    }
}

/// `bount_kill_01` → `bount`: в списке показываем имя без префикса героя.
fn prefix_of(name: &str) -> &str {
    name.split_once('_').map_or("", |(p, _)| p)
}

fn dota_status(dota: Option<&DotaInstall>) -> String {
    let Some(d) = dota else { return "Дота не найдена".into() };
    let Some(lang) = &d.audio_language else {
        return "Озвучка Доты: английская — для модов переключи язык озвучки".into();
    };
    match d.voice_pak() {
        Some(v) if v.enabled => format!("Озвучка: {lang} · pak01 Valve включён (перекрывает pak02)"),
        Some(_) => format!("Озвучка: {lang} · pak01 Valve отключён — работает pak02"),
        None => format!("Озвучка: {lang} · пак озвучки не скачан"),
    }
}

fn reveal(path: &Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
}
