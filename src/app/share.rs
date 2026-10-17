//! Окна обмена пачками: экспорт с выбором героев и импорт с предпросмотром —
//! что новое, что совпадает, что обновилось и где конфликты (их можно послушать).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use dota_revoice::audio::{self, Track};
use dota_revoice::exchange::{self, Decision, Incoming, Journal, Kind, Preview};
use dota_revoice::vpk::VpkReader;
use dota_revoice::{install, tr, trf};
use eframe::egui::{self, Align, Layout, RichText, vec2};

use super::App;
use super::settings::choice_chip;
use super::theme::{self, AMBER, FAINT, GREEN, MUTED, RED, TEXT, icon};

#[derive(Default)]
pub(super) struct ShareState {
    export: Option<ExportDialog>,
    import: Option<ImportDialog>,
    /// Пачка читается и сравнивается с моими фразами в фоне.
    reading: Option<mpsc::Receiver<Result<(PathBuf, Preview), String>>>,
    /// Последний импорт (его можно отменить); `None` внутри — импортов не было.
    last: Option<Option<Journal>>,
}

impl ShareState {
    pub(super) fn busy(&self) -> bool {
        self.reading.is_some()
    }
}

struct ExportDialog {
    /// Папка героя (`sounds/vo/axe`), сколько записано, отмечен ли.
    groups: Vec<(String, usize, bool)>,
    /// Положить исходники (wav, обрезку, обработку) — друг сможет править без потерь.
    sources: bool,
    error: Option<String>,
}

/// Что делать с конфликтами — для всех сразу или по одной.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Policy {
    KeepMine,
    Take,
    Each,
}

struct ImportDialog {
    path: PathBuf,
    preview: Preview,
    /// Снятые галочкой герои.
    off: BTreeSet<String>,
    /// Брать ли фразы, которых нет в игре.
    unknown: bool,
    policy: Policy,
    /// Решения «по одной»: путь фразы → взять из пачки или оставить мою.
    picks: BTreeMap<String, Decision>,
    error: Option<String>,
}

impl ImportDialog {
    fn decision(&self, line: &Incoming) -> Decision {
        if line.kind == Kind::Unknown {
            return if self.unknown { Decision::Take } else { Decision::Skip };
        }
        if self.off.contains(exchange::group_of(&line.res)) {
            return Decision::Skip;
        }
        let conflicts = match self.policy {
            Policy::KeepMine => Decision::KeepMine,
            Policy::Take => Decision::Take,
            Policy::Each => self.picks.get(&line.res).copied().unwrap_or(Decision::KeepMine),
        };
        exchange::default_decision(line.kind, conflicts)
    }
}

/// Что нажали в окне импорта (окно держим отдельно от `App`, а действия выполняем после).
enum ImportAction {
    Play(Vec<f32>),
    Apply,
    Close,
}

impl App {
    /// Окно экспорта; `only` — папка героя, если пришли из его карточки.
    pub(super) fn open_export(&mut self, only: Option<&str>) {
        let groups = match exchange::recorded_groups(&self.config.export_root()) {
            Ok(g) => g,
            Err(e) => return self.error(format!("{e:#}")),
        };
        if groups.is_empty() {
            return self.info(tr!("Пока нечего отправлять — нет ни одной записанной фразы", "Nothing to send yet — no recorded lines"));
        }
        let groups = groups.into_iter().map(|(g, n)| (only.is_none_or(|o| o == g), g, n)).map(|(on, g, n)| (g, n, on)).collect();
        self.share.export = Some(ExportDialog { groups, sources: false, error: None });
    }

    pub(super) fn pick_import(&mut self, ctx: &egui::Context) {
        let file = rfd::FileDialog::new()
            .set_title(tr!("Пачка фраз от друга", "Line bundle from a friend"))
            .add_filter(tr!("Пачка фраз", "Line bundle"), &["zip"])
            .pick_file();
        if let Some(file) = file {
            self.start_import(file, ctx);
        }
    }

    /// Пачку бросили в окно — открываем как через «Импорт пачки…».
    pub(super) fn handle_dropped(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect::<Vec<_>>());
        if let Some(zip) = dropped.into_iter().find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"))) {
            self.start_import(zip, ctx);
        }
    }

    /// Читает пачку в фоне: заодно открывает паки игры, чтобы узнать фразы «не из игры».
    fn start_import(&mut self, path: PathBuf, ctx: &egui::Context) {
        let root = self.config.export_root();
        let paks: Vec<PathBuf> = self
            .dota
            .as_ref()
            .map(|d| [install::valve_index(d), Some(d.game_dir.join("dota").join("pak01_dir.vpk"))].into_iter().flatten().collect())
            .unwrap_or_default();
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let readers: Vec<VpkReader> = paks.iter().filter_map(|p| VpkReader::open(p).ok()).collect();
            // Без игры проверить нечем — считаем, что всё из игры.
            let in_game = |res: &str| readers.is_empty() || readers.iter().any(|r| r.contains(&format!("{res}.vsnd_c")));
            let result = exchange::preview(&root, &path, &in_game).map(|p| (path, p)).map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.share.import = None;
        self.share.reading = Some(rx);
    }

    /// Каждый кадр: забрать прочитанную пачку и нарисовать открытые окна.
    pub(super) fn share_windows(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.share.reading
            && let Ok(result) = rx.try_recv()
        {
            self.share.reading = None;
            match result {
                Ok((path, preview)) => {
                    let off = BTreeSet::new();
                    self.share.import = Some(ImportDialog { path, preview, off, unknown: false, policy: Policy::KeepMine, picks: BTreeMap::new(), error: None });
                }
                Err(e) => self.error(trf!("Пачка не читается: {e}", "Can't read the bundle: {e}")),
            }
        }
        if self.share.reading.is_some() {
            egui::Modal::new(egui::Id::new("share_reading")).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr!("Читаю пачку и сравниваю с твоими фразами…", "Reading the bundle and comparing it with your lines…"));
                });
            });
        }
        self.export_window(ctx);
        self.import_window(ctx);
    }

    fn export_window(&mut self, ctx: &egui::Context) {
        let Some(mut dlg) = self.share.export.take() else { return };
        let mut save = false;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("share_export")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.label(theme::title(tr!("Экспорт пачки", "Export bundle"), 18.0));
            ui.label(theme::small(tr!("Один zip-файл для друга: твои фразы и заметки к ним", "One zip file for a friend: your lines and their notes")));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr!("Подпись", "Signed as")).color(MUTED));
                ui.add(egui::TextEdit::singleline(&mut self.config.author).hint_text(tr!("твоё имя или ник", "your name or nickname")).desired_width(200.0));
                let shown = if self.config.author.trim().is_empty() { tr!("без подписи", "unsigned").to_owned() } else { trf!("у друга: «от {}»", "your friend sees “from {}”", self.config.author.trim()) };
                ui.label(theme::small(shown));
            });
            ui.add_space(8.0);
            theme::card(ui, |ui| {
                egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                    for (group, n, on) in &mut dlg.groups {
                        ui.horizontal(|ui| {
                            if theme::chip(ui, *on, &self.group_name(group)).clicked() {
                                *on = !*on;
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(theme::small(trf!("записано {n}", "recorded {n}")));
                            });
                        });
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if theme::ghost(ui, true, tr!("выбрать всех", "select all")).clicked() {
                        dlg.groups.iter_mut().for_each(|g| g.2 = true);
                    }
                    if theme::ghost(ui, true, tr!("снять всех", "clear all")).clicked() {
                        dlg.groups.iter_mut().for_each(|g| g.2 = false);
                    }
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if theme::chip(ui, dlg.sources, tr!("с исходниками для правок", "with sources for editing")).clicked() {
                    dlg.sources = !dlg.sources;
                }
                ui.label(theme::small(tr!("wav, обрезка и громкость — пачка тяжелее", "wav, trim and loudness — a heavier bundle")));
            });
            let selected: usize = dlg.groups.iter().filter(|g| g.2).map(|g| g.1).sum();
            if let Some(e) = &dlg.error {
                ui.label(RichText::new(e.as_str()).size(12.0).color(RED));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(theme::small(trf!("в пачке фраз: {selected}", "lines in the bundle: {selected}")));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add_enabled(selected > 0, theme::primary(trf!("{}  Сохранить пачку…", "{}  Save bundle…", icon::EXPORT))).clicked() {
                        save = true;
                    }
                    if theme::ghost(ui, true, tr!("Отмена", "Cancel")).clicked() {
                        close = true;
                    }
                });
            });
        });
        if save {
            let groups: Vec<&String> = dlg.groups.iter().filter(|g| g.2).map(|g| &g.0).collect();
            let name = self.pack_file_name(&groups);
            let out = rfd::FileDialog::new()
                .set_title(tr!("Куда сохранить пачку фраз", "Where to save the line bundle"))
                .set_file_name(&name)
                .add_filter(tr!("Пачка фраз", "Line bundle"), &["zip"])
                .save_file();
            if let Some(out) = out {
                let only: BTreeSet<String> = groups.into_iter().cloned().collect();
                self.config.save();
                match exchange::export_pack(&self.config.export_root(), &out, &self.config.author, Some(&only), dlg.sources) {
                    Ok(n) => {
                        self.info(trf!("В пачке {n} фраз: {}", "{n} lines in the bundle: {}", out.display()));
                        return;
                    }
                    Err(e) => dlg.error = Some(format!("{e:#}")),
                }
            }
        }
        if !(close || modal.should_close()) {
            self.share.export = Some(dlg);
        }
    }

    fn import_window(&mut self, ctx: &egui::Context) {
        let Some(mut dlg) = self.share.import.take() else { return };
        let mut action = None;
        let modal = egui::Modal::new(egui::Id::new("share_import")).show(ctx, |ui| {
            ui.set_width(620.0);
            action = self.import_contents(ui, &mut dlg);
        });
        match action {
            Some(ImportAction::Play(samples)) => {
                if let Some(p) = &self.player {
                    p.play(Track::Cue, Arc::new(samples));
                }
            }
            Some(ImportAction::Apply) => return self.apply_import(dlg),
            Some(ImportAction::Close) => return,
            None => {}
        }
        if modal.should_close() {
            return;
        }
        self.share.import = Some(dlg);
    }

    fn import_contents(&self, ui: &mut egui::Ui, dlg: &mut ImportDialog) -> Option<ImportAction> {
        let mut action = None;
        let p = &dlg.preview;
        let author = if p.author.is_empty() { tr!("без подписи", "unsigned").to_owned() } else { p.author.clone() };
        let file = dlg.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        ui.label(theme::title(tr!("Импорт пачки", "Import bundle"), 18.0));
        ui.label(theme::small(trf!(
            "{author} · {} · {file} · фраз: {}",
            "{author} · {} · {file} · lines: {}",
            date_of(p.created_unix),
            p.lines.len()
        )));
        if p.with_sources() > 0 {
            ui.label(theme::small(trf!(
                "С исходниками для правок: {} — обрезку и громкость можно будет поменять без потерь",
                "With sources for editing: {} — trim and loudness can be changed without loss",
                p.with_sources()
            )));
        }
        if p.newer_format {
            ui.label(RichText::new(tr!("Пачка из более новой версии программы — обнови её, если что-то не так", "The bundle is from a newer app version — update the app if something looks off")).size(12.0).color(AMBER));
        }
        ui.add_space(8.0);

        // Сводка по героям: сколько каких фраз.
        let mut groups: BTreeMap<&str, [usize; 4]> = BTreeMap::new();
        for line in p.lines.iter().filter(|l| l.kind != Kind::Unknown) {
            let i = match line.kind {
                Kind::New => 0,
                Kind::Update => 1,
                Kind::Same => 2,
                _ => 3,
            };
            groups.entry(exchange::group_of(&line.res)).or_default()[i] += 1;
        }
        let unknown: Vec<&str> = p.lines.iter().filter(|l| l.kind == Kind::Unknown).map(|l| l.res.as_str()).collect();
        theme::card(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("import_groups").max_height(220.0).show(ui, |ui| {
                for (group, [new, upd, same, conf]) in &groups {
                    let on = !dlg.off.contains(*group);
                    ui.horizontal(|ui| {
                        ui.allocate_ui_with_layout(vec2(190.0, 24.0), Layout::left_to_right(Align::Center), |ui| {
                            ui.set_min_width(190.0);
                            if theme::chip(ui, on, &self.group_name(group)).clicked() {
                                if on {
                                    dlg.off.insert((*group).to_owned());
                                } else {
                                    dlg.off.remove(*group);
                                }
                            }
                        });
                        for (n, text, color) in [
                            (*new, tr!("новых", "new"), GREEN),
                            (*upd, tr!("обновлений", "updates"), GREEN),
                            (*same, tr!("совпадает", "same"), MUTED),
                            (*conf, tr!("конфликтов", "conflicts"), AMBER),
                        ] {
                            if n > 0 {
                                theme::pill(ui, &format!("{text} {n}"), if on { color } else { FAINT });
                            }
                        }
                    });
                }
                if !unknown.is_empty() {
                    ui.horizontal(|ui| {
                        if theme::chip(ui, dlg.unknown, tr!("Не из игры", "Not in the game")).on_hover_text(unknown.join("\n")).clicked() {
                            dlg.unknown = !dlg.unknown;
                        }
                        ui.label(theme::small(trf!(
                            "{} — таких фраз нет в игре: опечатка в имени или звук из другого мода",
                            "{} — the game has no such lines: a typo in the name or a sound from another mod",
                            unknown.len()
                        )));
                    });
                }
            });
            if !p.rejected.is_empty() {
                ui.label(theme::small(trf!("Пропущены файлы, не похожие на фразы: {}", "Skipped files that aren't lines: {}", p.rejected.len())));
            }
        });

        // Конфликты у отмеченных героев.
        let conflicts: Vec<&Incoming> = p.lines.iter().filter(|l| l.kind == Kind::Conflict && !dlg.off.contains(exchange::group_of(&l.res))).collect();
        if !conflicts.is_empty() {
            ui.add_space(8.0);
            theme::card(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(trf!("Конфликты ({}):", "Conflicts ({}):", conflicts.len())).color(TEXT));
                    for (policy, text) in [
                        (Policy::KeepMine, tr!("оставить мои", "keep mine")),
                        (Policy::Take, tr!("взять из пачки", "take from the bundle")),
                        (Policy::Each, tr!("решу по одной", "decide one by one")),
                    ] {
                        if choice_chip(ui, dlg.policy == policy, true, text).clicked() {
                            dlg.policy = policy;
                        }
                    }
                });
                if dlg.policy == Policy::Each {
                    let mine_root = self.config.export_root();
                    egui::ScrollArea::vertical().id_salt("import_conflicts").max_height(220.0).show(ui, |ui| {
                        for line in &conflicts {
                            ui.horizontal(|ui| {
                                let name = line.res.rsplit('/').next().unwrap_or(&line.res);
                                ui.allocate_ui_with_layout(vec2(170.0, 24.0), Layout::left_to_right(Align::Center), |ui| {
                                    ui.set_min_width(170.0);
                                    ui.add(egui::Label::new(RichText::new(name).monospace().size(12.0).color(TEXT)).truncate());
                                });
                                let mine = || audio::decode_file(&mine_root.join(format!("{}.mp3", line.res))).ok();
                                let theirs = || exchange::samples_of(line).ok();
                                if theme::ghost(ui, true, trf!("{}  моя", "{}  mine", icon::PLAY)).clicked() {
                                    action = mine().map(ImportAction::Play);
                                }
                                if theme::ghost(ui, true, format!("{}  {}", icon::PLAY, short_author(&line.author))).clicked() {
                                    action = theirs().map(ImportAction::Play);
                                }
                                if theme::ghost(ui, true, icon::ARROWS_LEFT_RIGHT).on_hover_text(tr!("Сначала моя, потом из пачки", "Mine first, then the bundle's")).clicked() {
                                    action = mine().zip(theirs()).map(|(mut a, b)| {
                                        a.resize(a.len() + audio::RATE as usize * 2 / 5, 0.0);
                                        a.extend(b);
                                        ImportAction::Play(a)
                                    });
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let pick = dlg.picks.get(&line.res).copied().unwrap_or(Decision::KeepMine);
                                    if choice_chip(ui, pick == Decision::Take, true, &short_author(&line.author)).clicked() {
                                        dlg.picks.insert(line.res.clone(), Decision::Take);
                                    }
                                    if choice_chip(ui, pick == Decision::KeepMine, true, tr!("моя", "mine")).clicked() {
                                        dlg.picks.insert(line.res.clone(), Decision::KeepMine);
                                    }
                                });
                            });
                        }
                    });
                }
                ui.label(theme::small(tr!("Невыбранная версия уходит в дубли — её можно послушать и вернуть.", "The version you don't pick goes to takes — you can listen to it and bring it back.")));
            });
        }

        if let Some(e) = &dlg.error {
            ui.label(RichText::new(e.as_str()).size(12.0).color(RED));
        }
        let count = p.lines.iter().filter(|l| dlg.decision(l) != Decision::Skip).count();
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = trf!("Импортировать фраз: {count}", "Import lines: {count}");
                if ui.add_enabled(count > 0, theme::primary(label)).clicked() {
                    action = Some(ImportAction::Apply);
                }
                if theme::ghost(ui, true, tr!("Отмена", "Cancel")).clicked() {
                    action = Some(ImportAction::Close);
                }
            });
        });
        action
    }

    fn apply_import(&mut self, mut dlg: ImportDialog) {
        if let Some(p) = &self.player {
            p.stop();
        }
        let root = self.config.export_root();
        match exchange::apply(&root, &dlg.preview, |l| dlg.decision(l)) {
            Ok(s) => {
                let mut parts = vec![trf!("Импорт: новых {}", "Import: {} new", s.added)];
                if s.replaced > 0 {
                    parts.push(trf!("заменено {} (прежние — в дублях)", "{} replaced (previous ones are in takes)", s.replaced));
                }
                if s.to_takes > 0 {
                    parts.push(trf!("в дубли рядом с твоими: {}", "put next to yours in takes: {}", s.to_takes));
                }
                parts.push(tr!("пересобери и установи пак", "rebuild and install the pack").to_owned());
                self.info(parts.join(" · "));
            }
            Err(e) => {
                dlg.error = Some(format!("{e:#}"));
                self.share.import = Some(dlg);
                return;
            }
        }
        self.after_import_change();
    }

    /// Фразы поменялись (импорт или его отмена): перечитать героя, счётчики и журнал.
    fn after_import_change(&mut self) {
        if let Some(p) = self.project.as_mut() {
            p.rescan();
            let selected = self.selected;
            self.select(selected);
        }
        self.heroes.refresh_counts();
        self.build.refresh_now();
        self.share.last = None;
    }

    /// Последний импорт — для «Отменить импорт» (читается с диска один раз).
    pub(super) fn last_import(&mut self) -> Option<&Journal> {
        if self.share.last.is_none() {
            self.share.last = Some(exchange::last_import(&self.config.export_root()));
        }
        self.share.last.as_ref().and_then(Option::as_ref)
    }

    pub(super) fn undo_import(&mut self) {
        if let Some(p) = &self.player {
            p.stop();
        }
        match exchange::undo_import(&self.config.export_root()) {
            Ok(u) if u.kept > 0 => self.info(trf!(
                "Импорт отменён: вернулось {} · {} фраз уже поменяли после импорта — их не трогал",
                "Import undone: {} reverted · {} lines were changed after the import — left as they are",
                u.reverted,
                u.kept
            )),
            Ok(u) => self.info(trf!("Импорт отменён: вернулось фраз — {}", "Import undone: lines reverted — {}", u.reverted)),
            Err(e) => self.error(format!("{e:#}")),
        }
        self.after_import_change();
    }

    /// Имя группы для людей: герой по имени, иначе сам путь.
    fn group_name(&self, group: &str) -> String {
        match group.strip_prefix("sounds/vo/") {
            Some(vo) if !vo.contains('/') => self.hero_name(vo),
            _ => group.to_owned(),
        }
    }

    /// `Вася_Axe+Bounty_Hunter_2026-10-17.zip`.
    fn pack_file_name(&self, groups: &[&String]) -> String {
        let mut parts = Vec::new();
        if !self.config.author.trim().is_empty() {
            parts.push(self.config.author.trim().to_owned());
        }
        let mut heroes: Vec<String> = groups.iter().take(3).map(|g| self.group_name(g)).collect();
        if groups.len() > 3 {
            heroes.push(format!("{}", groups.len() - 3));
        }
        parts.push(heroes.join("+"));
        parts.push(date_of(now_unix()));
        let name: String = parts.join("_").chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '+' | '_') { c } else { '_' }).collect();
        format!("{name}.zip")
    }
}

/// Подпись в кнопке: длинное имя обрезаем; без подписи — «из пачки».
pub(super) fn short_author(author: &str) -> String {
    match author.trim() {
        "" => tr!("из пачки", "bundle's").to_owned(),
        a if a.chars().count() > 12 => format!("{}…", a.chars().take(11).collect::<String>()),
        a => a.to_owned(),
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `2026-10-17` по времени в секундах (UTC; для имени файла и подписи пачки хватает).
pub(super) fn date_of(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    // Перевод дней в дату (алгоритм Говарда Хиннанта).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates() {
        assert_eq!(super::date_of(0), "1970-01-01");
        assert_eq!(super::date_of(1_792_195_200), "2026-10-17");
        assert_eq!(super::date_of(951_782_400), "2000-02-29");
    }
}
