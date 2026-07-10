//! cpal output stream and the real-time audio callback (the consumer).
//!
//! The callback runs on a real-time thread. Per the audio-thread rules in
//! CLAUDE.md it does nothing but: pull interleaved f32 frames from the ring,
//! apply volume, write to the device, and advance the consumed-frame counter.
//! No allocation, no locks, no I/O, no logging, no panics.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result};
use cpal::{
    Device, FromSample, OutputCallbackInfo, SampleFormat, SizedSample, Stream, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rtrb::Consumer;

use super::shared::SharedState;

/// Everything needed to describe the opened output device.
pub struct OutputInfo {
    pub device: Device,
    pub config: StreamConfig,
    pub sample_format: SampleFormat,
}

/// Open the default output device and query its default configuration. We open
/// at the device's native rate and let the decode thread resample off-rate
/// files onto it (the default strategy in CLAUDE.md).
pub fn open_default_output() -> Result<OutputInfo> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .context("no default output device")?;
    let supported = device
        .default_output_config()
        .context("no default output config")?;
    Ok(OutputInfo {
        sample_format: supported.sample_format(),
        config: supported.into(),
        device,
    })
}

/// Build and start the output stream, moving the ring consumer into the
/// callback. The returned `Stream` must be kept alive for playback to continue.
pub fn build_stream(
    info: &OutputInfo,
    consumer: Consumer<f32>,
    shared: Arc<SharedState>,
) -> Result<Stream> {
    let channels = info.config.channels as usize;
    let stream = match info.sample_format {
        SampleFormat::F32 => run::<f32>(info, channels, consumer, shared)?,
        SampleFormat::I16 => run::<i16>(info, channels, consumer, shared)?,
        SampleFormat::I32 => run::<i32>(info, channels, consumer, shared)?,
        SampleFormat::U16 => run::<u16>(info, channels, consumer, shared)?,
        other => anyhow::bail!("unsupported output sample format: {other:?}"),
    };
    stream.play().context("failed to start output stream")?;
    Ok(stream)
}

fn run<T>(
    info: &OutputInfo,
    channels: usize,
    mut consumer: Consumer<f32>,
    shared: Arc<SharedState>,
) -> Result<Stream>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let silence = T::from_sample(0.0f32);
    let data_fn = move |data: &mut [T], _: &OutputCallbackInfo| {
        // During a seek, discard buffered pre-seek audio and emit silence so no
        // stale audio leaks out. Position is not advanced.
        if shared.flushing.load(Ordering::Relaxed) {
            while consumer.pop().is_ok() {}
            for s in data.iter_mut() {
                *s = silence;
            }
            return;
        }

        // If paused, emit silence without draining the ring so resume is
        // seamless and position freezes.
        if shared.paused.load(Ordering::Relaxed) {
            for s in data.iter_mut() {
                *s = silence;
            }
            return;
        }

        let volume = shared.volume();
        let mut frames_consumed: u64 = 0;
        for frame in data.chunks_mut(channels) {
            let mut full = true;
            for slot in frame.iter_mut() {
                match consumer.pop() {
                    Ok(sample) => *slot = T::from_sample(sample * volume),
                    Err(_) => {
                        // Underrun: write silence, never block or glitch.
                        *slot = silence;
                        full = false;
                    }
                }
            }
            if full {
                frames_consumed += 1;
            }
        }
        if frames_consumed > 0 {
            shared
                .position_frames
                .fetch_add(frames_consumed, Ordering::Relaxed);
        }
    };

    let err_fn = |err: cpal::Error| eprintln!("output stream error: {err}");
    let stream = info
        .device
        .build_output_stream(info.config, data_fn, err_fn, None)
        .context("failed to build output stream")?;
    Ok(stream)
}
