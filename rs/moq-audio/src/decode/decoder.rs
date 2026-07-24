//! Opus decoder front end.
//!
//! Mirror of [`encode::Encoder`](crate::encode::Encoder): wraps libopus via
//! [`unsafe_libopus`] and produces interleaved `f32` PCM.

use std::time::Duration;

use unsafe_libopus::{OPUS_OK, OpusDecoder, opus_decode_float, opus_decoder_create, opus_decoder_destroy};

use crate::opus;
use crate::{Error, Format};

/// Opus packets cap at 120 ms (RFC 6716 §2.1.4).
const MAX_FRAME_MS: usize = 120;

/// Decoder configuration: the PCM layout to emit, plus the subscription's
/// latency budget.
///
/// The mirror of [`encode::Config`](crate::encode::Config): it describes the
/// output, since the codec's own shape is read from the catalog.
///
/// `#[non_exhaustive]`: build via [`Config::new`] (or `default()`) and set the
/// optional fields, so future knobs don't break callers.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// How to pack samples in each emitted frame.
	pub format: Format,
	/// Sample rate to emit at. `None` uses the codec's native rate from the
	/// catalog; anything else resamples.
	pub sample_rate: Option<u32>,
	/// Channel count to emit. `None` uses the codec's native count; anything
	/// else remixes mono and stereo at the decode boundary.
	pub channels: Option<u32>,
	/// Upper bound on buffering before skipping a stalled group.
	///
	/// Forwarded to [`moq_mux::container::Consumer::with_latency`]: if a group is
	/// stuck and a newer group is more than this far ahead, the consumer skips.
	/// `None` keeps the moq-mux default of zero, which skips aggressively. Set it
	/// to the playout buffer you can tolerate (typically tens to a few hundred ms)
	/// for the best congestion-vs-quality trade-off. The `_max` suffix is a
	/// reminder that we never *add* latency here: the consumer skips only when
	/// newer data is already this far ahead. A companion `latency_min` for
	/// jitter-buffer padding will land in a follow-up.
	pub latency_max: Option<Duration>,
}

impl Config {
	/// A default config: the codec's native rate and channel count, interleaved
	/// `f32`, and the moq-mux default latency.
	pub fn new() -> Self {
		Self::default()
	}
}

/// Decodes codec packets into interleaved `f32` PCM.
///
/// The bring-your-own-payload layer under [`Consumer`](super::Consumer): use it
/// when the packets don't come from a plain track subscription.
pub struct Decoder {
	inner: *mut OpusDecoder,
	sample_rate: u32,
	channel_count: u32,
	pre_skip_remaining: usize,
	max_frame_size: usize,
}

// SAFETY: see Encoder.
unsafe impl Send for Decoder {}

impl Decoder {
	/// Build a decoder from a catalog [`AudioConfig`](hang::catalog::AudioConfig).
	///
	/// Parses the OpusHead `description` if present; falls back to the catalog's
	/// declared sample rate / channel count.
	pub fn new(catalog: &hang::catalog::AudioConfig) -> Result<Self, Error> {
		let (sample_rate, channel_count, pre_skip) = if let Some(desc) = &catalog.description {
			let mut buf = desc.as_ref();
			match moq_mux::codec::opus::Config::parse(&mut buf) {
				Ok(head) => (head.sample_rate, head.channel_count, head.pre_skip),
				Err(_) => (catalog.sample_rate, catalog.channel_count, 0),
			}
		} else {
			(catalog.sample_rate, catalog.channel_count, 0)
		};

		opus::validate_rate(sample_rate)?;
		let channels = opus::validate_channels(channel_count)?;

		let mut err = 0i32;
		// SAFETY: out-pointer is valid; inner is checked for null below.
		let inner = unsafe { opus_decoder_create(sample_rate as i32, channels, &mut err) };
		if err != OPUS_OK || inner.is_null() {
			return Err(opus::error(err, "opus_decoder_create"));
		}

		let max_frame_size = (sample_rate as usize * MAX_FRAME_MS) / 1000;
		let pre_skip_remaining = (pre_skip as usize * sample_rate as usize) / 48_000;

		Ok(Self {
			inner,
			sample_rate,
			channel_count,
			pre_skip_remaining,
			max_frame_size,
		})
	}

	/// The rate the codec decodes at, read from the catalog.
	pub fn sample_rate(&self) -> u32 {
		self.sample_rate
	}

	/// The channel count the codec decodes at, read from the catalog.
	pub fn channel_count(&self) -> u32 {
		self.channel_count
	}

	/// Decode one packet into interleaved `f32` PCM.
	pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<f32>, Error> {
		let mut out = vec![0.0f32; self.max_frame_size * self.channel_count as usize];
		// SAFETY: `inner` owns a live OpusDecoder; packet/out slices bound
		// by the lengths we pass.
		let samples = unsafe {
			opus_decode_float(
				&mut *self.inner,
				packet.as_ptr(),
				packet.len() as i32,
				out.as_mut_ptr(),
				self.max_frame_size as i32,
				0,
			)
		};
		if samples < 0 {
			return Err(opus::error(samples, "opus_decode_float"));
		}
		out.truncate(samples as usize * self.channel_count as usize);
		let trim_frames = self.pre_skip_remaining.min(samples as usize);
		if trim_frames > 0 {
			let trim_samples = trim_frames * self.channel_count as usize;
			out.copy_within(trim_samples.., 0);
			out.truncate(out.len() - trim_samples);
			self.pre_skip_remaining -= trim_frames;
		}
		Ok(out)
	}
}

impl Drop for Decoder {
	fn drop(&mut self) {
		// SAFETY: `inner` is a live OpusDecoder that nothing else aliases.
		unsafe { opus_decoder_destroy(self.inner) };
	}
}
