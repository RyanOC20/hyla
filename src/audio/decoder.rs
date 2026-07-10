//! Symphonia decode thread (the producer).
//!
//! Opens a file, decodes packets to interleaved f32, adapts the channel count
//! to the output device, and pushes the result into the ring buffer. When the
//! ring is full it parks briefly and retries; decoding is far faster than
//! real-time, so this thread spends most of its life asleep.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use crossbeam_channel::Receiver;
use rtrb::Producer;
use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

use super::resample::Resampler;
use super::shared::SharedState;
use crate::command::Command;

/// Outcome of trying to push a decoded buffer into the ring.
enum PushOutcome {
    /// The whole buffer was written.
    Done,
    /// `stop` was set; the decode thread should exit.
    Stopped,
    /// A command is waiting; abandon this buffer and go handle it.
    Interrupted,
}

/// Output configuration the decode thread targets. `path` is the initial
/// track; further tracks arrive via [`Command::Load`].
pub struct DecodeConfig {
    pub path: PathBuf,
    pub out_sample_rate: u32,
    pub out_channels: usize,
}

/// Per-file decode state, rebuilt whenever a new track is loaded.
struct TrackState {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::audio::AudioDecoder>,
    resampler: Option<Resampler>,
    track_id: u32,
    src_channels: usize,
    src_rate: u32,
}

/// Open and probe `path`, publishing its duration and building a decoder plus
/// (if the rate differs) a resampler onto the output rate.
fn open_track(
    path: &Path,
    out_rate: u32,
    out_channels: usize,
    shared: &SharedState,
) -> Result<TrackState> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .context("unsupported or unrecognized audio format")?;

    let track = format
        .default_track(TrackType::Audio)
        .context("no audio track in file")?;
    let track_id = track.id;

    let audio_params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .context("missing audio codec parameters")?;
    let src_rate = audio_params.sample_rate.context("unknown sample rate")?;
    let src_channels = audio_params
        .channels
        .as_ref()
        .map(|c| c.count())
        .unwrap_or(1);

    // Publish duration in seconds (invariant to resampling).
    shared.set_duration_secs(
        track
            .num_frames
            .map(|f| f as f32 / src_rate as f32)
            .unwrap_or(0.0),
    );

    let decoder = symphonia::default::get_codecs()
        .make_audio_decoder(audio_params, &AudioDecoderOptions::default())
        .context("no decoder for this codec")?;

    // Off-rate files are converted onto the device rate by rubato; rate-matched
    // files skip it entirely, keeping the bit-transparent path.
    let resampler = if src_rate != out_rate {
        Some(Resampler::new(src_rate, out_rate, out_channels)?)
    } else {
        None
    };

    Ok(TrackState {
        format,
        decoder,
        resampler,
        track_id,
        src_channels,
        src_rate,
    })
}

/// Decode the current track, pushing interleaved f32 into `producer` and
/// servicing seek/load commands from `cmd_rx`. Returns when `stop` is set.
pub fn run(
    cfg: DecodeConfig,
    mut producer: Producer<f32>,
    shared: Arc<SharedState>,
    stop: Arc<AtomicBool>,
    cmd_rx: Receiver<Command>,
) -> Result<()> {
    let mut track = open_track(&cfg.path, cfg.out_sample_rate, cfg.out_channels, &shared)?;
    shared.position_frames.store(0, Ordering::Relaxed);

    let mut src_interleaved: Vec<f32> = Vec::new();
    let mut adapted: Vec<f32> = Vec::new();
    let mut resampled: Vec<f32> = Vec::new();
    // True once the track has been fully decoded; the thread then idles, still
    // servicing commands, rather than exiting.
    let mut finished_playing = false;

    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Service commands. A load supersedes a pending seek; otherwise only the
        // latest seek matters.
        let mut pending_seek: Option<f64> = None;
        let mut pending_load: Option<PathBuf> = None;
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Command::Seek(secs) => pending_seek = Some(secs),
                Command::Load(path) => {
                    pending_load = Some(path);
                    pending_seek = None;
                }
            }
        }

        if let Some(path) = pending_load {
            flush_ring(&mut producer, &shared, &stop);
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            match open_track(&path, cfg.out_sample_rate, cfg.out_channels, &shared) {
                Ok(new_track) => {
                    track = new_track;
                    shared.position_frames.store(0, Ordering::Relaxed);
                    shared.finished.store(false, Ordering::Relaxed);
                    finished_playing = false;
                }
                Err(e) => {
                    eprintln!("failed to load {}: {e:#}", path.display());
                    shared.set_duration_secs(0.0);
                    shared.position_frames.store(0, Ordering::Relaxed);
                    shared.finished.store(true, Ordering::Relaxed);
                    finished_playing = true;
                }
            }
            shared.flushing.store(false, Ordering::Relaxed);
            continue;
        }

        if let Some(secs) = pending_seek {
            seek(&mut producer, &mut track, &shared, &stop, &cfg, secs)?;
            finished_playing = false;
            continue;
        }

        if finished_playing {
            // Idle, waiting for a command without busy-spinning.
            std::thread::sleep(Duration::from_millis(15));
            continue;
        }

        let packet = match track.format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) | Err(SymphoniaError::IoError(_)) => {
                // End of stream: flush the resampler tail, wait for the ring to
                // drain so the final frames are heard, then idle.
                if let Some(rs) = &mut track.resampler {
                    resampled.clear();
                    rs.flush(&mut resampled)?;
                    match push_all(&mut producer, &resampled, &stop, &cmd_rx) {
                        PushOutcome::Stopped => return Ok(()),
                        PushOutcome::Interrupted => continue,
                        PushOutcome::Done => {}
                    }
                }
                while producer.slots() < producer.buffer().capacity() {
                    if stop.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    if !cmd_rx.is_empty() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                shared.finished.store(true, Ordering::Relaxed);
                finished_playing = true;
                continue;
            }
            Err(e) => return Err(e).context("reading packet"),
        };
        if packet.track_id != track.track_id {
            continue;
        }

        match track.decoder.decode(&packet) {
            Ok(buf) => {
                copy_interleaved(&buf, &mut src_interleaved);
                adapt_channels(
                    &src_interleaved,
                    track.src_channels,
                    cfg.out_channels,
                    &mut adapted,
                );
                let to_push: &[f32] = if let Some(rs) = &mut track.resampler {
                    resampled.clear();
                    rs.process(&adapted, &mut resampled)?;
                    &resampled
                } else {
                    &adapted
                };
                match push_all(&mut producer, to_push, &stop, &cmd_rx) {
                    PushOutcome::Stopped => return Ok(()),
                    PushOutcome::Interrupted => continue,
                    PushOutcome::Done => {}
                }
            }
            // Decode errors on a single packet are recoverable; skip it.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e).context("decoding packet"),
        }
    }
}

/// Signal the callback to discard buffered audio, then wait for the ring to
/// empty. Leaves `shared.flushing` set; the caller clears it once the new
/// position or track is ready.
fn flush_ring(producer: &mut Producer<f32>, shared: &SharedState, stop: &AtomicBool) {
    shared.flushing.store(true, Ordering::Relaxed);
    let capacity = producer.buffer().capacity();
    while producer.slots() < capacity {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Flush the ring and re-seek the current track to `secs`. No stale pre-seek
/// audio leaks out because the callback is silenced during the flush.
fn seek(
    producer: &mut Producer<f32>,
    track: &mut TrackState,
    shared: &SharedState,
    stop: &AtomicBool,
    cfg: &DecodeConfig,
    secs: f64,
) -> Result<()> {
    let clamped = secs.max(0.0);

    flush_ring(producer, shared, stop);
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }

    if let Some(time) = Time::try_from_secs_f64(clamped) {
        // Best-effort: if the format can't seek, keep playing from where we are.
        let _ = track.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(track.track_id),
            },
        );
    }
    track.decoder.reset();
    // The resampler holds pre-seek history; rebuild it so the new position is
    // clean.
    if track.src_rate != cfg.out_sample_rate {
        track.resampler = Some(Resampler::new(
            track.src_rate,
            cfg.out_sample_rate,
            cfg.out_channels,
        )?);
    }

    shared.position_frames.store(
        (clamped * cfg.out_sample_rate as f64) as u64,
        Ordering::Relaxed,
    );
    shared.finished.store(false, Ordering::Relaxed);
    shared.flushing.store(false, Ordering::Relaxed);
    Ok(())
}

/// Copy a decoded buffer into `dst` as interleaved f32.
fn copy_interleaved(buf: &GenericAudioBufferRef<'_>, dst: &mut Vec<f32>) {
    dst.resize(buf.samples_interleaved(), 0.0);
    buf.copy_to_slice_interleaved(dst);
}

/// Map `src_channels`-interleaved audio to `out_channels`-interleaved audio.
/// Handles the common cases (equal counts, mono upmix); otherwise truncates or
/// zero-fills per frame.
fn adapt_channels(src: &[f32], src_channels: usize, out_channels: usize, dst: &mut Vec<f32>) {
    if src_channels == out_channels {
        dst.clear();
        dst.extend_from_slice(src);
        return;
    }
    let frames = src.len().checked_div(src_channels).unwrap_or(0);
    dst.clear();
    dst.reserve(frames * out_channels);
    for f in 0..frames {
        let frame = &src[f * src_channels..f * src_channels + src_channels];
        for ch in 0..out_channels {
            let sample = if src_channels == 1 {
                frame[0] // mono: replicate to every output channel
            } else if ch < src_channels {
                frame[ch]
            } else {
                0.0
            };
            dst.push(sample);
        }
    }
}

/// Push every sample into the ring, parking briefly when it is full. Bails
/// early if `stop` is set or a command is waiting (so a seek is serviced
/// promptly instead of after the ring drains).
fn push_all(
    producer: &mut Producer<f32>,
    data: &[f32],
    stop: &AtomicBool,
    cmd_rx: &Receiver<Command>,
) -> PushOutcome {
    let mut i = 0;
    while i < data.len() {
        match producer.push(data[i]) {
            Ok(()) => i += 1,
            Err(_) => {
                if stop.load(Ordering::Relaxed) {
                    return PushOutcome::Stopped;
                }
                if !cmd_rx.is_empty() {
                    return PushOutcome::Interrupted;
                }
                std::thread::sleep(Duration::from_millis(3));
            }
        }
    }
    PushOutcome::Done
}
