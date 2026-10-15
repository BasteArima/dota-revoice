//! Тема «Dota, золото»: палитра, шрифты, иконки и общие виджеты.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Rect, RichText, Stroke, TextStyle,
    Theme, pos2, vec2,
};
pub use egui_phosphor::regular as icon;

pub const BG: Color32 = Color32::from_rgb(18, 22, 28);
pub const BG_DEEP: Color32 = Color32::from_rgb(13, 16, 21);
pub const PANEL: Color32 = Color32::from_rgb(24, 29, 37);
pub const CARD: Color32 = Color32::from_rgb(31, 37, 48);
pub const CARD_HOVER: Color32 = Color32::from_rgb(40, 47, 61);
pub const LINE: Color32 = Color32::from_rgb(43, 51, 64);
pub const TEXT: Color32 = Color32::from_rgb(232, 235, 241);
pub const MUTED: Color32 = Color32::from_rgb(139, 149, 167);
pub const FAINT: Color32 = Color32::from_rgb(92, 102, 120);
pub const GOLD: Color32 = Color32::from_rgb(212, 166, 72);
pub const ON_GOLD: Color32 = Color32::from_rgb(26, 20, 5);
pub const RED: Color32 = Color32::from_rgb(224, 72, 62);
pub const GREEN: Color32 = Color32::from_rgb(92, 192, 112);
pub const AMBER: Color32 = Color32::from_rgb(230, 162, 60);
pub const WAVE: Color32 = Color32::from_rgb(89, 98, 115);

/// Семейство для заголовков (полужирный системный шрифт, если есть).
pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

pub fn setup(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_theme(Theme::Dark);
    ctx.style_mut_of(Theme::Dark, |s| {
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(12.0, 6.0);
        s.spacing.interact_size.y = 28.0;
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Heading, FontId::new(20.0, bold())),
            (TextStyle::Monospace, FontId::monospace(13.0)),
        ]
        .into();

        let v = &mut s.visuals;
        v.dark_mode = true;
        v.panel_fill = BG;
        v.window_fill = PANEL;
        v.window_stroke = Stroke::new(1.0_f32, LINE);
        v.window_corner_radius = CornerRadius::same(12);
        v.menu_corner_radius = CornerRadius::same(8);
        v.extreme_bg_color = BG_DEEP;
        v.faint_bg_color = CARD;
        v.code_bg_color = BG_DEEP;
        v.hyperlink_color = GOLD;
        v.selection.bg_fill = GOLD;
        v.selection.stroke = Stroke::new(1.0_f32, ON_GOLD);

        let w = &mut v.widgets;
        let base = |fill: Color32, stroke: Color32, text: Color32| egui::style::WidgetVisuals {
            bg_fill: fill,
            weak_bg_fill: fill,
            bg_stroke: Stroke::new(1.0_f32, stroke),
            corner_radius: CornerRadius::same(8),
            fg_stroke: Stroke::new(1.0_f32, text),
            expansion: 0.0,
        };
        w.noninteractive = base(CARD, LINE, TEXT);
        w.inactive = base(CARD, LINE, TEXT);
        w.hovered = base(CARD_HOVER, Color32::from_rgb(64, 74, 92), TEXT);
        w.active = base(Color32::from_rgb(48, 56, 72), GOLD, TEXT);
        w.open = base(CARD_HOVER, LINE, TEXT);
    });
}

fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);

    // Системный шрифт выглядит родным и умеет кириллицу; встроенные шрифты egui остаются запасными.
    let load = |name: &str, paths: &[&str], fonts: &mut FontDefinitions| {
        let bytes = paths.iter().find_map(|p| std::fs::read(p).ok())?;
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_owned(bytes)));
        Some(())
    };
    let regular = load("ui", &["C:\\Windows\\Fonts\\segoeui.ttf", "/System/Library/Fonts/Supplemental/Arial.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"], &mut fonts);
    let semibold = load("ui-bold", &["C:\\Windows\\Fonts\\seguisb.ttf", "/System/Library/Fonts/Supplemental/Arial Bold.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"], &mut fonts);
    let mono = load("ui-mono", &["C:\\Windows\\Fonts\\consola.ttf"], &mut fonts);

    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    if regular.is_some() {
        proportional.insert(0, "ui".into());
    }
    let mut bold_list = proportional.clone();
    if semibold.is_some() {
        bold_list.insert(0, "ui-bold".into());
    }
    fonts.families.insert(bold(), bold_list);
    if mono.is_some() {
        fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "ui-mono".into());
    }
    fonts
}

/// Карточка с рамкой.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    card_with(ui, LINE, add)
}

pub fn card_with<R>(ui: &mut egui::Ui, border: Color32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::default()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, border))
        .corner_radius(12)
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Плашка-статус: цветной текст на полупрозрачном фоне того же цвета.
pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::default()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(99)
        .inner_margin(Margin::symmetric(9, 2))
        .show(ui, |ui| ui.label(RichText::new(text).size(12.0).color(color)));
}

/// Основная кнопка: золотая заливка.
pub fn primary(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).color(ON_GOLD).strong()).fill(GOLD).stroke(Stroke::NONE)
}

/// Тихая кнопка без рамки: при наведении подсвечивается фон и текст.
pub fn ghost(ui: &mut egui::Ui, enabled: bool, text: impl Into<String>) -> egui::Response {
    ghost_colored(ui, enabled, text, MUTED)
}

pub fn ghost_colored(ui: &mut egui::Ui, enabled: bool, text: impl Into<String>, color: Color32) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| {
        let galley = egui::WidgetText::from(RichText::new(text.into()).size(13.0)).into_galley(
            ui,
            Some(egui::TextWrapMode::Extend),
            f32::INFINITY,
            TextStyle::Button,
        );
        let pad = vec2(8.0, 5.0);
        let (rect, resp) = ui.allocate_exact_size(galley.size() + pad * 2.0, egui::Sense::click());
        if ui.is_rect_visible(rect) {
            let hovered = resp.hovered() && enabled;
            if hovered {
                ui.painter().rect_filled(rect, 7.0, CARD_HOVER);
            }
            let c = if !enabled { FAINT } else if hovered { TEXT } else { color };
            ui.painter().galley(rect.min + pad, galley, c);
        }
        if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
    })
    .inner
}

/// Кнопка-инструмент с рамкой (рамка и подсветка — из темы).
pub fn tool(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).size(13.0))
}

/// Переключатель-чип: включённый — с золотой рамкой и подсветкой.
pub fn chip(ui: &mut egui::Ui, on: bool, text: &str) -> egui::Response {
    let mark = if on { icon::CHECK_SQUARE } else { icon::SQUARE };
    let label = RichText::new(format!("{mark}  {text}")).size(13.0).color(if on { GOLD } else { MUTED });
    let mut btn = egui::Button::new(label).corner_radius(99);
    if on {
        btn = btn.fill(GOLD.gamma_multiply(0.12)).stroke(Stroke::new(1.0_f32, GOLD.gamma_multiply(0.6)));
    }
    ui.add(btn)
}

pub fn small(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).size(12.0).color(MUTED)
}

pub fn title(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text.into()).font(FontId::new(size, bold())).color(TEXT)
}

/// Волна столбиками на доле `span` ширины. Возвращает поле рисования.
pub fn paint_bars(ui: &egui::Ui, rect: Rect, peaks: Option<&[(f32, f32)]>, span: f32, color: Color32, empty: &str) -> Rect {
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 8.0, BG_DEEP);
    let inner = rect.shrink2(vec2(8.0, 8.0));
    let width = inner.width() * span.clamp(0.0, 1.0);
    match peaks.filter(|p| !p.is_empty()) {
        Some(peaks) => {
            const STEP: f32 = 4.0;
            let bars = (width / STEP).floor().max(1.0) as usize;
            let mid = inner.center().y;
            let half = inner.height() / 2.0;
            for b in 0..bars {
                let a = b * peaks.len() / bars;
                let z = ((b + 1) * peaks.len() / bars).clamp(a + 1, peaks.len());
                let amp = peaks[a..z].iter().fold(0f32, |m, &(lo, hi)| m.max(-lo).max(hi)).min(1.0);
                let h = (amp * half).max(1.5);
                let x = inner.left() + b as f32 * STEP;
                p.rect_filled(Rect::from_min_max(pos2(x, mid - h), pos2(x + STEP - 1.5, mid + h)), 1.5, color);
            }
        }
        None => {
            p.text(rect.center(), egui::Align2::CENTER_CENTER, empty, FontId::proportional(13.0), FAINT);
        }
    }
    inner
}
