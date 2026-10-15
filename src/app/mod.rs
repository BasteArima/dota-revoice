//! Окно программы. Две вкладки: «Запись» (`record.rs`) и «Сборка и установка» (`build.rs`).

mod build;
mod record;
mod theme;

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dota_revoice::audio::{self, Clip, Player, Recorder, Track};
use dota_revoice::encode;
use dota_revoice::game::{self, DotaInstall};
use dota_revoice::project::{self, Line, Project, Status};
use eframe::egui::{self, Align, Key, Modifiers, RichText};

use crate::config::Config;
use build::BuildState;
use theme::{GOLD, ON_GOLD, PANEL, RED, icon};

const MIN_TAKE_SECS: f32 = 0.2;
const COMPARE_GAP_SECS: f32 = 0.4;
/// Минимальная длина обрезанной фразы.
const MIN_TRIM_SAMPLES: usize = audio::RATE as usize / 10;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Record,
    Build,
}

enum Message {
    Info(String),
    Error(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Handle {
    Start,
    End,
}

/// Перетаскивание края обрезки: пока тянем, файл не пересохраняем.
#[derive(Clone, Copy)]
struct TrimDrag {
    handle: Handle,
    range: (usize, usize),
}

pub struct App {
    config: Config,
    dota: Option<DotaInstall>,
    screen: Screen,
    project: Option<Project>,
    /// Категории в порядке показа: (категория, индексы фраз).
    groups: Vec<(String, Vec<usize>)>,
    collapsed: HashSet<String>,
    selected: usize,
    search: String,
    /// Заметка выбранной фразы, пока её редактируют.
    note: String,
    /// Сколько кадров ещё докручивать список до выбранной фразы:
    /// в первом кадре позиции строк ещё неточные.
    scroll_to_selected: u8,

    original: Option<Clip>,
    /// Моя запись целиком (исходный wav) — её показываем на волне.
    mine_raw: Option<Clip>,
    /// Кусок `mine_raw`, который идёт в игру, в сэмплах.
    trim: (usize, usize),
    /// Обрезанная версия — её играем и сравниваем.
    mine: Option<Clip>,
    drag: Option<TrimDrag>,
    player: Option<Player>,
    recorder: Option<Recorder>,
    input_devices: Vec<String>,
    level: f32,
    clipped: bool,

    message: Option<Message>,
    build: BuildState,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::setup(&cc.egui_ctx);
        let config = Config::load();
        let mut app = Self {
            dota: game::find_dota(),
            screen: Screen::Record,
            project: None,
            groups: Vec::new(),
            collapsed: HashSet::new(),
            selected: 0,
            search: String::new(),
            note: String::new(),
            scroll_to_selected: 0,
            original: None,
            mine_raw: None,
            trim: (0, 0),
            mine: None,
            drag: None,
            player: None,
            recorder: None,
            input_devices: Recorder::input_device_names(),
            level: 0.0,
            clipped: false,
            message: None,
            build: BuildState::default(),
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

    fn pick_folder(&mut self) {
        if let Some(dir) = rfd::FileDialog::new().set_title("Папка с фразами героя").pick_folder() {
            self.open_project(&dir);
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
                    hero_title(&p.hero),
                    p.lines.len(),
                    p.recorded_count(),
                    p.export_root.join(&p.resource_dir).display()
                ));
                self.groups = groups;
                self.collapsed.clear();
                self.project = Some(p);
                self.build = BuildState::default();
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
        self.mine_raw = None;
        self.drag = None;
        let Some(line) = self.current_line() else { return };
        self.note = self.project.as_ref().map(|p| p.note(&line).to_owned()).unwrap_or_default();
        match audio::decode_file(&line.original) {
            Ok(s) => self.original = Some(Clip::new(s)),
            Err(e) => self.error(format!("Оригинал не читается: {e:#}")),
        }
        self.reload_mine(&line);
    }

    /// Загружает мою запись: исходный wav (если есть) и сохранённую обрезку.
    fn reload_mine(&mut self, line: &Line) {
        self.mine = None;
        self.mine_raw = None;
        self.drag = None;
        let Some(p) = &self.project else { return };
        let (mp3, raw) = (p.export_mp3(line), p.raw_wav(line));
        if !mp3.is_file() {
            return;
        }
        // Без wav (например, файл положили руками) обрезка от mp3 не имеет смысла.
        let (source, trim) = if raw.is_file() { (raw, p.trim(line)) } else { (mp3, None) };
        match audio::decode_file(&source) {
            Ok(samples) => {
                let len = samples.len();
                let range = trim
                    .map(|(a, b)| (secs_to_index(a, len), secs_to_index(b, len)))
                    .filter(|(a, b)| b > a)
                    .unwrap_or((0, len));
                self.mine_raw = Some(Clip::new(samples));
                self.set_mine_slice(range);
            }
            Err(e) => self.error(format!("Моя версия не читается: {e:#}")),
        }
    }

    /// Пересчитывает обрезанную версию из исходника (без записи на диск).
    fn set_mine_slice(&mut self, range: (usize, usize)) {
        let Some(raw) = &self.mine_raw else { return };
        let len = raw.samples.len();
        self.trim = (range.0.min(len), range.1.min(len));
        let mut part = raw.samples[self.trim.0..self.trim.1].to_vec();
        if self.trim != (0, len) {
            audio::fade_edges(&mut part);
        }
        self.mine = Some(Clip::new(part));
    }

    /// Применяет обрезку: запоминает границы и перезаписывает mp3 для игры.
    fn apply_trim(&mut self, range: (usize, usize)) -> anyhow::Result<()> {
        let (Some(line), Some(raw)) = (self.current_line(), self.mine_raw.clone()) else { return Ok(()) };
        if let Some(pl) = &self.player {
            pl.stop();
        }
        self.set_mine_slice(range);
        let len = raw.samples.len();
        let Some(p) = self.project.as_mut() else { return Ok(()) };
        let raw_path = p.raw_wav(&line);
        if !raw_path.is_file() {
            encode::save_raw(&raw_path, &raw.samples)?;
        }
        let full = self.trim == (0, len);
        p.set_trim(&line, (!full).then(|| (index_to_secs(self.trim.0), index_to_secs(self.trim.1))))?;
        let mine = self.mine.as_ref().map(|c| c.samples.clone()).unwrap_or_default();
        encode::write_mp3(&p.export_mp3(&line), &mine)
    }

    fn trim_silence(&mut self) {
        let Some(raw) = &self.mine_raw else { return };
        let len = raw.samples.len();
        let Some(range) = audio::detect_voice(&raw.samples) else {
            self.error("Не нашёл голос в записи — слишком тихо?");
            return;
        };
        match self.apply_trim(range) {
            Ok(()) if range == (0, len) => self.info("Тишины по краям нет — обрезать нечего"),
            Ok(()) => self.info(format!("Тишина обрезана: {:.2}–{:.2} с", index_to_secs(range.0), index_to_secs(range.1))),
            Err(e) => self.error(format!("Обрезка не сохранилась: {e:#}")),
        }
    }

    fn reset_trim(&mut self) {
        let Some(len) = self.mine_raw.as_ref().map(|r| r.samples.len()) else { return };
        match self.apply_trim((0, len)) {
            Ok(()) => self.info("Обрезка снята — в игру пойдёт запись целиком"),
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    fn save_note(&mut self) {
        let (Some(line), Some(p)) = (self.current_line(), self.project.as_mut()) else { return };
        if let Err(e) = p.set_note(&line, &self.note) {
            self.error(format!("Заметка не сохранилась: {e:#}"));
        }
    }

    /// Подходит ли фраза под поиск: имя, категория или заметка.
    fn matches(&self, i: usize, query: &str) -> bool {
        let Some(p) = &self.project else { return false };
        let line = &p.lines[i];
        query.is_empty()
            || line.name.contains(query)
            || project::category_label(&line.category).to_lowercase().contains(query)
            || p.note(line).to_lowercase().contains(query)
    }

    /// Фразы в порядке списка с учётом поиска.
    fn visible_order(&self) -> Vec<usize> {
        let q = self.search.trim().to_lowercase();
        self.groups.iter().flat_map(|(_, idx)| idx.iter().copied()).filter(|&i| self.matches(i, &q)).collect()
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
        let order = self.visible_order();
        let Some(p) = &self.project else { return };
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

    fn is_recording(&self) -> bool {
        self.recorder.as_ref().is_some_and(|r| r.is_recording())
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
        if let Err(e) = encode::save_raw(&p.raw_wav(&line), &samples) {
            self.error(format!("Не сохранилось: {e:#}"));
            return;
        }
        let len = samples.len();
        let range = if self.config.auto_trim { audio::detect_voice(&samples).unwrap_or((0, len)) } else { (0, len) };
        self.mine_raw = Some(Clip::new(samples));
        let result = self.apply_trim(range).and_then(|_| match self.project.as_mut() {
            Some(p) => p.mark_recorded(&line),
            None => Ok(()),
        });
        match result {
            Ok(()) if range != (0, len) => self.info(format!(
                "{verb}: {} — тишина обрезана, {:.1} из {secs:.1} с",
                mp3.display(),
                index_to_secs(range.1 - range.0)
            )),
            Ok(()) => self.info(format!("{verb}: {} ({secs:.1} с)", mp3.display())),
            Err(e) => self.error(format!("Не сохранилось: {e:#}")),
        }
    }

    fn set_redo(&mut self, redo: bool) {
        let (Some(line), Some(p)) = (self.current_line(), self.project.as_mut()) else { return };
        if let Err(e) = p.set_redo(&line, redo) {
            self.error(format!("{e:#}"));
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
                self.mine_raw = None;
                self.info(format!("{} перенесена в корзину: {}", line.name, trash_dir.display()));
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if self.project.is_none() || self.screen != Screen::Record || ctx.wants_keyboard_input() {
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
        if pressed(Key::T) {
            self.trim_silence();
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
            // Логотип: золотой квадрат с микрофоном.
            let (rect, _) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 7.0, GOLD);
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icon::MICROPHONE, egui::FontId::proportional(16.0), ON_GOLD);
            ui.label(theme::title("Dota Revoice", 16.0));
            ui.add_space(10.0);

            if let Some(p) = &self.project {
                let text = format!("{}  ·  {}/{}", hero_title(&p.hero), p.recorded_count(), p.lines.len());
                let chip = egui::Button::new(RichText::new(text).color(theme::TEXT)).corner_radius(99);
                if ui.add(chip).on_hover_text("Открыть другую папку с фразами").clicked() {
                    self.pick_folder();
                }
            } else if ui.add(theme::primary(format!("{}  Открыть папку", icon::FOLDER_OPEN))).clicked() {
                self.pick_folder();
            }
            ui.add_space(10.0);

            // Вкладки — сегментированный переключатель.
            egui::Frame::default().fill(theme::BG).corner_radius(10).inner_margin(egui::Margin::same(3)).show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (screen, label) in [(Screen::Record, "Запись"), (Screen::Build, "Сборка и установка")] {
                    let on = self.screen == screen;
                    let text = RichText::new(label).color(if on { theme::TEXT } else { theme::MUTED });
                    let btn = egui::Button::new(text).fill(if on { theme::CARD } else { egui::Color32::TRANSPARENT }).stroke(egui::Stroke::NONE).corner_radius(8);
                    if ui.add_enabled(self.project.is_some(), btn).clicked() && !on {
                        self.screen = screen;
                        if screen == Screen::Build {
                            self.build.refresh_now();
                        }
                    }
                }
            });

            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let current = self.recorder.as_ref().map(|r| r.device_name.clone()).unwrap_or_else(|| "нет микрофона".into());
                let mut chosen = None;
                egui::ComboBox::from_id_salt("mic")
                    .selected_text(format!("{}  {current}", icon::MICROPHONE))
                    .width(240.0)
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
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        // Статус Доты справа целиком, сообщение слева обрезается по ширине.
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.label(theme::small(dota_status(self.dota.as_ref())));
            ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                let text = match &self.message {
                    Some(Message::Info(t)) => theme::small(t.as_str()),
                    Some(Message::Error(t)) => RichText::new(t.as_str()).size(12.0).color(RED),
                    None => RichText::new(""),
                };
                ui.add(egui::Label::new(text).truncate());
            });
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_keys(ctx);
        self.poll_build_tasks();

        let bar_frame = egui::Frame::side_top_panel(&ctx.style()).fill(PANEL).inner_margin(egui::Margin::symmetric(14, 10));
        egui::TopBottomPanel::top("top").frame(bar_frame).show(ctx, |ui| self.top_bar(ui));
        egui::TopBottomPanel::bottom("status")
            .frame(egui::Frame::side_top_panel(&ctx.style()).fill(PANEL).inner_margin(egui::Margin::symmetric(14, 5)))
            .show(ctx, |ui| self.status_bar(ui));
        match self.screen {
            Screen::Record => {
                egui::SidePanel::left("lines")
                    .resizable(true)
                    .default_width(250.0)
                    .frame(egui::Frame::side_top_panel(&ctx.style()).fill(PANEL).inner_margin(egui::Margin::same(10)))
                    .show(ctx, |ui| self.line_list(ui));
                egui::CentralPanel::default()
                    .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(22, 16)))
                    .show(ctx, |ui| self.editor(ui));
            }
            Screen::Build => {
                egui::CentralPanel::default()
                    .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(22, 18)))
                    .show(ctx, |ui| self.build_screen(ui));
            }
        }

        // Анимация волны, индикатора и кнопки записи; в покое — редкая перерисовка.
        let busy = self.player.as_ref().is_some_and(|p| p.now_playing().is_some()) || self.is_recording() || self.build.busy();
        ctx.request_repaint_after(if busy { Duration::from_millis(16) } else { Duration::from_millis(60) });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_note();
        self.config.save();
    }
}

fn secs_to_index(secs: f32, len: usize) -> usize {
    ((secs.max(0.0) * audio::RATE as f32) as usize).min(len)
}

fn index_to_secs(index: usize) -> f32 {
    index as f32 / audio::RATE as f32
}

/// `bounty_hunter` → `Bounty Hunter`.
fn hero_title(hero: &str) -> String {
    hero.split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `bount_kill_01` → `bount`: в списке показываем имя без префикса героя.
fn prefix_of(name: &str) -> &str {
    name.split_once('_').map_or("", |(p, _)| p)
}

fn dota_status(dota: Option<&DotaInstall>) -> String {
    let Some(d) = dota else { return "Дота не найдена".into() };
    let Some(lang) = &d.audio_language else {
        return "Озвучка Доты английская — для модов переключи язык озвучки".into();
    };
    match d.voice_pak() {
        Some(v) if v.enabled => format!("Дота · озвучка {lang} · пак Valve включён"),
        Some(_) => format!("Дота · озвучка {lang} · работает наш пак"),
        None => format!("Дота · озвучка {lang} · пак озвучки не скачан"),
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
