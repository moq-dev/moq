//! Native audio encoding and decoding for Media over QUIC.
//!
//! Sits on top of [`moq_mux`] and [`hang`] and adds the missing piece for
//! native callers: real codec implementations that turn raw PCM samples
//! into Opus packets (and back). [`moq_mux`] handles container framing
//! and on-wire ingestion; this crate handles the codec itself.
//!
//! - [`AudioFormat`] mirrors WebCodecs `AudioData.format`. The
//!   per-sample helpers in [`format`] convert between any supported
//!   layout and the interleaved `f32` representation codecs work in.
//! - [`AudioSamples`] is a thin owned PCM buffer with timestamp,
//!   sample rate, channel count, and layout.
//! - [`codec`] exposes the generic [`Encoder`](codec::Encoder) /
//!   [`Decoder`](codec::Decoder) traits, with an Opus implementation
//!   gated behind the `opus` feature.
//! - [`AudioProducer`] / [`AudioConsumer`] glue the codec to
//!   [`moq_mux::container`] and the [`hang`] catalog.

mod error;
mod format;
mod samples;

pub mod codec;
pub mod consumer;
pub mod producer;

#[cfg(feature = "resample")]
pub mod resample;

pub use error::*;
pub use format::*;
pub use samples::*;

pub use consumer::AudioConsumer;
pub use producer::AudioProducer;
