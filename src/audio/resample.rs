//! High-quality sample-rate conversion via rubato, used on the decode thread
//! only when a file's rate differs from the output device's rate. Rate-matched
//! files never touch this module, preserving the bit-transparent path.

use anyhow::Result;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, Indexing, Resampler as _, SincInterpolationParameters};

/// Number of input frames the resampler consumes per `process` call. A modest
/// chunk keeps the sinc filters in cache while amortizing per-call overhead.
const CHUNK_FRAMES: usize = 1024;

/// A streaming interleaved f32 resampler for a fixed input/output rate pair.
///
/// Feed interleaved samples with [`Resampler::process`]; produced samples are
/// appended to the caller's output vector. Call [`Resampler::flush`] at end of
/// stream to drain the filter's tail.
pub struct Resampler {
    inner: Async<f32>,
    channels: usize,
    /// Pending interleaved input not yet consumed by a full `process` chunk.
    input: Vec<f32>,
    /// Reusable interleaved output scratch, sized to the resampler's max output.
    scratch: Vec<f32>,
}

impl Resampler {
    pub fn new(src_rate: u32, out_rate: u32, channels: usize) -> Result<Self> {
        let ratio = out_rate as f64 / src_rate as f64;
        let params = SincInterpolationParameters::default();
        let inner = Async::<f32>::new_sinc(
            ratio,
            // Fixed playback ratio; no runtime ratio changes, so the relative
            // headroom can be minimal.
            1.1,
            &params,
            CHUNK_FRAMES,
            channels,
            FixedAsync::Input,
        )?;
        let scratch = vec![0.0; inner.output_frames_max() * channels];
        Ok(Self {
            inner,
            channels,
            input: Vec::new(),
            scratch,
        })
    }

    /// Resample `input` (interleaved), appending produced samples to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<()> {
        self.input.extend_from_slice(input);
        loop {
            let need = self.inner.input_frames_next();
            if self.input.len() / self.channels < need {
                break;
            }
            let produced = self.run_chunk(need, None, out)?;
            let _ = produced;
        }
        Ok(())
    }

    /// Drain any buffered input plus the resampler's internal delay tail.
    pub fn flush(&mut self, out: &mut Vec<f32>) -> Result<()> {
        let remaining = self.input.len() / self.channels;
        if remaining == 0 {
            return Ok(());
        }
        let need = self.inner.input_frames_next();
        // Pad the final partial chunk with silence up to a full chunk; rubato is
        // told the real length via `partial_len`.
        self.input.resize(need * self.channels, 0.0);
        let mut indexing = Indexing::new();
        indexing.partial_len = Some(remaining);
        self.run_chunk(need, Some(&indexing), out)?;
        self.input.clear();
        Ok(())
    }

    /// Process exactly `need` input frames from the front of `self.input`,
    /// appending output to `out` and removing consumed input.
    fn run_chunk(
        &mut self,
        need: usize,
        indexing: Option<&Indexing>,
        out: &mut Vec<f32>,
    ) -> Result<usize> {
        let out_frames = self.scratch.len() / self.channels;
        let input_adapter =
            InterleavedSlice::new(&self.input[..need * self.channels], self.channels, need)?;
        let mut output_adapter =
            InterleavedSlice::new_mut(&mut self.scratch, self.channels, out_frames)?;
        let (consumed, produced) =
            self.inner
                .process_into_buffer(&input_adapter, &mut output_adapter, indexing)?;
        // The adapters' borrows of `self.input`/`self.scratch` end here (their
        // last use), so we can read the output and drain consumed input.
        out.extend_from_slice(&self.scratch[..produced * self.channels]);
        self.input.drain(..consumed * self.channels);
        Ok(produced)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resampling 44.1k -> 48k must produce output whose length matches the
    /// rate ratio to within the filter's startup delay. A correct length ratio
    /// is equivalent to correct playback speed and pitch.
    #[test]
    fn output_length_matches_rate_ratio() {
        let channels = 2;
        let src_rate = 44_100;
        let out_rate = 48_000;
        let in_frames = src_rate as usize; // one second of audio
        // A quiet sine so values are realistic (not that the count depends on it).
        let mut input = Vec::with_capacity(in_frames * channels);
        for i in 0..in_frames {
            let v = (i as f32 * 0.05).sin() * 0.1;
            input.push(v);
            input.push(v);
        }

        let mut rs = Resampler::new(src_rate, out_rate, channels).unwrap();
        let mut out = Vec::new();
        rs.process(&input, &mut out).unwrap();
        rs.flush(&mut out).unwrap();

        let out_frames = out.len() / channels;
        let expected = in_frames as f64 * out_rate as f64 / src_rate as f64;
        let err = (out_frames as f64 - expected).abs();
        // Allow a couple of chunks of slack for the sinc delay/tail.
        assert!(
            err < 2.0 * CHUNK_FRAMES as f64,
            "out_frames={out_frames} expected≈{expected} (err={err})"
        );
    }
}
