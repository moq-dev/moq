//! Audio encoder front end.
//!
//! [`Encoder`] dispatches over the closed [`Codec`] set. Opus wraps libopus
//! 1.3.1 via [`unsafe_libopus`], while PCM serializes interleaved `f32` samples
//! directly.

use std::str::FromStr;
use std::time::Duration;

use bytes::Bytes;
use unsafe_libopus::{
	OPUS_APPLICATION_AUDIO, OPUS_OK, OPUS_SET_BITRATE_REQUEST, OpusEncoder, opus_encode_float, opus_encoder_create,
	opus_encoder_ctl_impl, opus_encoder_destroy, varargs,
};

use crate::opus;
use crate::{Error, Format};

/// libopus packet size ceiling per RFC 6716 §3.4.
const MAX_PACKET_BYTES: usize = 4_000;

/// Output audio codec. `#[non_exhaustive]` so new codecs can be added without
/// breaking external `match`es.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Codec {
	/// Opus (RFC 6716), and the default.
	#[default]
	Opus,
	/// Uncompressed interleaved little-endian IEEE-754 binary32 PCM.
	Pcm,
}

impl Codec {
	/// Canonical lowercase identifier, matching the WebCodecs / RFC catalog
	/// string. Used as the wire/FFI codec name everywhere.
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Opus => "opus",
			Self::Pcm => "pcm",
		}
	}
}

impl std::fmt::Display for Codec {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.as_str())
	}
}

impl FromStr for Codec {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"opus" => Ok(Self::Opus),
			"pcm" => Ok(Self::Pcm),
			other => Err(Error::Unsupported(format!("unknown codec: {other}"))),
		}
	}
}

/// The PCM layout of the buffers handed to [`Encoder::encode`] /
/// [`Producer::write`](super::Producer::write).
///
/// The encoder's counterpart to a video encoder's width / height: it describes
/// the input, not the output. `publish_capture` fills it in from the capture
/// source, so only a bring-your-own-PCM caller builds one.
#[derive(Clone, Debug)]
pub struct Input {
	/// How samples are packed in each buffer.
	pub format: Format,
	/// Samples per second per channel. Resampled to the codec rate if they differ.
	pub sample_rate: u32,
	/// Channels per frame.
	pub channels: u32,
}

impl Default for Input {
	fn default() -> Self {
		Self {
			format: Format::F32,
			sample_rate: 48_000,
			channels: 2,
		}
	}
}

/// Encoder configuration: the input PCM layout plus the codec knobs.
///
/// The bring-your-own-PCM counterpart to [`Options`](super::Options), which
/// `publish_capture` uses when the layout comes from the capture source instead
/// of the caller.
///
/// `#[non_exhaustive]`: build via [`Config::new`] and set the optional fields,
/// so future knobs don't break callers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The PCM layout fed to the encoder.
	pub input: Input,
	/// Output codec. Defaults to [`Codec::Opus`].
	pub codec: Codec,
	/// Sample rate the codec runs at. `None` snaps [`Input::sample_rate`] up to
	/// the nearest rate the codec supports, resampling if that moved it.
	pub sample_rate: Option<u32>,
	/// Channel count the codec runs at. `None` matches [`Input::channels`];
	/// anything else is rejected, since remapping isn't implemented.
	pub channels: Option<u32>,
	/// Bitrate in bits per second. `None` lets Opus pick. PCM requires `None`
	/// because its bitrate is fixed by the sample rate and channel count.
	pub bitrate: Option<u32>,
	/// Encoded frame duration. Opus accepts 2.5 / 5 / 10 / 20 / 40 / 60 ms.
	/// PCM accepts any duration containing a whole number of samples.
	pub frame_duration: Duration,
}

impl Config {
	/// A config encoding `input` with the default codec settings.
	pub fn new(input: Input) -> Self {
		Self {
			input,
			codec: Codec::default(),
			sample_rate: None,
			channels: None,
			bitrate: None,
			frame_duration: Duration::from_millis(20),
		}
	}
}

/// Audio encoder over the PCM layout declared in [`Config::input`].
///
/// Build one with [`Encoder::new`], feed it PCM via [`encode`](Self::encode),
/// and publish the resulting packets through a [`Producer`](super::Producer)
/// built from the same [`Config`].
pub struct Encoder {
	backend: Backend,
	config: Config,
	/// Resolved codec sample rate (from `config.sample_rate`, else the input rate
	/// snapped up to a supported one).
	codec_rate: u32,
	/// Resolved codec channel count (currently always the input's).
	codec_channels: u32,
	frame_size: usize,
}

enum Backend {
	Opus(Opus),
	Pcm,
}

struct Opus {
	inner: *mut OpusEncoder,
	scratch: Vec<u8>,
}

// SAFETY: OpusEncoder is heap-allocated state owned exclusively by this
// struct; libopus encoder methods take a single &mut, so a unique owner is
// allowed to move it across threads.
unsafe impl Send for Opus {}

impl Encoder {
	/// Open an encoder for `config`.
	pub fn new(config: &Config) -> Result<Self, Error> {
		match config.codec {
			Codec::Opus => Self::new_opus(config.clone()),
			Codec::Pcm => Self::new_pcm(config.clone()),
		}
	}

	fn new_opus(config: Config) -> Result<Self, Error> {
		let codec_rate = config
			.sample_rate
			.unwrap_or_else(|| opus::pick_rate(config.input.sample_rate));
		opus::validate_rate(codec_rate)?;

		let codec_channels = config.channels.unwrap_or(config.input.channels);
		if codec_channels != config.input.channels {
			return Err(Error::Unsupported(format!(
				"channel remapping not implemented (input {}ch, output {codec_channels}ch)",
				config.input.channels
			)));
		}
		let channels = opus::validate_channels(codec_channels)?;

		let frame_size = opus::frame_size(codec_rate, config.frame_duration)?;

		let mut err = 0i32;
		// SAFETY: out-pointer `err` is valid; inner is checked for null below.
		let inner = unsafe { opus_encoder_create(codec_rate as i32, channels, OPUS_APPLICATION_AUDIO, &mut err) };
		if err != OPUS_OK || inner.is_null() {
			return Err(opus::error(err, "opus_encoder_create"));
		}

		if let Some(b) = config.bitrate {
			// SAFETY: `inner` is a freshly-created encoder; varargs! produces
			// the single i32 the SET_BITRATE request expects.
			let rc = unsafe { opus_encoder_ctl_impl(inner, OPUS_SET_BITRATE_REQUEST, varargs![b as i32]) };
			if rc != OPUS_OK {
				// SAFETY: `inner` was created above and not yet handed out.
				unsafe { opus_encoder_destroy(inner) };
				return Err(opus::error(rc, "OPUS_SET_BITRATE"));
			}
		}

		Ok(Self {
			backend: Backend::Opus(Opus {
				inner,
				scratch: vec![0u8; MAX_PACKET_BYTES],
			}),
			config,
			codec_rate,
			codec_channels,
			frame_size,
		})
	}

	fn new_pcm(config: Config) -> Result<Self, Error> {
		if config.bitrate.is_some() {
			return Err(Error::Unsupported(
				"pcm bitrate is fixed; leave Config::bitrate unset".into(),
			));
		}

		let codec_rate = config.sample_rate.unwrap_or(config.input.sample_rate);
		if codec_rate == 0 {
			return Err(Error::Unsupported("pcm sample rate must be greater than zero".into()));
		}

		let codec_channels = config.channels.unwrap_or(config.input.channels);
		if codec_channels == 0 {
			return Err(Error::Unsupported("pcm channel count must be greater than zero".into()));
		}
		if codec_channels != config.input.channels {
			return Err(Error::Unsupported(format!(
				"channel remapping not implemented (input {}ch, output {codec_channels}ch)",
				config.input.channels
			)));
		}

		let frame_size = pcm_frame_size(codec_rate, config.frame_duration)?;
		frame_size
			.checked_mul(codec_channels as usize)
			.ok_or_else(|| Error::Unsupported("pcm frame contains too many samples".into()))?;
		pcm_bitrate(codec_rate, codec_channels)?;
		Ok(Self {
			backend: Backend::Pcm,
			config,
			codec_rate,
			codec_channels,
			frame_size,
		})
	}

	/// The config this encoder opened with.
	pub fn config(&self) -> &Config {
		&self.config
	}

	/// The codec this encoder emits. A [`Producer`](super::Producer) must be
	/// built for the same codec to publish its packets.
	pub fn codec(&self) -> Codec {
		self.config.codec
	}

	/// Sample rate the codec actually runs at, which is
	/// [`Config::sample_rate`] resolved.
	pub fn codec_rate(&self) -> u32 {
		self.codec_rate
	}

	/// Channel count the codec actually runs at, which is
	/// [`Config::channels`] resolved.
	pub fn codec_channels(&self) -> u32 {
		self.codec_channels
	}

	/// Number of samples per channel the codec consumes per call to
	/// [`encode`](Self::encode).
	pub fn frame_size(&self) -> usize {
		self.frame_size
	}

	/// Encode one frame of interleaved `f32` PCM at [`codec_rate`](Self::codec_rate).
	///
	/// `pcm.len()` must equal `frame_size() * codec_channels()`. The
	/// [`Producer`](super::Producer) handles format conversion and resampling
	/// before calling this; for direct use, the caller does the same.
	pub fn encode(&mut self, pcm: &[f32]) -> Result<Bytes, Error> {
		let expected = self.frame_size * self.codec_channels as usize;
		if pcm.len() != expected {
			return Err(Error::Misaligned {
				got: std::mem::size_of_val(pcm),
				expected: expected * std::mem::size_of::<f32>(),
			});
		}
		match &mut self.backend {
			Backend::Opus(opus) => {
				// SAFETY: `inner` owns a live OpusEncoder; pcm and scratch slices
				// are bounded by the lengths we pass.
				let n = unsafe {
					opus_encode_float(
						opus.inner,
						pcm.as_ptr(),
						self.frame_size as i32,
						opus.scratch.as_mut_ptr(),
						opus.scratch.len() as i32,
					)
				};
				if n < 0 {
					return Err(crate::opus::error(n, "opus_encode_float"));
				}
				Ok(Bytes::copy_from_slice(&opus.scratch[..n as usize]))
			}
			Backend::Pcm => {
				let mut payload = Vec::with_capacity(std::mem::size_of_val(pcm));
				for sample in pcm {
					payload.extend_from_slice(&sample.to_le_bytes());
				}
				Ok(payload.into())
			}
		}
	}

	/// hang catalog entry describing this encoder's output stream.
	pub fn catalog(&self) -> hang::catalog::AudioConfig {
		match self.config.codec {
			Codec::Opus => {
				// `codec_channels` is validated to mono/stereo at encoder construction,
				// so the OpusHead (channel mapping family 0) always encodes.
				let head = moq_mux::codec::opus::Config {
					sample_rate: self.codec_rate,
					channel_count: self.codec_channels,
				}
				.encode()
				.expect("opus encoder channels validated to mono/stereo");

				let mut config = hang::catalog::AudioConfig::new(
					hang::catalog::AudioCodec::Opus,
					self.codec_rate,
					self.codec_channels,
				);
				config.bitrate = self.config.bitrate.map(|b| b as u64);
				config.description = Some(head);
				config.container = hang::catalog::Container::Legacy;
				config
			}
			Codec::Pcm => {
				let mut config = hang::catalog::AudioConfig::new(
					hang::catalog::AudioCodec::Pcm,
					self.codec_rate,
					self.codec_channels,
				);
				config.bitrate = Some(
					pcm_bitrate(self.codec_rate, self.codec_channels)
						.expect("pcm encoder bitrate validated at construction"),
				);
				config.container = hang::catalog::Container::Legacy;
				config
			}
		}
	}
}

impl Drop for Opus {
	fn drop(&mut self) {
		// SAFETY: `inner` is a live OpusEncoder that nothing else aliases.
		unsafe { opus_encoder_destroy(self.inner) };
	}
}

fn pcm_frame_size(sample_rate: u32, duration: Duration) -> Result<usize, Error> {
	let samples = u128::from(sample_rate) * duration.as_nanos();
	if samples == 0 || !samples.is_multiple_of(1_000_000_000) {
		return Err(Error::Unsupported(format!(
			"pcm frame duration must contain a whole number of samples at {sample_rate} Hz"
		)));
	}
	usize::try_from(samples / 1_000_000_000).map_err(|_| Error::Unsupported("pcm frame duration is too large".into()))
}

fn pcm_bitrate(sample_rate: u32, channels: u32) -> Result<u64, Error> {
	u64::from(sample_rate)
		.checked_mul(u64::from(channels))
		.and_then(|samples| samples.checked_mul(32))
		.ok_or_else(|| Error::Unsupported("pcm bitrate exceeds the catalog range".into()))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::decode::Decoder;

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

	fn stereo_48k() -> Input {
		Input {
			format: Format::F32,
			sample_rate: 48_000,
			channels: 2,
		}
	}

	#[test]
	fn opus_encode_then_decode_keeps_signal_close() {
		let mut enc = Encoder::new(&Config {
			bitrate: Some(96_000),
			..Config::new(stereo_48k())
		})
		.unwrap();

		let cfg = enc.catalog();
		let mut dec = Decoder::new(&cfg).unwrap();

		let frame = sine(440.0, 48_000, 2, enc.frame_size());
		for _ in 0..5 {
			let pkt = enc.encode(&frame).unwrap();
			let _ = dec.decode(&pkt).unwrap();
		}

		let pkt = enc.encode(&frame).unwrap();
		let decoded = dec.decode(&pkt).unwrap();
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
		let err = Encoder::new(&Config {
			frame_duration: Duration::from_millis(15),
			..Config::new(Input::default())
		});
		assert!(matches!(err, Err(Error::Unsupported(_))));
	}

	#[test]
	fn opus_rejects_misaligned_input() {
		let mut enc = Encoder::new(&Config::new(Input::default())).unwrap();
		assert!(matches!(enc.encode(&[0.0f32; 100]), Err(Error::Misaligned { .. })));
	}

	#[test]
	fn opus_catalog_includes_opushead() {
		let enc = Encoder::new(&Config {
			bitrate: Some(64_000),
			..Config::new(stereo_48k())
		})
		.unwrap();
		let cfg = enc.catalog();
		assert_eq!(cfg.sample_rate, 48_000);
		assert_eq!(cfg.channel_count, 2);
		assert_eq!(cfg.bitrate, Some(64_000));
		let desc = cfg.description.expect("OpusHead should be present");
		assert_eq!(desc.len(), 19);
	}

	#[test]
	fn codec_roundtrips_as_str() {
		assert_eq!(Codec::Opus.as_str(), "opus");
		assert_eq!(Codec::Opus.to_string(), "opus");
		assert_eq!("opus".parse::<Codec>().unwrap(), Codec::Opus);
		assert_eq!(Codec::Pcm.as_str(), "pcm");
		assert_eq!(Codec::Pcm.to_string(), "pcm");
		assert_eq!("pcm".parse::<Codec>().unwrap(), Codec::Pcm);
		assert!("aac".parse::<Codec>().is_err());
	}

	#[test]
	fn config_sample_rate_overrides_the_codec_rate() {
		let enc = Encoder::new(&Config {
			sample_rate: Some(24_000),
			..Config::new(Input {
				sample_rate: 48_000,
				channels: 1,
				..Input::default()
			})
		})
		.unwrap();
		assert_eq!(enc.codec_rate(), 24_000);
		assert_eq!(enc.catalog().sample_rate, 24_000);
	}

	#[test]
	fn pcm_roundtrip_is_lossless() {
		let mut enc = Encoder::new(&Config {
			codec: Codec::Pcm,
			..Config::new(stereo_48k())
		})
		.unwrap();
		let mut dec = Decoder::new(&enc.catalog()).unwrap();
		let input = sine(440.0, enc.codec_rate(), enc.codec_channels(), enc.frame_size());

		let packet = enc.encode(&input).unwrap();
		let output = dec.decode(&packet).unwrap();

		assert_eq!(output, input);
	}

	#[test]
	fn pcm_catalog_declares_fixed_bitrate() {
		let enc = Encoder::new(&Config {
			codec: Codec::Pcm,
			..Config::new(stereo_48k())
		})
		.unwrap();
		let catalog = enc.catalog();

		assert_eq!(catalog.codec, hang::catalog::AudioCodec::Pcm);
		assert_eq!(catalog.bitrate, Some(48_000 * 2 * 32));
		assert_eq!(catalog.description, None);
	}

	#[test]
	fn pcm_rejects_fractional_sample_frame_duration() {
		let err = Encoder::new(&Config {
			codec: Codec::Pcm,
			frame_duration: Duration::from_micros(2_500),
			..Config::new(Input {
				sample_rate: 44_100,
				..Input::default()
			})
		});
		assert!(matches!(err, Err(Error::Unsupported(_))));
	}

	#[test]
	fn pcm_rejects_bitrate_overflow() {
		let err = Encoder::new(&Config {
			codec: Codec::Pcm,
			frame_duration: Duration::from_secs(1),
			..Config::new(Input {
				sample_rate: u32::MAX,
				channels: u32::MAX,
				..Input::default()
			})
		});
		assert!(matches!(err, Err(Error::Unsupported(_))));
	}
}
