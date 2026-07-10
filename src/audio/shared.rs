//! State shared between the UI thread, the decode thread, and the real-time
//! audio callback. Everything here is lock-free (atomics only) so the callback
//! can touch it without ever blocking.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Lock-free playback state. Wrapped in an `Arc` and shared by all three threads.
pub struct SharedState {
    /// When true, the callback emits silence and stops draining the ring, so
    /// pause is instant and resume is seamless.
    pub paused: AtomicBool,
    /// Output volume in `[0, 1]`, stored as the bit pattern of an `f32`.
    volume_bits: AtomicU32,
    /// Frames actually consumed by the callback at the output sample rate. This
    /// is the source of truth for the progress bar: it reflects what is audible,
    /// not what has been decoded ahead.
    pub position_frames: AtomicU64,
    /// The output device sample rate, used by the UI to turn `position_frames`
    /// into seconds.
    pub output_sample_rate: AtomicU32,
    /// Total duration of the current track in seconds, stored as `f32` bits.
    /// `0.0` means unknown. Duration in seconds is invariant to resampling, so
    /// this stays valid regardless of the output rate.
    duration_secs_bits: AtomicU32,
    /// Set by the decode thread when the track has been fully decoded and the
    /// ring has drained.
    pub finished: AtomicBool,
    /// Set during a seek: the callback discards any buffered pre-seek audio and
    /// emits silence until the decode thread has re-seeked and cleared it.
    pub flushing: AtomicBool,
}

impl SharedState {
    pub fn new() -> Self {
        Self {
            paused: AtomicBool::new(false),
            volume_bits: AtomicU32::new(1.0f32.to_bits()),
            position_frames: AtomicU64::new(0),
            output_sample_rate: AtomicU32::new(48_000),
            duration_secs_bits: AtomicU32::new(0),
            finished: AtomicBool::new(false),
            flushing: AtomicBool::new(false),
        }
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume_bits.load(Ordering::Relaxed))
    }

    pub fn set_volume(&self, v: f32) {
        self.volume_bits
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn duration_secs(&self) -> f32 {
        f32::from_bits(self.duration_secs_bits.load(Ordering::Relaxed))
    }

    pub fn set_duration_secs(&self, secs: f32) {
        self.duration_secs_bits
            .store(secs.to_bits(), Ordering::Relaxed);
    }

    /// Current playback position in seconds, derived from consumed frames.
    pub fn position_secs(&self) -> f32 {
        let rate = self.output_sample_rate.load(Ordering::Relaxed);
        if rate == 0 {
            return 0.0;
        }
        self.position_frames.load(Ordering::Relaxed) as f32 / rate as f32
    }
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}
