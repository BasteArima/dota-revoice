// В релизе не открываем консольное окно рядом с программой.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Dota Revoice")
            .with_inner_size([1180.0, 820.0])
            .with_min_inner_size([820.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native("Dota Revoice", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
