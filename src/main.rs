mod app;
mod audio;
mod command;
mod library;
mod media_keys;
mod player;
mod track;

use std::path::PathBuf;

use eframe::egui;

use app::HylaApp;
use track::Track;

/// The default library folder scanned at startup when no path is given on the
/// command line: `~/Music/hyla-library/`.
fn default_library_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Music/hyla-library"))
}

fn main() -> eframe::Result<()> {
    // An optional path on the command line seeds the library: a folder is
    // scanned; a file loads its containing folder. Without one, fall back to the
    // default library at ~/Music/hyla-library/; if that is missing, start empty
    // and let the user pick a folder in the UI.
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
        _ => match default_library_dir() {
            Some(dir) if dir.is_dir() => library::scan_dir(&dir),
            _ => Vec::new(),
        },
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 560.0])
            .with_title("Hyla")
            // Blend the titlebar into the app: make it transparent, hide the
            // title text, and let the content extend up behind it. The traffic
            // light buttons stay put so the window is still controllable.
            .with_titlebar_shown(false)
            .with_title_shown(false)
            .with_fullsize_content_view(true),
        ..Default::default()
    };
    eframe::run_native(
        "Hyla",
        options,
        Box::new(move |cc| {
            app::configure(&cc.egui_ctx);
            Ok(Box::new(HylaApp::new(library)))
        }),
    )
}
