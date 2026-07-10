//! `Player` wires the pipeline together and owns its lifetime: it opens the
//! output device, spawns the decode thread, and holds the cpal stream alive.
//!
//! The cpal stream is `!Send` on macOS, so a `Player` must live on the thread
//! that created it (the UI thread). The decode thread and shared state are the
//! `Send` parts that cross the boundary.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use anyhow::Result;
use crossbeam_channel::Sender;
use rtrb::RingBuffer;

use crate::audio::decoder::{self, DecodeConfig};
use crate::audio::output;
use crate::audio::shared::SharedState;
use crate::command::Command;

/// How many seconds of decoded audio the ring buffer can hold. Large enough to
/// ride out decode/scheduling hiccups without underrunning, small enough that
/// seek latency (ring drain) stays low.
const RING_BUFFER_SECONDS: usize = 2;

pub struct Player {
    pub shared: Arc<SharedState>,
    // Kept alive to keep the stream running; dropping it stops playback.
    _stream: cpal::Stream,
    stop: Arc<AtomicBool>,
    commands: Sender<Command>,
    decode_handle: Option<JoinHandle<()>>,
}

impl Player {
    /// Open the default output device and begin playing `path`.
    pub fn new(path: PathBuf) -> Result<Self> {
        let info = output::open_default_output()?;
        let out_rate = info.config.sample_rate;
        let out_channels = info.config.channels as usize;

        let shared = Arc::new(SharedState::new());
        shared.output_sample_rate.store(out_rate, Ordering::Relaxed);

        let capacity = out_rate as usize * out_channels * RING_BUFFER_SECONDS;
        let (producer, consumer) = RingBuffer::<f32>::new(capacity);

        let stop = Arc::new(AtomicBool::new(false));
        let (commands, cmd_rx) = crossbeam_channel::unbounded::<Command>();
        let cfg = DecodeConfig {
            path,
            out_sample_rate: out_rate,
            out_channels,
        };
        let decode_shared = shared.clone();
        let decode_stop = stop.clone();
        let decode_handle = std::thread::Builder::new()
            .name("hyla-decode".into())
            .spawn(move || {
                if let Err(e) = decoder::run(cfg, producer, decode_shared, decode_stop, cmd_rx) {
                    eprintln!("decode error: {e:#}");
                }
            })?;

        let stream = output::build_stream(&info, consumer, shared.clone())?;

        Ok(Self {
            shared,
            _stream: stream,
            stop,
            commands,
            decode_handle: Some(decode_handle),
        })
    }

    /// Seek to `secs` from the start of the track.
    pub fn seek(&self, secs: f64) {
        let _ = self.commands.send(Command::Seek(secs));
    }

    /// Stop the current track and start playing the file at `path`.
    pub fn load(&self, path: std::path::PathBuf) {
        let _ = self.commands.send(Command::Load(path));
    }

    pub fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::Relaxed)
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Relaxed);
    }

    pub fn toggle_pause(&self) {
        self.set_paused(!self.is_paused());
    }

    pub fn set_volume(&self, volume: f32) {
        self.shared.set_volume(volume);
    }

    pub fn position_secs(&self) -> f32 {
        self.shared.position_secs()
    }

    pub fn duration_secs(&self) -> f32 {
        self.shared.duration_secs()
    }

    pub fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::Relaxed)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.decode_handle.take() {
            let _ = handle.join();
        }
    }
}
