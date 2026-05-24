//! Codec traits and per-codec implementations.
//!
//! Each codec exposes an [`Encoder`] / [`Decoder`] pair. They work in
//! interleaved `f32` PCM at a fixed sample rate and channel count; the
//! [`AudioProducer`](crate::AudioProducer) / [`AudioConsumer`](crate::AudioConsumer)
//! layer handles format conversion and (optionally) resampling.

use bytes::Bytes;

use crate::AudioError;

#[cfg(feature = "opus")]
pub mod opus;

#[cfg(feature = "opus")]
pub use opus::{OpusDecoder, OpusEncoder};

/// An audio encoder that turns interleaved `f32` PCM into per-packet
/// codec output.
///
/// Implementations are stateful and may buffer input until they have
/// enough samples to emit a packet — most codecs operate on fixed-size
/// frames (Opus: 20 ms by default; AAC: 1024 samples).
pub trait Encoder: Send {
	/// Catalog rendition describing this encoder's output stream.
	///
	/// Used by [`AudioProducer`](crate::AudioProducer) to register the
	/// track with the hang catalog.
	fn config(&self) -> hang::catalog::AudioConfig;

	/// Number of input frames consumed per call to [`encode`](Self::encode).
	///
	/// Callers must feed exactly this many frames (per channel) of
	/// interleaved `f32` each time. Returning `0` means the codec accepts
	/// arbitrary buffer sizes.
	fn frame_size(&self) -> usize;

	/// Sample rate the encoder expects on its input.
	fn sample_rate(&self) -> u32;

	/// Channel count the encoder expects on its input.
	fn channel_count(&self) -> u32;

	/// Encode one frame of interleaved `f32` PCM into a codec packet.
	///
	/// `pcm.len()` must equal `frame_size() * channel_count()`.
	fn encode(&mut self, pcm: &[f32]) -> Result<Bytes, AudioError>;
}

/// An audio decoder that turns codec packets back into interleaved `f32` PCM.
pub trait Decoder: Send {
	/// Sample rate the decoder will output at.
	fn sample_rate(&self) -> u32;

	/// Channel count the decoder will output.
	fn channel_count(&self) -> u32;

	/// Decode one codec packet into interleaved `f32` PCM.
	///
	/// The returned buffer's length is `frames * channel_count()`.
	fn decode(&mut self, packet: &[u8]) -> Result<Vec<f32>, AudioError>;
}
