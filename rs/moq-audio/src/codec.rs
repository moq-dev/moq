//! Opus codec wrapper.
//!
//! Single-codec implementation today: [`Encoder`] / [`Decoder`] wrap
//! libopus via the [`opus`](https://crates.io/crates/opus) crate.
//! When AAC or other codecs land we'll factor out a `Codec` enum;
//! introducing the trait now would be premature.

use std::time::Duration;

use bytes::Bytes;
use opus::{Application, Channels};

use crate::{AudioError, AudioFormat};

/// libopus packet size ceiling per RFC 6716 §3.4.
const MAX_PACKET_BYTES: usize = 4_000;

/// Codec identifier. Opus is the only variant today; AAC may follow.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Codec {
	Opus,
}

/// Encoder configuration.
///
/// `input_format` / `input_sample_rate` / `input_channels` describe
/// what the caller will pass to [`Encoder::encode_f32`]. They are
/// baked in at construction so individual frames don't carry format
/// information.
#[derive(Clone, Debug)]
pub struct EncoderConfig {
	pub codec: Codec,
	pub input_format: AudioFormat,
	pub input_sample_rate: u32,
	pub input_channels: u32,
	/// libopus bitrate in bits per second. `None` lets libopus pick.
	pub bitrate: Option<u32>,
	/// Encoded frame duration. Opus accepts 2.5 / 5 / 10 / 20 / 40 / 60 ms.
	pub frame_duration: Duration,
}

impl Default for EncoderConfig {
	fn default() -> Self {
		Self {
			codec: Codec::Opus,
			input_format: AudioFormat::F32,
			input_sample_rate: 48_000,
			input_channels: 2,
			bitrate: None,
			frame_duration: Duration::from_millis(20),
		}
	}
}

/// Decoder configuration.
///
/// `output_*` describe how the caller wants samples delivered.
/// `None` for rate or channels means "match the codec's native shape
/// from the catalog".
#[derive(Clone, Debug, Default)]
pub struct DecoderConfig {
	pub output_format: AudioFormat,
	pub output_sample_rate: Option<u32>,
	pub output_channels: Option<u32>,
}

fn channels_for(count: u32) -> Result<Channels, AudioError> {
	match count {
		1 => Ok(Channels::Mono),
		2 => Ok(Channels::Stereo),
		other => Err(AudioError::Unsupported(format!(
			"opus only supports 1 or 2 channels (got {other})"
		))),
	}
}

/// Pick a libopus-supported rate close to `input_rate`.
pub fn pick_opus_rate(input_rate: u32) -> u32 {
	const SUPPORTED: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];
	if SUPPORTED.contains(&input_rate) {
		return input_rate;
	}
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

/// Opus encoder over the input format declared in [`EncoderConfig`].
pub struct Encoder {
	inner: opus::Encoder,
	config: EncoderConfig,
	/// Sample rate libopus actually runs at (snapped to a supported rate).
	codec_rate: u32,
	frame_size: usize,
	scratch: Vec<u8>,
}

impl Encoder {
	pub fn new(config: EncoderConfig) -> Result<Self, AudioError> {
		match config.codec {
			Codec::Opus => Self::new_opus(config),
		}
	}

	fn new_opus(config: EncoderConfig) -> Result<Self, AudioError> {
		let codec_rate = pick_opus_rate(config.input_sample_rate);
		validate_opus_rate(codec_rate)?;
		let channels = channels_for(config.input_channels)?;

		let mut inner = opus::Encoder::new(codec_rate, channels, Application::Audio)?;
		if let Some(b) = config.bitrate {
			inner.set_bitrate(opus::Bitrate::Bits(b as i32))?;
		}

		let frame_size = frame_size_for(codec_rate, config.frame_duration)?;

		Ok(Self {
			inner,
			config,
			codec_rate,
			frame_size,
			scratch: vec![0u8; MAX_PACKET_BYTES],
		})
	}

	pub fn config(&self) -> &EncoderConfig {
		&self.config
	}

	/// Sample rate libopus actually runs at. Equal to
	/// `config.input_sample_rate` if that was already a libopus-supported
	/// rate; otherwise snapped up (e.g. 44.1 kHz → 48 kHz).
	pub fn codec_rate(&self) -> u32 {
		self.codec_rate
	}

	/// Number of input frames libopus consumes per call.
	pub fn frame_size(&self) -> usize {
		self.frame_size
	}

	/// Encode one frame of interleaved `f32` PCM at `codec_rate`.
	///
	/// `pcm.len()` must equal `frame_size() * config.input_channels`. The
	/// producer typically handles format conversion and resampling
	/// before calling this; for direct use, the caller does the same.
	pub fn encode_f32(&mut self, pcm: &[f32]) -> Result<Bytes, AudioError> {
		let expected = self.frame_size * self.config.input_channels as usize;
		if pcm.len() != expected {
			return Err(AudioError::Misaligned {
				got: std::mem::size_of_val(pcm),
				expected: expected * std::mem::size_of::<f32>(),
			});
		}
		let n = self.inner.encode_float(pcm, &mut self.scratch)?;
		Ok(Bytes::copy_from_slice(&self.scratch[..n]))
	}

	/// hang catalog entry describing this encoder's output stream.
	pub fn catalog_config(&self) -> hang::catalog::AudioConfig {
		let head = moq_mux::codec::opus::Config {
			sample_rate: self.codec_rate,
			channel_count: self.config.input_channels,
		}
		.encode();

		let mut config = hang::catalog::AudioConfig::new(
			hang::catalog::AudioCodec::Opus,
			self.codec_rate,
			self.config.input_channels,
		);
		config.bitrate = self.config.bitrate.map(|b| b as u64);
		config.description = Some(head);
		config.container = hang::catalog::Container::Legacy;
		config
	}
}

/// Opus decoder producing interleaved `f32` PCM.
pub struct Decoder {
	inner: opus::Decoder,
	sample_rate: u32,
	channel_count: u32,
	max_frame_size: usize,
}

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
		let channels = channels_for(channel_count)?;
		let inner = opus::Decoder::new(sample_rate, channels)?;
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
		let samples = self.inner.decode_float(packet, &mut out, false)?;
		out.truncate(samples * self.channel_count as usize);
		Ok(out)
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
		let mut enc = Encoder::new(EncoderConfig {
			input_sample_rate: 48_000,
			input_channels: 2,
			bitrate: Some(96_000),
			..EncoderConfig::default()
		})
		.unwrap();

		let cfg = enc.catalog_config();
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
		let err = Encoder::new(EncoderConfig {
			frame_duration: Duration::from_millis(15),
			..EncoderConfig::default()
		});
		assert!(matches!(err, Err(AudioError::Unsupported(_))));
	}

	#[test]
	fn opus_rejects_misaligned_input() {
		let mut enc = Encoder::new(EncoderConfig::default()).unwrap();
		assert!(matches!(
			enc.encode_f32(&[0.0f32; 100]),
			Err(AudioError::Misaligned { .. })
		));
	}

	#[test]
	fn opus_config_includes_opushead() {
		let enc = Encoder::new(EncoderConfig {
			input_sample_rate: 48_000,
			input_channels: 2,
			bitrate: Some(64_000),
			..EncoderConfig::default()
		})
		.unwrap();
		let cfg = enc.catalog_config();
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
}
