//! Окно программы. Вкладки «Герои» (`heroes.rs`), «Запись» (`record.rs`),
//! «Сборка и установка» (`build.rs`), «Настройки» (`settings.rs`);
//! при первом запуске — мастер (`setup.rs`).

mod build;
mod heroes;
mod loudness;
mod record;
mod settings;
mod setup;
mod theme;

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use dota_revoice::audio::{self, Clip, Player, Recorder, Track};
use dota_revoice::{encode, install, update};
use dota_revoice::process::{self, Settings};
use dota_revoice::game::{self, DotaInstall};
use dota_revoice::project::{self, Line, Project, Status, Take};
use eframe::egui::{self, Align, Key, Modifiers, RichText};

use crate::config::{Config, PreRoll};
use build::BuildState;
use heroes::HeroesState;
use settings::SettingsState;
use theme::{GOLD, ON_GOLD, PANEL, RED, icon};
use dota_revoice::{tr, trf};

const MIN_TAKE_SECS: f32 = 0.2;
/// Как часто перепроверять игру: Steam мог обновить Доту, пока программа открыта.
const GAME_CHECK: Duration = Duration::from_secs(5);
const COMPARE_GAP_SECS: f32 = 0.4;
/// Сколько секунд висит обычное сообщение (плюс время на чтение длинного текста).
const TOAST_SECS: f32 = 3.5;
/// Минимальная длина обрезанной фразы.
const MIN_TRIM_SAMPLES: usize = audio::RATE as usize / 10;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Heroes,
    Record,
    Build,
    Settings,
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

/// Подготовка к записи: отсчёт или прослушивание оригинала.
#[derive(Clone, Copy)]
enum Pending {
    Countdown { started: f64, beeps: u8 },
    Original { ended: Option<f64> },
}

/// Кеш тяжёлой обработки (шумодав, тон) для одной записи.
struct Shaped {
    raw: Arc<Vec<f32>>,
    denoise: bool,
    pitch: f32,
    samples: Vec<f32>,
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
    /// Моя запись как есть (исходный wav).
    mine_raw: Option<Arc<Vec<f32>>>,
    /// Запись после шумодава и тона, до усиления: ползунок громкости пересчитывает только усиление.
    shaped: Option<Shaped>,
    /// Запись целиком после шумодава и усиления — её показываем на волне.
    mine_full: Option<Clip>,
    /// Кусок `mine_full`, который идёт в игру, в сэмплах.
    trim: (usize, usize),
    /// Шумодав и усиление выбранной фразы.
    fx: Settings,
    /// Готовая версия для игры — её играем и сравниваем.
    mine: Option<Clip>,
    drag: Option<TrimDrag>,
    pending: Option<Pending>,
    /// Прежние дубли выбранной фразы.
    takes: Vec<Take>,
    /// Ширина переключателя вкладок с прошлого кадра — чтобы поставить его по центру.
    tabs_width: f32,
    /// Что не так с игрой (Steam вернул озвучку Valve и т.п.) — баннер сверху.
    alert: Option<install::Alert>,
    game_checked: Option<Instant>,
    /// Ответ GitHub о новой версии (проверяется в фоне при запуске).
    update_rx: Option<mpsc::Receiver<Option<update::Release>>>,
    /// Новая версия для баннера (его можно скрыть до следующего запуска).
    update: Option<update::Release>,
    /// Что ответил GitHub: `Some(None)` — у меня последняя версия, `None` — ещё не знаем.
    latest: Option<Option<update::Release>>,
    player: Option<Player>,
    recorder: Option<Recorder>,
    input_devices: Vec<String>,
    level: f32,
    clipped: bool,

    /// Последнее сообщение — всплывает снизу (`toast`).
    message: Option<Message>,
    message_at: Option<Instant>,
    /// Подгонка громкости всех фраз героя: идёт в фоне или ждёт подтверждения.
    match_all: Option<loudness::MatchAll>,
    build: BuildState,
    settings: SettingsState,
    heroes: HeroesState,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::setup(&cc.egui_ctx);
        let config = Config::load();
        let mut app = Self {
            dota: find_dota(&config),
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
            shaped: None,
            mine_full: None,
            trim: (0, 0),
            fx: Settings::default(),
            mine: None,
            drag: None,
            pending: None,
            takes: Vec::new(),
            tabs_width: 270.0,
            alert: None,
            game_checked: None,
            update_rx: None,
            latest: None,
            message_at: None,
            update: None,
            player: None,
            recorder: None,
            input_devices: Recorder::input_device_names(),
            level: 0.0,
            clipped: false,
            message: None,
            match_all: None,
            build: BuildState::default(),
            settings: SettingsState::default(),
            heroes: HeroesState::default(),
            config,
        };
        match Player::new() {
            Ok(p) => app.player = Some(p),
            Err(e) => app.error(trf!("Звук не выводится: {e:#}", "No audio output: {e:#}")),
        }
        app.open_recorder(app.config.input_device.clone());
        if app.config.check_updates {
            app.start_update_check(&cc.egui_ctx);
        }
        app.migrate_old_config();
        if let Some(folder) = app.config.last_folder.clone().filter(|f| f.is_dir()) {
            app.open_project(&folder);
        }
        if app.project.is_none() {
            app.screen = Screen::Heroes;
        }
        app
    }

    /// Версия до настроек папок хранила свои фразы в `export` рядом с выгрузкой героя.
    /// Тем, кто уже записывал, мастер не показываем: эта папка становится «Моими фразами».
    fn migrate_old_config(&mut self) {
        if self.config.setup_done {
            return;
        }
        let Some(folder) = self.config.last_folder.clone() else { return };
        self.config.export_dir = Some(project::legacy_export_root(&folder));
        self.config.setup_done = true;
        self.config.save();
    }

    fn info(&mut self, text: impl Into<String>) {
        self.message = Some(Message::Info(text.into()));
        self.message_at = Some(Instant::now());
    }

    fn error(&mut self, text: impl Into<String>) {
        self.message = Some(Message::Error(text.into()));
        self.message_at = Some(Instant::now());
    }

    fn open_recorder(&mut self, name: Option<String>) {
        self.recorder = None;
        match Recorder::new(name.as_deref()) {
            Ok(r) => {
                self.config.input_device = name;
                self.recorder = Some(r);
            }
            Err(e) => self.error(trf!("Микрофон не открылся: {e:#}", "Couldn't open the microphone: {e:#}")),
        }
    }

    fn pick_folder(&mut self) {
        if let Some(dir) = rfd::FileDialog::new().set_title(tr!("Папка с фразами героя", "Folder with the hero's lines")).pick_folder() {
            self.open_project(&dir);
        }
    }

    fn open_project(&mut self, folder: &Path) {
        match Project::open_in(folder, &self.config.export_root()) {
            Ok(p) => {
                let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
                for (i, line) in p.lines.iter().enumerate() {
                    match groups.iter_mut().find(|(c, _)| *c == line.category) {
                        Some((_, idx)) => idx.push(i),
                        None => groups.push((line.category.clone(), vec![i])),
                    }
                }
                // Обычные фразы героя — сверху, вторые версии и прочие группы — после них.
                groups.sort_by_key(|(c, _)| c.contains('/'));
                self.info(trf!(
                    "Открыт {}: {} фраз, записано {}. Свои фразы сохраняются в {}", "Opened {}: {} lines, {} recorded. Your lines are saved to {}",
                    self.hero_name(&p.hero),
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
        self.pending = None;
        self.original = None;
        self.mine = None;
        self.mine_full = None;
        self.mine_raw = None;
        self.drag = None;
        let Some(line) = self.current_line() else { return };
        self.note = self.project.as_ref().map(|p| p.note(&line).to_owned()).unwrap_or_default();
        match audio::decode_file(&line.original) {
            Ok(s) => self.original = Some(Clip::new(s)),
            Err(e) => self.error(trf!("Оригинал не читается: {e:#}", "Can't read the original: {e:#}")),
        }
        self.reload_mine(&line);
    }

    /// Загружает мою запись: исходный wav (если есть), обрезку и обработку.
    fn reload_mine(&mut self, line: &Line) {
        self.mine = None;
        self.mine_full = None;
        self.mine_raw = None;
        self.drag = None;
        let Some(p) = &self.project else { return };
        self.takes = p.takes(line);
        let (mp3, raw) = (p.export_mp3(line), p.raw_wav(line));
        if !mp3.is_file() {
            return;
        }
        // Без wav (например, файл положили руками) обрезка и обработка от mp3 не имеют смысла.
        let (source, trim, fx) =
            if raw.is_file() { (raw, p.trim(line), p.processing(line)) } else { (mp3, None, Settings::default()) };
        match audio::decode_file(&source) {
            Ok(samples) => {
                let len = samples.len();
                self.trim = trim
                    .map(|(a, b)| (secs_to_index(a, len), secs_to_index(b, len)))
                    .filter(|(a, b)| b > a)
                    .unwrap_or((0, len));
                self.fx = fx;
                self.mine_raw = Some(Arc::new(samples));
                self.render_full();
            }
            Err(e) => self.error(trf!("Моя версия не читается: {e:#}", "Can't read my version: {e:#}")),
        }
    }

    /// Пересчитывает обработанную запись целиком и кусок для игры (без записи на диск).
    fn render_full(&mut self) {
        let Some(raw) = self.mine_raw.clone() else { return };
        let fx = self.fx;
        let cached = self.shaped.take().filter(|s| Arc::ptr_eq(&s.raw, &raw) && s.denoise == fx.denoise && s.pitch == fx.pitch);
        let shaped = cached.unwrap_or_else(|| Shaped { samples: process::shape(&raw, fx), raw, denoise: fx.denoise, pitch: fx.pitch });
        self.mine_full = Some(Clip::new(process::amplify(&shaped.samples, fx.gain_db)));
        self.shaped = Some(shaped);
        self.render_cut();
    }

    /// Ползунок громкости: пока тянут — только пересчёт, файл пишем, когда отпустили.
    fn preview_gain(&mut self, gain_db: f32) {
        self.fx.gain_db = gain_db;
        self.render_full();
    }

    fn commit_gain(&mut self) {
        let gain = self.fx.gain_db;
        match self.save_mine() {
            Ok(()) => self.info(trf!("Громкость: {gain:+.1} dB", "Loudness: {gain:+.1} dB")),
            Err(e) => self.error(trf!("Громкость не сохранилась: {e:#}", "Loudness wasn't saved: {e:#}")),
        }
    }

    fn render_cut(&mut self) {
        let Some(full) = &self.mine_full else { return };
        let len = full.samples.len();
        self.trim = (self.trim.0.min(len), self.trim.1.min(len));
        self.mine = Some(Clip::new(process::cut(&full.samples, self.trim)));
    }

    fn full_len(&self) -> usize {
        self.mine_full.as_ref().map_or(0, |c| c.samples.len())
    }

    /// Запоминает обрезку и обработку и перезаписывает mp3 для игры.
    fn save_mine(&mut self) -> anyhow::Result<()> {
        let (Some(line), Some(raw)) = (self.current_line(), self.mine_raw.clone()) else { return Ok(()) };
        if let Some(pl) = &self.player {
            pl.stop();
        }
        let len = raw.len();
        let full = self.trim == (0, len);
        let trim = (!full).then(|| (index_to_secs(self.trim.0), index_to_secs(self.trim.1)));
        let mine = self.mine.as_ref().map(|c| c.samples.clone()).unwrap_or_default();
        let Some(p) = self.project.as_mut() else { return Ok(()) };
        let raw_path = p.raw_wav(&line);
        if !raw_path.is_file() {
            encode::save_raw(&raw_path, &raw)?;
        }
        p.set_trim(&line, trim)?;
        p.set_processing(&line, self.fx)?;
        encode::write_mp3(&p.export_mp3(&line), &mine)
    }

    /// Применяет обрезку: запоминает границы и перезаписывает mp3 для игры.
    fn apply_trim(&mut self, range: (usize, usize)) -> anyhow::Result<()> {
        self.trim = range;
        self.render_cut();
        self.save_mine()
    }

    fn apply_fx(&mut self, fx: Settings) -> anyhow::Result<()> {
        self.fx = fx;
        self.render_full();
        self.save_mine()
    }

    /// Усиление, при котором моя версия звучит так же громко, как оригинал.
    fn measure_gain(&self) -> Option<f32> {
        let (Some(o), Some(raw)) = (&self.original, &self.mine_raw) else { return None };
        process::match_gain(&o.samples, raw, Settings { gain_db: 0.0, ..self.fx }, self.trim)
    }

    fn match_loudness(&mut self) {
        if self.mine_raw.is_none() {
            return;
        }
        let Some(gain) = self.measure_gain() else {
            self.error(tr!("Не получилось измерить громкость — запись слишком короткая или тихая", "Couldn't measure loudness — the recording is too short or too quiet"));
            return;
        };
        match self.apply_fx(Settings { gain_db: gain, ..self.fx }) {
            Ok(()) => self.info(trf!("Громкость подогнана под оригинал: {gain:+.1} dB", "Loudness matched to the original: {gain:+.1} dB")),
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    /// Меняет обработку. Шумодав и тон влияют на громкость — если её подгоняли, подгоняем
    /// заново, сохраняя ручную поправку ползунком (насколько громче или тише оригинала).
    fn change_fx(&mut self, change: impl FnOnce(&mut Settings), done: &str) {
        if self.mine_raw.is_none() {
            return;
        }
        let mut fx = self.fx;
        change(&mut fx);
        let regain = fx.gain_db != 0.0;
        let offset = if regain { self.measure_gain().map(|before| fx.gain_db - before) } else { None };
        self.fx = fx;
        if regain && let Some(gain) = self.measure_gain() {
            let gain = gain + offset.unwrap_or(0.0);
            fx.gain_db = gain.clamp(*process::GAIN_RANGE.start(), *process::GAIN_RANGE.end());
        }
        match self.apply_fx(fx) {
            Ok(()) => self.info(done),
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    fn toggle_denoise(&mut self) {
        let on = !self.fx.denoise;
        self.change_fx(|fx| fx.denoise = on, if on { tr!("Шумодав включён", "Noise reduction on") } else { tr!("Шумодав выключен", "Noise reduction off") });
    }

    fn set_pitch(&mut self, semitones: f32) {
        let semitones = semitones.clamp(-12.0, 12.0);
        let text = if semitones == 0.0 { tr!("Тон как в записи", "Pitch as recorded").to_owned() } else { trf!("Тон: {semitones:+.0} полутона", "Pitch: {semitones:+.0} semitones") };
        self.change_fx(|fx| fx.pitch = semitones, &text);
    }

    /// Возвращает прежний дубль; текущий уходит в дубли.
    fn restore_take(&mut self, id: u32) {
        let (Some(line), Some(p)) = (self.current_line(), self.project.as_mut()) else { return };
        let result = p.restore_take(&line, id);
        if let Err(e) = result {
            self.error(trf!("Дубль не вернулся: {e:#}", "Couldn't restore the take: {e:#}"));
            return;
        }
        self.reload_mine(&line);
        let saved = self.save_mine().and_then(|_| match self.project.as_mut() {
            Some(p) => p.mark_recorded(&line),
            None => Ok(()),
        });
        match saved {
            Ok(()) => self.info(trf!("Дубль №{id} снова в работе, прежняя версия — в дублях", "Take #{id} is current again, the previous version went to takes")),
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    fn trash_take(&mut self, id: u32) {
        let (Some(line), Some(p)) = (self.current_line(), self.project.as_mut()) else { return };
        match p.trash_take(&line, id) {
            Ok(()) => {
                self.takes = p.takes(&line);
                self.info(trf!("Дубль №{id} — в корзине", "Take #{id} moved to the trash"));
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    /// Прослушать прежний дубль как есть, без обработки.
    fn play_take(&mut self, take: &Take) {
        match audio::decode_file(&take.path) {
            Ok(s) => {
                if let Some(p) = &self.player {
                    p.play(Track::Cue, Arc::new(s));
                }
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    fn trim_silence(&mut self) {
        let Some(raw) = &self.mine_full else { return };
        let len = raw.samples.len();
        let Some(range) = audio::detect_voice(&raw.samples) else {
            self.error(tr!("Не нашёл голос в записи — слишком тихо?", "No voice found in the recording — too quiet?"));
            return;
        };
        match self.apply_trim(range) {
            Ok(()) if range == (0, len) => self.info(tr!("Тишины по краям нет — обрезать нечего", "No silence at the edges — nothing to trim")),
            Ok(()) => self.info(trf!("Тишина обрезана: {:.2}–{:.2} с", "Silence trimmed: {:.2}–{:.2} s", index_to_secs(range.0), index_to_secs(range.1))),
            Err(e) => self.error(trf!("Обрезка не сохранилась: {e:#}", "Trim wasn't saved: {e:#}")),
        }
    }

    fn reset_trim(&mut self) {
        let Some(len) = self.mine_raw.as_ref().map(|r| r.len()) else { return };
        match self.apply_trim((0, len)) {
            Ok(()) => self.info(tr!("Обрезка снята — в игру пойдёт запись целиком", "Trim removed — the whole recording goes into the game")),
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    fn save_note(&mut self) {
        let (Some(line), Some(p)) = (self.current_line(), self.project.as_mut()) else { return };
        if let Err(e) = p.set_note(&line, &self.note) {
            self.error(trf!("Заметка не сохранилась: {e:#}", "Note wasn't saved: {e:#}"));
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
            None => self.info(tr!("Все фразы в списке записаны", "Every line in the list is recorded")),
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
            Track::Cue => None,
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
            self.error(tr!("Микрофон не открыт — выбери его в настройках", "No microphone open — pick one in the settings"));
            return;
        };
        if self.match_all_running() && !rec.is_recording() {
            self.info(tr!("Подожди: подгоняется громкость всех фраз", "Wait: loudness is being matched for all lines"));
            return;
        }
        if self.pending.take().is_some() {
            if let Some(p) = &self.player {
                p.stop();
            }
            self.info(tr!("Запись отменена", "Recording cancelled"));
            return;
        }
        if !rec.is_recording() {
            match self.config.pre_roll {
                PreRoll::Off => self.start_recording(),
                PreRoll::Countdown => self.pending = Some(Pending::Countdown { started: f64::NAN, beeps: 0 }),
                PreRoll::Original if self.original.is_some() => {
                    if let (Some(p), Some(o)) = (&self.player, &self.original) {
                        p.play(Track::Original, o.samples.clone());
                    }
                    self.pending = Some(Pending::Original { ended: None });
                }
                PreRoll::Original => self.start_recording(),
            }
            return;
        }
        match rec.stop() {
            Ok(samples) => self.store_take(samples, tr!("Записано", "Recorded")),
            Err(e) => self.error(trf!("Запись не сохранилась: {e:#}", "Recording wasn't saved: {e:#}")),
        }
    }

    fn start_recording(&mut self) {
        if let Some(p) = &self.player {
            p.stop();
        }
        self.clipped = false;
        if let Some(r) = &self.recorder {
            r.start();
        }
    }

    /// Ведёт отсчёт или ждёт конца оригинала, потом включает запись.
    fn tick_pending(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending else { return };
        let now = ctx.input(|i| i.time);
        match pending {
            Pending::Countdown { started, beeps } => {
                let started = if started.is_nan() { now } else { started };
                let elapsed = now - started;
                if elapsed >= 3.0 {
                    self.pending = None;
                    self.start_recording();
                    return;
                }
                let mut beeps = beeps;
                if (beeps as f64) <= elapsed {
                    if let Some(p) = &self.player {
                        p.play(Track::Cue, Arc::new(audio::beep(880.0, 0.09)));
                    }
                    beeps += 1;
                }
                self.pending = Some(Pending::Countdown { started, beeps });
            }
            Pending::Original { ended: None } => {
                if !self.player.as_ref().is_some_and(|p| p.is_playing(Track::Original)) {
                    self.pending = Some(Pending::Original { ended: Some(now) });
                }
            }
            Pending::Original { ended: Some(t) } => {
                // Короткая пауза после оригинала, чтобы успеть вдохнуть.
                if now - t >= 0.35 {
                    self.pending = None;
                    self.start_recording();
                }
            }
        }
    }

    fn match_all_running(&self) -> bool {
        self.match_all.as_ref().is_some_and(|m| m.running())
    }

    fn import_file(&mut self) {
        if self.match_all_running() {
            self.info(tr!("Подожди: подгоняется громкость всех фраз", "Wait: loudness is being matched for all lines"));
            return;
        }
        let Some(file) = rfd::FileDialog::new()
            .set_title(tr!("Выбери файл с фразой", "Pick a file with the line"))
            .add_filter(tr!("Аудио", "Audio"), &project::AUDIO_EXTENSIONS)
            .pick_file()
        else {
            return;
        };
        match audio::decode_file(&file) {
            Ok(samples) => self.store_take(samples, tr!("Импортировано", "Imported")),
            Err(e) => self.error(trf!("Файл не читается: {e:#}", "Can't read the file: {e:#}")),
        }
    }

    fn store_take(&mut self, samples: Vec<f32>, verb: &str) {
        let secs = samples.len() as f32 / audio::RATE as f32;
        if secs < MIN_TAKE_SECS {
            self.error(tr!("Слишком короткая запись — ничего не сохранено", "Recording too short — nothing saved"));
            return;
        }
        let (Some(line), Some(p)) = (self.current_line(), self.project.as_mut()) else { return };
        let mp3 = p.export_mp3(&line);
        let saved = p.archive_current(&line).and_then(|_| encode::save_raw(&p.raw_wav(&line), &samples));
        self.takes = p.takes(&line);
        if let Err(e) = saved {
            self.error(trf!("Не сохранилось: {e:#}", "Not saved: {e:#}"));
            return;
        }
        let len = samples.len();
        self.mine_raw = Some(Arc::new(samples));
        self.fx = Settings { denoise: self.config.denoise_new, ..Settings::default() };
        self.trim = (0, len);
        self.render_full();
        // Тишину ищем уже после шумодава — так границы точнее.
        if self.config.auto_trim
            && let Some(full) = &self.mine_full
        {
            self.trim = audio::detect_voice(&full.samples).unwrap_or((0, len));
            self.render_cut();
        }
        if self.config.auto_level
            && let Some(gain) = self.measure_gain()
        {
            self.fx.gain_db = gain;
            self.render_full();
        }
        let result = self.save_mine().and_then(|_| match self.project.as_mut() {
            Some(p) => p.mark_recorded(&line),
            None => Ok(()),
        });
        let mut done = vec![format!("{verb}: {}", mp3.display())];
        if self.trim != (0, len) {
            done.push(trf!("тишина обрезана, {:.1} из {secs:.1} с", "silence trimmed, {:.1} of {secs:.1} s", index_to_secs(self.trim.1 - self.trim.0)));
        }
        if self.fx.gain_db != 0.0 {
            done.push(trf!("громкость {:+.1} dB", "loudness {:+.1} dB", self.fx.gain_db));
        }
        match result {
            Ok(()) => self.info(done.join(" · ")),
            Err(e) => self.error(trf!("Не сохранилось: {e:#}", "Not saved: {e:#}")),
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
                self.mine_full = None;
                self.mine_raw = None;
                self.info(trf!("{} перенесена в корзину: {}", "{} moved to the trash: {}", line.name, trash_dir.display()));
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if self.project.is_none() || self.screen != Screen::Record || ctx.wants_keyboard_input() {
            return;
        }
        // Забираем клавиши до виджетов, чтобы пробел не «нажимал» кнопку в фокусе.
        // Ctrl+Space проверяем раньше обычного пробела, чтобы его не перехватил Space.
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Space)) {
            self.toggle_play(Track::Mine);
        }
        let pressed = |key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key));
        if pressed(Key::Space) {
            self.toggle_play(Track::Original);
        }
        if pressed(Key::R) {
            self.toggle_record();
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
        if pressed(Key::L) {
            self.match_loudness();
        }
        if pressed(Key::Delete) {
            self.trash_current();
        }
        if pressed(Key::Escape) {
            if self.pending.take().is_some() {
                self.info(tr!("Запись отменена", "Recording cancelled"));
            }
            if let Some(r) = &self.recorder
                && r.is_recording()
            {
                r.cancel();
                self.info(tr!("Запись отменена", "Recording cancelled"));
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
                let text = format!("{}  ·  {}/{}", self.hero_name(&p.hero), p.recorded_count(), p.lines.len());
                let chip = egui::Button::new(RichText::new(text).color(theme::TEXT)).corner_radius(99);
                // На «Героях» чип ведёт обратно к записи этого героя, с остальных вкладок — к списку.
                let (target, hint) = if self.screen == Screen::Heroes {
                    (Screen::Record, tr!("К фразам героя", "To the hero's lines"))
                } else {
                    (Screen::Heroes, tr!("К списку героев", "Back to the hero list"))
                };
                if ui.add(chip).on_hover_text(hint).clicked() {
                    self.screen = target;
                }
            } else if ui.add(theme::primary(trf!("{}  Выбрать героя", "{}  Pick a hero", icon::USERS_THREE))).clicked() {
                self.screen = Screen::Heroes;
            }

            // Вкладки — сегментированный переключатель ровно по центру окна.
            let gap = ui.max_rect().center().x - self.tabs_width / 2.0 - ui.cursor().min.x;
            ui.add_space(gap.max(10.0));
            let tabs = egui::Frame::default().fill(theme::BG).corner_radius(10).inner_margin(egui::Margin::same(3)).show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (screen, label) in [(Screen::Heroes, tr!("Герои", "Heroes")), (Screen::Record, tr!("Запись", "Record")), (Screen::Build, tr!("Сборка и установка", "Build & install"))] {
                    let on = self.screen == screen;
                    let text = RichText::new(label).color(if on { theme::TEXT } else { theme::MUTED });
                    let btn = egui::Button::new(text).fill(if on { theme::CARD } else { egui::Color32::TRANSPARENT }).stroke(egui::Stroke::NONE).corner_radius(8);
                    let enabled = screen == Screen::Heroes || self.project.is_some();
                    if ui.add_enabled(enabled, btn).clicked() && !on {
                        self.screen = screen;
                        if screen == Screen::Build {
                            self.build.refresh_now();
                        }
                    }
                }
            });
            self.tabs_width = tabs.response.rect.width();

            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let on = self.screen == Screen::Settings;
                let gear = egui::Button::new(RichText::new(icon::GEAR_SIX).size(18.0).color(if on { GOLD } else { theme::MUTED }))
                    .fill(if on { theme::CARD } else { egui::Color32::TRANSPARENT })
                    .stroke(egui::Stroke::NONE)
                    .corner_radius(8);
                if ui.add(gear).on_hover_text(tr!("Настройки", "Settings")).clicked() {
                    self.screen = match (on, self.project.is_some()) {
                        (false, _) => Screen::Settings,
                        (true, true) => Screen::Record,
                        (true, false) => Screen::Heroes,
                    };
                }
                if let Some(d) = &self.dota {
                    let text = trf!("Озвучка: {}", "Voice-over: {}", game::language_name(d.voice_language.as_deref()));
                    ui.label(theme::small(text));
                }
            });
        });
    }

    /// В фоне спрашивает у GitHub, нет ли новой версии. Без сети просто молчит.
    fn start_update_check(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            if let Ok(release) = update::check() {
                let _ = tx.send(release);
                ctx.request_repaint();
            }
        });
        self.update_rx = Some(rx);
    }

    fn update_banner(&mut self, ui: &mut egui::Ui) {
        let Some(release) = self.update.clone() else { return };
        ui.horizontal(|ui| {
            ui.label(RichText::new(icon::SPARKLE).size(18.0).color(GOLD));
            ui.label(RichText::new(trf!(
                "Вышла новая версия {} (у тебя {}). Скачай её со страницы релиза и замени старую программу.", "Version {} is out (you have {}). Download it from the release page and replace the old app.",
                release.version,
                env!("CARGO_PKG_VERSION")
            )).color(theme::TEXT));
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if theme::ghost(ui, true, icon::X).on_hover_text(tr!("Скрыть до следующего запуска", "Hide until next launch")).clicked() {
                    self.update = None;
                }
                if ui.add(theme::primary(trf!("{}  Скачать", "{}  Download", icon::DOWNLOAD_SIMPLE))).clicked() {
                    open_url(&release.url);
                }
            });
        });
    }

    /// Перечитывает состояние игры: язык озвучки, на месте ли пак Valve.
    fn check_game(&mut self) {
        if self.game_checked.is_some_and(|t| t.elapsed() < GAME_CHECK) {
            return;
        }
        self.game_checked = Some(Instant::now());
        self.dota = find_dota(&self.config);
        self.alert = self.dota.as_ref().and_then(install::check);
    }

    /// Предупреждение под верхней панелью — видно на обеих вкладках.
    fn alert_banner(&mut self, ui: &mut egui::Ui, alert: install::Alert) {
        let (game_lang, voice_lang) = match &self.dota {
            Some(d) => (game::language_name(d.audio_language.as_deref()), game::language_name(d.voice_language.as_deref())),
            None => Default::default(),
        };
        let (text, action) = match alert {
            install::Alert::ValveRestored => (
                tr!("Steam вернул озвучку Valve — так бывает после обновления Доты или проверки файлов. Сейчас в игре звучат не твои фразы.", "Steam restored Valve's voice-over — this happens after a Dota update or a file check. Your lines aren't playing in the game right now.").to_owned(),
                Some((tr!("Переустановить", "Reinstall"), tr!("Пересобрать и установить пак заново", "Rebuild and reinstall the pack"), Screen::Build)),
            ),
            install::Alert::EnglishAudio => (
                tr!("В Доте выбрана английская озвучка — моды из папки языка не загружаются. Переключи язык озвучки игры на русский и перезапусти её.", "Dota is set to English voice-over — mods from a language folder don't load. Switch the game's voice-over language (e.g. to Russian) and restart it.").to_owned(),
                None,
            ),
            install::Alert::OtherAudio => (
                trf!(
                    "В Доте включена {game_lang} озвучка, а мод собирается для озвучки «{voice_lang}» — в игре его не будет слышно. Переключи язык озвучки игры или выбери другую озвучку в настройках.", "Dota uses {game_lang} voice-over, but the mod is built for {voice_lang} voice-over — you won't hear it in the game. Switch the game's voice-over language or pick another one in the settings."),
                Some((tr!("Настройки", "Settings"), tr!("Выбрать, какую озвучку переделываем", "Choose which voice-over to remake"), Screen::Settings)),
            ),
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(icon::WARNING).size(18.0).color(theme::AMBER));
            ui.add(egui::Label::new(RichText::new(text).color(theme::TEXT)).wrap());
            if let Some((label, hint, screen)) = action {
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(theme::primary(label)).on_hover_text(hint).clicked() {
                        self.screen = screen;
                        self.build.refresh_now();
                    }
                });
            }
        });
    }

    /// Сообщение о последнем действии — плашка снизу по центру. Обычное гаснет само
    /// (чем длиннее текст, тем позже), ошибка висит, пока её не закроют.
    fn toast(&mut self, ctx: &egui::Context) {
        let Some(message) = &self.message else { return };
        let (text, is_error) = match message {
            Message::Info(t) => (t.clone(), false),
            Message::Error(t) => (t.clone(), true),
        };
        let age = self.message_at.map_or(0.0, |t| t.elapsed().as_secs_f32());
        let life = TOAST_SECS + text.chars().count() as f32 / 40.0;
        if !is_error && age > life {
            self.message = None;
            return;
        }
        let mut close = false;
        egui::Area::new(egui::Id::new("toast"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -18.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                // Последние полсекунды плавно гаснет.
                if !is_error {
                    ui.multiply_opacity(((life - age) / 0.5).clamp(0.0, 1.0));
                }
                egui::Frame::default()
                    .fill(PANEL)
                    .stroke(egui::Stroke::new(1.0_f32, if is_error { RED.gamma_multiply(0.7) } else { theme::LINE }))
                    .corner_radius(10)
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .shadow(egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: egui::Color32::from_black_alpha(120) })
                    .show(ui, |ui| {
                        ui.set_max_width(640.0);
                        ui.horizontal(|ui| {
                            let (glyph, color) = if is_error { (icon::WARNING, RED) } else { (icon::CHECK_CIRCLE, theme::GREEN) };
                            ui.label(RichText::new(glyph).size(16.0).color(color));
                            ui.add(egui::Label::new(RichText::new(text).size(13.0).color(theme::TEXT)).wrap());
                            if theme::ghost(ui, true, icon::X).on_hover_text(tr!("Закрыть", "Close")).clicked() {
                                close = true;
                            }
                        });
                    });
            });
        if close {
            self.message = None;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.check_game();
        if let Some(rx) = &self.update_rx
            && let Ok(release) = rx.try_recv()
        {
            self.update.clone_from(&release);
            self.latest = Some(release);
            self.update_rx = None;
        }
        self.handle_keys(ctx);
        self.tick_pending(ctx);
        self.poll_build_tasks();
        self.poll_match_all();

        if !self.config.setup_done {
            egui::CentralPanel::default()
                .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(22, 28)))
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.setup_screen(ui));
                });
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        self.poll_heroes(ctx);

        let bar_frame = egui::Frame::side_top_panel(&ctx.style()).fill(PANEL).inner_margin(egui::Margin::symmetric(14, 10));
        egui::TopBottomPanel::top("top").frame(bar_frame).show(ctx, |ui| self.top_bar(ui));
        if let Some(alert) = self.alert {
            let frame = egui::Frame::default()
                .fill(theme::AMBER.gamma_multiply(0.14))
                .stroke(egui::Stroke::new(1.0_f32, theme::AMBER.gamma_multiply(0.5)))
                .inner_margin(egui::Margin::symmetric(16, 8));
            egui::TopBottomPanel::top("alert").frame(frame).show(ctx, |ui| self.alert_banner(ui, alert));
        }
        if self.update.is_some() {
            let frame = egui::Frame::default()
                .fill(GOLD.gamma_multiply(0.10))
                .stroke(egui::Stroke::new(1.0_f32, GOLD.gamma_multiply(0.4)))
                .inner_margin(egui::Margin::symmetric(16, 8));
            egui::TopBottomPanel::top("update").frame(frame).show(ctx, |ui| self.update_banner(ui));
        }
        match self.screen {
            Screen::Record => {
                egui::SidePanel::left("lines")
                    .resizable(true)
                    .default_width(250.0)
                    .frame(egui::Frame::side_top_panel(&ctx.style()).fill(PANEL).inner_margin(egui::Margin::same(10)))
                    .show(ctx, |ui| self.line_list(ui));
                egui::CentralPanel::default()
                    .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(22, 16)))
                    .show(ctx, |ui| {
                        // На маленьком экране правая часть прокручивается, а не обрезается.
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.editor(ui));
                    });
            }
            Screen::Heroes => {
                egui::CentralPanel::default()
                    .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(22, 18)))
                    .show(ctx, |ui| self.heroes_screen(ui));
            }
            Screen::Build | Screen::Settings => {
                egui::CentralPanel::default()
                    .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(22, 18)))
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            if self.screen == Screen::Build {
                                self.build_screen(ui);
                            } else {
                                self.settings_screen(ui);
                            }
                        });
                    });
            }
        }

        self.toast(ctx);

        // Анимация волны, индикатора и кнопки записи; в покое — редкая перерисовка.
        let busy = self.player.as_ref().is_some_and(|p| p.now_playing().is_some())
            || self.is_recording()
            || self.pending.is_some()
            || self.build.busy()
            || self.match_all_running()
            || self.heroes.busy();
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

/// `bount_kill_01` → `bount`: в списке показываем имя без префикса героя.
fn prefix_of(name: &str) -> &str {
    name.split_once('_').map_or("", |(p, _)| p)
}

/// Дота по настройкам: своя папка игры и выбранный язык озвучки.
fn find_dota(config: &Config) -> Option<DotaInstall> {
    game::find_dota(config.dota_dir.as_deref(), config.voice_language.as_deref())
}

fn dota_status(dota: Option<&DotaInstall>) -> String {
    let Some(d) = dota else { return tr!("Дота не найдена", "Dota not found").into() };
    if d.voice_language.is_none() {
        return tr!("Озвучка Доты английская — для модов переключи язык озвучки", "Dota's voice-over is English — switch the voice-over language for mods").into();
    }
    let lang = game::language_name(d.voice_language.as_deref());
    match (d.voice_pak(), install::mod_state(d)) {
        (None, _) => trf!("Дота · озвучка {lang} · пак озвучки не скачан", "Dota · {lang} voice-over · voice pack not downloaded"),
        (Some(v), install::ModState::Enabled) if !v.enabled => trf!("Дота · озвучка {lang} · работает наш пак", "Dota · {lang} voice-over · our pack is active"),
        (Some(_), install::ModState::Disabled) => trf!("Дота · озвучка {lang} · мод выключен", "Dota · {lang} voice-over · mod off"),
        (Some(v), install::ModState::Legacy) if !v.enabled => trf!("Дота · озвучка {lang} · работает пак прежней версии", "Dota · {lang} voice-over · the previous version's pack is active"),
        (Some(_), _) => trf!("Дота · озвучка {lang} · пак Valve включён", "Dota · {lang} voice-over · Valve pack on"),
    }
}

fn open_url(url: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).creation_flags(CREATE_NO_WINDOW).spawn();
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Открывает папку в проводнике (создав её, если ещё нет).
fn open_folder(path: &Path) {
    let _ = std::fs::create_dir_all(path);
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

fn reveal(path: &Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
}
