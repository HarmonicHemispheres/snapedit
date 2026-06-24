// Hide the console window in release builds (keep it in debug for logs).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod capture;
mod document;
mod export;
mod output;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SnapEdit")
            .with_inner_size([1120.0, 740.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "SnapEdit",
        options,
        Box::new(|cc| Ok(Box::new(app::SnapEdit::new(cc)))),
    )
}
