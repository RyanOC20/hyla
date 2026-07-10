//! Commands sent from the UI thread to the decode thread over a channel.
//! Realtime-safe control (pause, volume) goes through atomics instead; the
//! channel carries actions that the decoder must perform, like seeking.

use std::path::PathBuf;

/// A control message for the decode thread.
pub enum Command {
    /// Seek to the given position in seconds from the start of the track.
    Seek(f64),
    /// Stop the current track and start decoding the file at this path.
    Load(PathBuf),
}
