//! A track is just a file on disk and its display name. hyla shows filenames
//! only, no metadata browsing or cover art (CLAUDE.md non-goals).

use std::path::PathBuf;

#[derive(Clone)]
pub struct Track {
    pub path: PathBuf,
    pub file_name: String,
}

impl Track {
    pub fn from_path(path: PathBuf) -> Self {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<unknown>")
            .to_string();
        Self { path, file_name }
    }
}
