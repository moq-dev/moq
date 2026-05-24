//! Opus encoder and decoder.
//!
//! Wraps the [`opus`](https://crates.io/crates/opus) crate (libopus FFI).
//! Frames are fixed at 20 ms to match the JS publish path
//! (`js/publish/src/audio/encoder.ts`). Each encode emits one packet that
//! becomes its own moq-lite group (see [`AudioProducer`](crate::AudioProducer)).

use bytes::Bytes;
use opus::{Application, Channels};

use crate::AudioError;
use crate::codec::{Decoder, Encoder};

/// Frame duration in milliseconds — matches the JS publisher.
const FRAME_DURATION_MS: u32 = 20;

/// Largest packet libopus can produce, per RFC 6716 §3.4.
const MAX_PACKET_BYTES: usize = 4_000;

fn channels_for(count: u32) -> Result<Channels, AudioError> {
	match count {
		1 => Ok(Channels::Mono),
		2 => Ok(Channels::Stereo),
		other => Err(AudioError::Unsupported(format!(
			"opus only supports 1 or 2 channels (got {other})"
		))),
	}
}

fn validate_sample_rate(rate: u32) -> Result<(), AudioError> {
	match rate {
		8_000 | 12_000 | 16_000 | 24_000 | 48_000 => Ok(()),
		other => Err(AudioError::Unsupported(format!(
			"opus only supports 8/12/16/24/48 kHz (got {other})"
		))),
	}
}

/// Opus encoder over interleaved `f32` PCM.
pub struct OpusEncoder {
	inner: opus::Encoder,
	sample_rate: u32,
	channel_count: u32,
	frame_size: usize,
	bitrate: Option<u32>,
	output: Vec<u8>,
}

impl OpusEncoder {
	/// Build a new encoder.
	///
	/// `bitrate` is the target bitrate in bits per second; `None` lets
	/// libopus pick a sensible default (96 kbps for stereo at 48 kHz).
	pub fn new(sample_rate: u32, channel_count: u32, bitrate: Option<u32>) -> Result<Self, AudioError> {
		validate_sample_rate(sample_rate)?;
		let channels = channels_for(channel_count)?;

		let mut inner = opus::Encoder::new(sample_rate, channels, Application::Audio)?;
		if let Some(b) = bitrate {
			inner.set_bitrate(opus::Bitrate::Bits(b as i32))?;
		}

		let frame_size = (sample_rate as usize * FRAME_DURATION_MS as usize) / 1000;
		Ok(Self {
			inner,
			sample_rate,
			channel_count,
			frame_size,
			bitrate,
			output: vec![0u8; MAX_PACKET_BYTES],
		})
	}
}

impl Encoder for OpusEncoder {
	fn config(&self) -> hang::catalog::AudioConfig {
		let head = moq_mux::codec::opus::Config {
			sample_rate: self.sample_rate,
			channel_count: self.channel_count,
		}
		.encode();

		hang::catalog::AudioConfig {
			codec: hang::catalog::AudioCodec::Opus,
			sample_rate: self.sample_rate,
			channel_count: self.channel_count,
			bitrate: self.bitrate.map(|b| b as u64),
			description: Some(head),
			container: hang::catalog::Container::Legacy,
			jitter: None,
		}
	}

	fn frame_size(&self) -> usize {
		self.frame_size
	}

	fn sample_rate(&self) -> u32 {
		self.sample_rate
	}

	fn channel_count(&self) -> u32 {
		self.channel_count
	}

	fn encode(&mut self, pcm: &[f32]) -> Result<Bytes, AudioError> {
		let expected = self.frame_size * self.channel_count as usize;
		if pcm.len() != expected {
			return Err(AudioError::Misaligned {
				got: std::mem::size_of_val(pcm),
				expected: expected * std::mem::size_of::<f32>(),
			});
		}

		let n = self.inner.encode_float(pcm, &mut self.output)?;
		Ok(Bytes::copy_from_slice(&self.output[..n]))
	}
}

/// Opus decoder producing interleaved `f32` PCM.
pub struct OpusDecoder {
	inner: opus::Decoder,
	sample_rate: u32,
	channel_count: u32,
	max_frame_size: usize,
}

impl OpusDecoder {
	/// Build a new decoder.
	///
	/// Pass the sample rate and channel count from the hang catalog
	/// (parsed from the OpusHead `description`); defaults to 48 kHz / 2 ch
	/// if the description is missing.
	pub fn new(sample_rate: u32, channel_count: u32) -> Result<Self, AudioError> {
		validate_sample_rate(sample_rate)?;
		let channels = channels_for(channel_count)?;

		let inner = opus::Decoder::new(sample_rate, channels)?;
		// libopus packets cap at 120 ms of audio per RFC 6716.
		let max_frame_size = (sample_rate as usize * 120) / 1000;

		Ok(Self {
			inner,
			sample_rate,
			channel_count,
			max_frame_size,
		})
	}

	/// Build a decoder from a hang [`AudioConfig`](hang::catalog::AudioConfig).
	///
	/// Parses the OpusHead `description` if present; otherwise falls back
	/// to the catalog's declared sample rate / channel count.
	pub fn from_config(config: &hang::catalog::AudioConfig) -> Result<Self, AudioError> {
		if let Some(desc) = &config.description {
			let mut buf = desc.as_ref();
			if let Ok(head) = moq_mux::codec::opus::Config::parse(&mut buf) {
				return Self::new(head.sample_rate, head.channel_count);
			}
		}
		Self::new(config.sample_rate, config.channel_count)
	}
}

impl Decoder for OpusDecoder {
	fn sample_rate(&self) -> u32 {
		self.sample_rate
	}

	fn channel_count(&self) -> u32 {
		self.channel_count
	}

	fn decode(&mut self, packet: &[u8]) -> Result<Vec<f32>, AudioError> {
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
		let sr = 48_000;
		let ch = 2;
		let mut enc = OpusEncoder::new(sr, ch, Some(96_000)).unwrap();
		let mut dec = OpusDecoder::new(sr, ch).unwrap();

		let frame = sine(440.0, sr, ch, enc.frame_size());

		// Prime the encoder/decoder: Opus needs a few frames to stabilize.
		for _ in 0..5 {
			let pkt = enc.encode(&frame).unwrap();
			let _ = dec.decode(&pkt).unwrap();
		}

		let pkt = enc.encode(&frame).unwrap();
		assert!(!pkt.is_empty(), "encoder should produce a non-empty packet");

		let decoded = dec.decode(&pkt).unwrap();
		assert_eq!(decoded.len(), frame.len());

		let mut energy_in = 0.0f32;
		let mut energy_out = 0.0f32;
		for (&a, &b) in frame.iter().zip(decoded.iter()) {
			energy_in += a * a;
			energy_out += b * b;
		}
		let ratio = energy_out / energy_in;
		assert!(
			(0.5..2.0).contains(&ratio),
			"output energy {energy_out:.4} / input energy {energy_in:.4} = {ratio:.3} should be close to 1"
		);
	}

	#[test]
	fn opus_rejects_unsupported_sample_rate() {
		assert!(matches!(
			OpusEncoder::new(44_100, 2, None),
			Err(AudioError::Unsupported(_))
		));
	}

	#[test]
	fn opus_rejects_misaligned_input() {
		let mut enc = OpusEncoder::new(48_000, 2, None).unwrap();
		// frame_size = 960 frames * 2 channels = 1920 samples
		let too_short = vec![0.0f32; 100];
		assert!(matches!(enc.encode(&too_short), Err(AudioError::Misaligned { .. })));
	}

	#[test]
	fn opus_config_includes_opushead() {
		let enc = OpusEncoder::new(48_000, 2, Some(64_000)).unwrap();
		let cfg = enc.config();
		assert_eq!(cfg.sample_rate, 48_000);
		assert_eq!(cfg.channel_count, 2);
		assert_eq!(cfg.bitrate, Some(64_000));
		let desc = cfg.description.expect("OpusHead should be present");
		// 19-byte OpusHead per RFC 7845.
		assert_eq!(desc.len(), 19);
		let parsed = moq_mux::codec::opus::Config::parse(&mut desc.as_ref()).unwrap();
		assert_eq!(parsed.sample_rate, 48_000);
		assert_eq!(parsed.channel_count, 2);
	}
}
