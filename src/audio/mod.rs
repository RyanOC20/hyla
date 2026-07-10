//! The audio pipeline: a producer/consumer split around a lock-free ring
//! buffer. The decode thread ([`decoder`]) produces interleaved f32 samples;
//! the real-time callback ([`output`]) consumes them. [`shared`] holds the
//! atomics both sides touch.

pub mod decoder;
pub mod output;
pub mod resample;
pub mod shared;
