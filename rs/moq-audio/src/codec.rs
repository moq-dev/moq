//! Opus codec wrapper.
//!
//! Single-codec implementation today: [`Encoder`] / [`Decoder`] wrap
//! libopus 1.3.1 via [`unsafe_libopus`], a pure-Rust c2rust
//! transpilation. No CMake toolchain, no sys crate, no linker
//! gymnastics. When AAC or other codecs land we'll factor out a
//! `Codec` enum dispatch; introducing a trait now would be
//! premature.

use std::str::FromStr;
use std::time::Duration;

use bytes::Bytes;
use unsafe_libopus::{
	OPUS_APPLICATION_AUDIO, OPUS_OK, OPUS_SET_BITRATE_REQUEST, OpusDecoder, OpusEncoder, opus_decode_float,
	opus_decoder_create, opus_decoder_destroy, opus_encode_float, opus_encoder_create, opus_encoder_ctl_impl,
	opus_encoder_destroy, varargs,
};

use crate::{AudioError, AudioFormat};

/// libopus packet size ceiling per RFC 6716 §3.4.
const MAX_PACKET_BYTES: usize = 4_000;

/// Codec identifier. Opus is the only variant today; AAC may follow.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Codec {
	Opus,
}

impl Codec {
	/// Canonical lowercase identifier, matching the WebCodecs / RFC
	/// catalog string. Used as the wire/FFI codec name everywhere.
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Opus => "opus",
		}
	}
}

impl std::fmt::Display for Codec {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.as_str())
	}
}

impl FromStr for Codec {
	type Err = AudioError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"opus" => Ok(Self::Opus),
			other => Err(AudioError::Unsupported(format!("unknown codec: {other}"))),
		}
	}
}

/// PCM layout the caller hands to [`Encoder::encode_f32`] /
/// `AudioProducer::write`.
#[derive(Clone, Debug)]
pub struct EncoderInput {
	pub format: AudioFormat,
	pub sample_rate: u32,
	pub channels: u32,
}

impl Default for EncoderInput {
	fn default() -> Self {
		Self {
			format: AudioFormat::F32,
			sample_rate: 48_000,
			channels: 2,
		}
	}
}

/// Codec-side configuration. `sample_rate` and `channels` are
/// optional overrides; `None` means "match input (snapping the rate
/// up to a libopus-supported value if necessary)".
#[derive(Clone, Debug)]
pub struct EncoderOutput {
	pub codec: Codec,
	pub sample_rate: Option<u32>,
	pub channels: Option<u32>,
	/// libopus bitrate in bits per second. `None` lets libopus pick.
	pub bitrate: Option<u32>,
	/// Encoded frame duration. Opus accepts 2.5 / 5 / 10 / 20 / 40 / 60 ms.
	pub frame_duration: Duration,
}

impl Default for EncoderOutput {
	fn default() -> Self {
		Self {
			codec: Codec::Opus,
			sample_rate: None,
			channels: None,
			bitrate: None,
			frame_duration: Duration::from_millis(20),
		}
	}
}

/// PCM layout the caller wants out of [`Decoder::decode_f32`] /
/// `AudioConsumer::read`. `sample_rate` and `channels` `None`
/// means "match the codec's native shape from the catalog".
#[derive(Clone, Debug, Default)]
pub struct DecoderOutput {
	pub format: AudioFormat,
	pub sample_rate: Option<u32>,
	pub channels: Option<u32>,
	/// Upper bound on buffering before skipping a stalled group.
	///
	/// Forwarded to [`moq_mux::container::Consumer::with_latency`]: if
	/// a group is stuck and a newer group is more than this far ahead,
	/// the consumer skips. `None` keeps the moq-mux default of zero,
	/// which skips aggressively. Set to the playout buffer you can
	/// tolerate (typically tens to a few hundred ms) for the best
	/// congestion-vs-quality trade-off. The `_max` suffix is a
	/// reminder that we never *add* latency here: the consumer skips
	/// only when newer data is already this far ahead. A companion
	/// `latency_min` for jitter-buffer padding will land in a follow-up.
	pub latency_max: Option<Duration>,
}

fn validate_opus_channels(count: u32) -> Result<i32, AudioError> {
	match count {
		1 | 2 => Ok(count as i32),
		other => Err(AudioError::Unsupported(format!(
			"opus only supports 1 or 2 channels (got {other})"
		))),
	}
}

fn opus_error(code: i32, context: &str) -> AudioError {
	AudioError::Unsupported(format!("libopus {context} failed (code {code})"))
}

/// Snap an arbitrary sample rate up to the nearest libopus-supported
/// rate (8/12/16/24/48 kHz); falls back to 48 kHz for anything above.
pub fn pick_opus_rate(input_rate: u32) -> u32 {
	const SUPPORTED: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];
	SUPPORTED.iter().copied().find(|&r| r >= input_rate).unwrap_or(48_000)
}

fn validate_opus_rate(rate: u32) -> Result<(), AudioError> {
	match rate {
		8_000 | 12_000 | 16_000 | 24_000 | 48_000 => Ok(()),
		other => Err(AudioError::Unsupported(format!(
			"opus only supports 8/12/16/24/48 kHz (got {other})"
		))),
	}
}

fn frame_size_for(sample_rate: u32, duration: Duration) -> Result<usize, AudioError> {
	// Opus only accepts these exact durations.
	let micros = duration.as_micros();
	let allowed = [2_500u128, 5_000, 10_000, 20_000, 40_000, 60_000];
	if !allowed.contains(&micros) {
		return Err(AudioError::Unsupported(format!(
			"opus frame duration must be 2.5/5/10/20/40/60 ms (got {} us)",
			micros
		)));
	}
	Ok((sample_rate as u128 * micros / 1_000_000) as usize)
}

/// Opus encoder over the PCM layout declared in [`EncoderInput`].
pub struct Encoder {
	inner: *mut OpusEncoder,
	input: EncoderInput,
	output: EncoderOutput,
	/// Resolved codec sample rate (from `output.sample_rate` or
	/// `pick_opus_rate(input.sample_rate)`).
	codec_rate: u32,
	/// Resolved codec channel count (currently same as `input.channels`).
	codec_channels: u32,
	frame_size: usize,
	scratch: Vec<u8>,
}

// SAFETY: OpusEncoder is heap-allocated state owned exclusively by this
// struct; libopus encoder methods take a single &mut, so a unique
// owner is allowed to move it across threads.
unsafe impl Send for Encoder {}

impl Encoder {
	pub fn new(input: EncoderInput, output: EncoderOutput) -> Result<Self, AudioError> {
		match output.codec {
			Codec::Opus => Self::new_opus(input, output),
		}
	}

	fn new_opus(input: EncoderInput, output: EncoderOutput) -> Result<Self, AudioError> {
		let codec_rate = output.sample_rate.unwrap_or_else(|| pick_opus_rate(input.sample_rate));
		validate_opus_rate(codec_rate)?;

		let codec_channels = output.channels.unwrap_or(input.channels);
		if codec_channels != input.channels {
			return Err(AudioError::Unsupported(format!(
				"channel remapping not implemented (input {}ch, output {codec_channels}ch)",
				input.channels
			)));
		}
		let channels = validate_opus_channels(codec_channels)?;

		let frame_size = frame_size_for(codec_rate, output.frame_duration)?;

		let mut err = 0i32;
		// SAFETY: out-pointer `err` is valid; inner is checked for null below.
		let inner = unsafe { opus_encoder_create(codec_rate as i32, channels, OPUS_APPLICATION_AUDIO, &mut err) };
		if err != OPUS_OK || inner.is_null() {
			return Err(opus_error(err, "opus_encoder_create"));
		}

		if let Some(b) = output.bitrate {
			// SAFETY: `inner` is a freshly-created encoder; varargs! produces
			// the single i32 the SET_BITRATE request expects.
			let rc = unsafe { opus_encoder_ctl_impl(inner, OPUS_SET_BITRATE_REQUEST, varargs![b as i32]) };
			if rc != OPUS_OK {
				// SAFETY: `inner` was created above and not yet handed out.
				unsafe { opus_encoder_destroy(inner) };
				return Err(opus_error(rc, "OPUS_SET_BITRATE"));
			}
		}

		Ok(Self {
			inner,
			input,
			output,
			codec_rate,
			codec_channels,
			frame_size,
			scratch: vec![0u8; MAX_PACKET_BYTES],
		})
	}

	pub fn input(&self) -> &EncoderInput {
		&self.input
	}

	pub fn output(&self) -> &EncoderOutput {
		&self.output
	}

	/// Sample rate libopus actually runs at.
	pub fn codec_rate(&self) -> u32 {
		self.codec_rate
	}

	/// Channel count libopus actually runs at.
	pub fn codec_channels(&self) -> u32 {
		self.codec_channels
	}

	/// Number of input frames libopus consumes per call.
	pub fn frame_size(&self) -> usize {
		self.frame_size
	}

	/// Encode one frame of interleaved `f32` PCM at `codec_rate`.
	///
	/// `pcm.len()` must equal `frame_size() * codec_channels()`. The
	/// producer typically handles format conversion and resampling
	/// before calling this; for direct use, the caller does the same.
	pub fn encode_f32(&mut self, pcm: &[f32]) -> Result<Bytes, AudioError> {
		let expected = self.frame_size * self.codec_channels as usize;
		if pcm.len() != expected {
			return Err(AudioError::Misaligned {
				got: std::mem::size_of_val(pcm),
				expected: expected * std::mem::size_of::<f32>(),
			});
		}
		// SAFETY: `inner` owns a live OpusEncoder; pcm and scratch slices
		// are bounded by the lengths we pass.
		let n = unsafe {
			opus_encode_float(
				self.inner,
				pcm.as_ptr(),
				self.frame_size as i32,
				self.scratch.as_mut_ptr(),
				self.scratch.len() as i32,
			)
		};
		if n < 0 {
			return Err(opus_error(n, "opus_encode_float"));
		}
		Ok(Bytes::copy_from_slice(&self.scratch[..n as usize]))
	}

	/// hang catalog entry describing this encoder's output stream.
	pub fn catalog(&self) -> hang::catalog::AudioConfig {
		// `codec_channels` is validated to mono/stereo at encoder construction, so the
		// OpusHead (channel mapping family 0) always encodes.
		let head = moq_mux::codec::opus::Config {
			sample_rate: self.codec_rate,
			channel_count: self.codec_channels,
		}
		.encode()
		.expect("opus encoder channels validated to mono/stereo");

		let mut config =
			hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, self.codec_rate, self.codec_channels);
		config.bitrate = self.output.bitrate.map(|b| b as u64);
		config.description = Some(head);
		config.container = hang::catalog::Container::Legacy;
		config
	}
}

/// Opus decoder producing interleaved `f32` PCM.
pub struct Decoder {
	inner: *mut OpusDecoder,
	sample_rate: u32,
	channel_count: u32,
	max_frame_size: usize,
}

// SAFETY: see Encoder above.
unsafe impl Send for Decoder {}

impl Decoder {
	/// Build a decoder from a catalog [`AudioConfig`](hang::catalog::AudioConfig).
	///
	/// Parses the OpusHead `description` if present; falls back to the
	/// catalog's declared sample rate / channel count.
	pub fn new(catalog: &hang::catalog::AudioConfig) -> Result<Self, AudioError> {
		let (sample_rate, channel_count) = if let Some(desc) = &catalog.description {
			let mut buf = desc.as_ref();
			match moq_mux::codec::opus::Config::parse(&mut buf) {
				Ok(head) => (head.sample_rate, head.channel_count),
				Err(_) => (catalog.sample_rate, catalog.channel_count),
			}
		} else {
			(catalog.sample_rate, catalog.channel_count)
		};

		validate_opus_rate(sample_rate)?;
		let channels = validate_opus_channels(channel_count)?;

		let mut err = 0i32;
		// SAFETY: out-pointer is valid; inner is checked for null below.
		let inner = unsafe { opus_decoder_create(sample_rate as i32, channels, &mut err) };
		if err != OPUS_OK || inner.is_null() {
			return Err(opus_error(err, "opus_decoder_create"));
		}

		// Opus packets cap at 120 ms.
		let max_frame_size = (sample_rate as usize * 120) / 1000;

		Ok(Self {
			inner,
			sample_rate,
			channel_count,
			max_frame_size,
		})
	}

	pub fn sample_rate(&self) -> u32 {
		self.sample_rate
	}

	pub fn channel_count(&self) -> u32 {
		self.channel_count
	}

	/// Decode one packet into interleaved `f32` PCM.
	pub fn decode_f32(&mut self, packet: &[u8]) -> Result<Vec<f32>, AudioError> {
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
			return Err(opus_error(samples, "opus_decode_float"));
		}
		out.truncate(samples as usize * self.channel_count as usize);
		Ok(out)
	}
}

impl Drop for Encoder {
	fn drop(&mut self) {
		// SAFETY: `inner` is a live OpusEncoder that nothing else aliases.
		unsafe { opus_encoder_destroy(self.inner) };
	}
}

impl Drop for Decoder {
	fn drop(&mut self) {
		// SAFETY: same as Encoder.
		unsafe { opus_decoder_destroy(self.inner) };
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sine(freq: f32, sample_rate: u32, channels: u32, frames: usize) -> Vec<f32> {
		let mut out = Vec::with_capacity(frames * channels as usize);
		for i in 0..frames {
			let t = i as f32 / sample_rate as f32;
			let v = (2.0 * std::f32::consts::PI * freq * t).sin() * 0.5;
			for _ in 0..channels {
				out.push(v);
			}
		}
		out
	}

	#[test]
	fn opus_encode_then_decode_keeps_signal_close() {
		let mut enc = Encoder::new(
			EncoderInput {
				format: AudioFormat::F32,
				sample_rate: 48_000,
				channels: 2,
			},
			EncoderOutput {
				bitrate: Some(96_000),
				..EncoderOutput::default()
			},
		)
		.unwrap();

		let cfg = enc.catalog();
		let mut dec = Decoder::new(&cfg).unwrap();

		let frame = sine(440.0, 48_000, 2, enc.frame_size());
		for _ in 0..5 {
			let pkt = enc.encode_f32(&frame).unwrap();
			let _ = dec.decode_f32(&pkt).unwrap();
		}

		let pkt = enc.encode_f32(&frame).unwrap();
		let decoded = dec.decode_f32(&pkt).unwrap();
		assert_eq!(decoded.len(), frame.len());

		let energy_in: f32 = frame.iter().map(|s| s * s).sum();
		let energy_out: f32 = decoded.iter().map(|s| s * s).sum();
		let ratio = energy_out / energy_in;
		assert!(
			(0.5..2.0).contains(&ratio),
			"output energy ratio {ratio:.3} should be close to 1"
		);
	}

	#[test]
	fn opus_rejects_unsupported_frame_duration() {
		let err = Encoder::new(
			EncoderInput::default(),
			EncoderOutput {
				frame_duration: Duration::from_millis(15),
				..EncoderOutput::default()
			},
		);
		assert!(matches!(err, Err(AudioError::Unsupported(_))));
	}

	#[test]
	fn opus_rejects_misaligned_input() {
		let mut enc = Encoder::new(EncoderInput::default(), EncoderOutput::default()).unwrap();
		assert!(matches!(
			enc.encode_f32(&[0.0f32; 100]),
			Err(AudioError::Misaligned { .. })
		));
	}

	#[test]
	fn opus_catalog_includes_opushead() {
		let enc = Encoder::new(
			EncoderInput {
				sample_rate: 48_000,
				channels: 2,
				..EncoderInput::default()
			},
			EncoderOutput {
				bitrate: Some(64_000),
				..EncoderOutput::default()
			},
		)
		.unwrap();
		let cfg = enc.catalog();
		assert_eq!(cfg.sample_rate, 48_000);
		assert_eq!(cfg.channel_count, 2);
		assert_eq!(cfg.bitrate, Some(64_000));
		let desc = cfg.description.expect("OpusHead should be present");
		assert_eq!(desc.len(), 19);
	}

	#[test]
	fn rate_picker_snaps_up() {
		assert_eq!(pick_opus_rate(44_100), 48_000);
		assert_eq!(pick_opus_rate(22_050), 24_000);
		for &r in &[8_000, 12_000, 16_000, 24_000, 48_000] {
			assert_eq!(pick_opus_rate(r), r);
		}
	}

	#[test]
	fn codec_roundtrips_as_str() {
		assert_eq!(Codec::Opus.as_str(), "opus");
		assert_eq!(Codec::Opus.to_string(), "opus");
		assert_eq!("opus".parse::<Codec>().unwrap(), Codec::Opus);
		assert!("aac".parse::<Codec>().is_err());
	}

	#[test]
	fn encoder_output_overrides_codec_rate() {
		let enc = Encoder::new(
			EncoderInput {
				sample_rate: 48_000,
				channels: 1,
				..EncoderInput::default()
			},
			EncoderOutput {
				sample_rate: Some(24_000),
				..EncoderOutput::default()
			},
		)
		.unwrap();
		assert_eq!(enc.codec_rate(), 24_000);
		assert_eq!(enc.catalog().sample_rate, 24_000);
	}
}
