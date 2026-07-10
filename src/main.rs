mod app;
mod audio;
mod command;
mod library;
mod player;
mod track;

use std::path::PathBuf;

use eframe::egui;

use app::HylaApp;
use track::Track;

fn main() -> eframe::Result<()> {
    // An optional path on the command line seeds the library: a folder is
    // scanned; a file loads its containing folder. Without one, use the
    // "open folder" button in the UI.
    let library = match std::env::args().nth(1).map(PathBuf::from) {
        Some(path) if path.is_dir() => library::scan_dir(&path),
        Some(path) if path.is_file() => {
            let mut tracks = path.parent().map(library::scan_dir).unwrap_or_default();
            // Ensure the named file is present even if the parent scan missed it.
            if !tracks.iter().any(|t| t.path == path) {
                tracks.insert(0, Track::from_path(path));
            }
            tracks
        }
        _ => Vec::new(),
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([420.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "hyla",
        options,
        Box::new(move |cc| {
            app::configure(&cc.egui_ctx);
            Ok(Box::new(HylaApp::new(library)))
        }),
    )
}
