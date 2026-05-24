//! Native audio encoding and decoding for Media over QUIC.
//!
//! Sits on top of [`moq_mux`] and [`hang`] and adds the missing piece
//! for native callers: a Rust-native Opus codec implementation that
//! turns raw PCM into the bitstreams `moq_mux::codec::opus` already
//! knows how to ingest (and vice versa for decode).
//!
//! - [`AudioFormat`] mirrors WebCodecs `AudioData.format`. The helpers
//!   convert between any supported layout and the interleaved `f32`
//!   representation libopus expects.
//! - [`Frame`] is a thin owned buffer: just a timestamp and a payload.
//!   PCM layout lives on the [`Encoder`] / [`Decoder`] via
//!   [`EncoderInput`] / [`EncoderOutput`] / [`DecoderOutput`], not on
//!   each frame, so callers can't drift between calls.
//! - [`Encoder`] / [`Decoder`] are the Opus codec types.
//! - [`AudioProducer`] / [`AudioConsumer`] wire those together with
//!   `moq_mux::container` and the `hang` catalog.

mod codec;
mod error;
mod format;
mod frame;
mod resample;

pub mod consumer;
pub mod producer;

pub use codec::*;
pub use error::*;
pub use format::*;
pub use frame::*;
pub use resample::Resampler;

pub use consumer::AudioConsumer;
pub use producer::AudioProducer;
