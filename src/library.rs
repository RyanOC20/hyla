//! Scanning a directory for playable audio files.

use std::path::Path;

use crate::track::Track;

/// File extensions hyla will attempt to play, matching the codecs Symphonia is
/// built with (FLAC, MP3, WAV, AAC/ALAC, OGG, AIFF).
const SUPPORTED_EXTENSIONS: &[&str] = &[
    "flac", "mp3", "wav", "aac", "m4a", "alac", "ogg", "oga", "aiff", "aif",
];

fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            SUPPORTED_EXTENSIONS
                .iter()
                .any(|s| s.eq_ignore_ascii_case(ext))
        })
}

/// Return the playable tracks directly inside `dir`, sorted by filename. The
/// scan is shallow (no recursion) to keep the list simple and predictable.
pub fn scan_dir(dir: &Path) -> Vec<Track> {
    let mut tracks: Vec<Track> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && is_supported(p))
            .map(Track::from_path)
            .collect(),
        Err(_) => Vec::new(),
    };
    tracks.sort_by_key(|t| t.file_name.to_lowercase());
    tracks
}
