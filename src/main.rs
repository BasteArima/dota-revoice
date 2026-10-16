// В релизе не открываем консольное окно рядом с программой.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Dota Revoice")
            .with_inner_size([1180.0, 820.0])
            .with_min_inner_size([820.0, 520.0])
            .with_icon(icon()),
        ..Default::default()
    };
    eframe::run_native("Dota Revoice", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}

/// Иконка окна и панели задач — тот же логотип, что в шапке (`assets/icon.png`).
fn icon() -> eframe::egui::IconData {
    let decode = || {
        let mut reader = png::Decoder::new(&include_bytes!("../assets/icon.png")[..]).read_info().ok()?;
        let mut rgba = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut rgba).ok()?;
        (info.color_type == png::ColorType::Rgba).then_some(eframe::egui::IconData { rgba, width: info.width, height: info.height })
    };
    decode().unwrap_or_default()
}
