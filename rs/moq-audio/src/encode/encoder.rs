//! Opus encoder front end.
//!
//! Single-codec implementation today: [`Encoder`] wraps libopus 1.3.1 via
//! [`unsafe_libopus`], a pure-Rust c2rust transpilation. No CMake toolchain, no
//! sys crate, no linker gymnastics. When AAC or other codecs land we'll factor
//! out a backend dispatch behind [`Codec`]; introducing a trait now would be
//! premature.

use std::str::FromStr;
use std::time::Duration;

use bytes::Bytes;
use unsafe_libopus::{
	OPUS_APPLICATION_AUDIO, OPUS_GET_BITRATE_REQUEST, OPUS_GET_LOOKAHEAD_REQUEST, OPUS_OK, OPUS_SET_BITRATE_REQUEST,
	OPUS_SET_DTX_REQUEST, OPUS_SET_INBAND_FEC_REQUEST, OpusEncoder, opus_encode_float, opus_encoder_create,
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
	/// Opus (RFC 6716). The only codec today, and the default.
	#[default]
	Opus,
}

impl Codec {
	/// Canonical lowercase identifier, matching the WebCodecs / RFC catalog
	/// string. Used as the wire/FFI codec name everywhere.
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
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"opus" => Ok(Self::Opus),
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
	/// Bitrate in bits per second. `None` lets the codec pick.
	pub bitrate: Option<u32>,
	/// Enable Opus in-band forward error correction.
	pub fec: bool,
	/// Enable Opus discontinuous transmission during silence.
	pub dtx: bool,
	/// Encoded frame duration. Opus accepts 2.5 / 5 / 10 / 20 / 40 / 60 ms.
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
			fec: false,
			dtx: false,
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
	inner: *mut OpusEncoder,
	config: Config,
	/// Resolved codec sample rate (from `config.sample_rate`, else the input rate
	/// snapped up to a supported one).
	codec_rate: u32,
	/// Resolved codec channel count (currently always the input's).
	codec_channels: u32,
	/// Current libopus target bitrate.
	bitrate: u64,
	/// Encoder lookahead expressed in the OpusHead 48 kHz timebase.
	pre_skip: u16,
	frame_size: usize,
	scratch: Vec<u8>,
}

// SAFETY: OpusEncoder is heap-allocated state owned exclusively by this
// struct; libopus encoder methods take a single &mut, so a unique
// owner is allowed to move it across threads.
unsafe impl Send for Encoder {}

impl Encoder {
	/// Open an encoder for `config`.
	pub fn new(config: &Config) -> Result<Self, Error> {
		match config.codec {
			Codec::Opus => Self::new_opus(config.clone()),
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

		let configured = Self::configure_opus(inner, &config, codec_rate, codec_channels);
		let (bitrate, pre_skip) = match configured {
			Ok(configured) => configured,
			Err(err) => {
				// SAFETY: `inner` was created above and not yet handed out.
				unsafe { opus_encoder_destroy(inner) };
				return Err(err);
			}
		};

		Ok(Self {
			inner,
			config,
			codec_rate,
			codec_channels,
			bitrate,
			pre_skip,
			frame_size,
			scratch: vec![0u8; MAX_PACKET_BYTES],
		})
	}

	fn configure_opus(
		inner: *mut OpusEncoder,
		config: &Config,
		codec_rate: u32,
		codec_channels: u32,
	) -> Result<(u64, u16), Error> {
		if let Some(bitrate) = config.bitrate {
			Self::set_opus_bitrate(inner, codec_channels, bitrate as u64)?;
		}
		Self::set_opus_ctl(
			inner,
			OPUS_SET_INBAND_FEC_REQUEST,
			i32::from(config.fec),
			"OPUS_SET_INBAND_FEC",
		)?;
		Self::set_opus_ctl(inner, OPUS_SET_DTX_REQUEST, i32::from(config.dtx), "OPUS_SET_DTX")?;

		let bitrate = Self::get_opus_ctl(inner, OPUS_GET_BITRATE_REQUEST, "OPUS_GET_BITRATE")?;
		let bitrate = u64::try_from(bitrate)
			.map_err(|_| Error::Unsupported(format!("Opus reported negative bitrate {bitrate}")))?;
		let lookahead = Self::get_opus_ctl(inner, OPUS_GET_LOOKAHEAD_REQUEST, "OPUS_GET_LOOKAHEAD")?;
		let lookahead = u64::try_from(lookahead)
			.map_err(|_| Error::Unsupported(format!("Opus reported negative lookahead {lookahead}")))?;
		let pre_skip = u16::try_from((lookahead * 48_000) / codec_rate as u64)
			.map_err(|_| Error::Unsupported(format!("Opus lookahead {lookahead} does not fit in OpusHead")))?;

		Ok((bitrate, pre_skip))
	}

	fn set_opus_bitrate(inner: *mut OpusEncoder, channels: u32, bitrate: u64) -> Result<(), Error> {
		let max = 300_000 * channels as u64;
		if !(500..=max).contains(&bitrate) {
			return Err(Error::Unsupported(format!(
				"Opus bitrate must be between 500 and {max} bits per second for {channels} channel(s), got {bitrate}"
			)));
		}
		Self::set_opus_ctl(inner, OPUS_SET_BITRATE_REQUEST, bitrate as i32, "OPUS_SET_BITRATE")
	}

	fn set_opus_ctl(inner: *mut OpusEncoder, request: i32, value: i32, name: &'static str) -> Result<(), Error> {
		// SAFETY: `inner` owns a live encoder and each request here expects one i32.
		let rc = unsafe { opus_encoder_ctl_impl(inner, request, varargs![value]) };
		if rc != OPUS_OK {
			return Err(opus::error(rc, name));
		}
		Ok(())
	}

	fn get_opus_ctl(inner: *mut OpusEncoder, request: i32, name: &'static str) -> Result<i32, Error> {
		let mut value = 0;
		// SAFETY: `inner` owns a live encoder and each request here expects one
		// valid mutable i32 output.
		let rc = unsafe { opus_encoder_ctl_impl(inner, request, varargs![&mut value]) };
		if rc != OPUS_OK {
			return Err(opus::error(rc, name));
		}
		Ok(value)
	}

	/// The encoder config, including the latest accepted runtime bitrate.
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

	/// Current target bitrate in bits per second.
	pub fn bitrate(&self) -> u64 {
		self.bitrate
	}

	/// Retune the live Opus encoder to `bitrate` bits per second.
	pub fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
		if bitrate == self.bitrate {
			return Ok(());
		}
		Self::set_opus_bitrate(self.inner, self.codec_channels, bitrate)?;
		self.bitrate = bitrate;
		self.config.bitrate = Some(bitrate as u32);
		Ok(())
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
			return Err(opus::error(n, "opus_encode_float"));
		}
		Ok(Bytes::copy_from_slice(&self.scratch[..n as usize]))
	}

	/// hang catalog entry describing this encoder's output stream.
	pub fn catalog(&self) -> hang::catalog::AudioConfig {
		// `codec_channels` is validated to mono/stereo at encoder construction, so the
		// OpusHead (channel mapping family 0) always encodes.
		let head = moq_mux::codec::opus::Config::new(self.codec_rate, self.codec_channels)
			.with_pre_skip(self.pre_skip)
			.encode()
			.expect("opus encoder channels validated to mono/stereo");

		let mut config =
			hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, self.codec_rate, self.codec_channels);
		config.bitrate = self.config.bitrate.map(u64::from);
		config.description = Some(head);
		config.container = hang::catalog::Container::Legacy;
		config
	}
}

impl Drop for Encoder {
	fn drop(&mut self) {
		// SAFETY: `inner` is a live OpusEncoder that nothing else aliases.
		unsafe { opus_encoder_destroy(self.inner) };
	}
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
		let head = moq_mux::codec::opus::Config::parse(&mut desc.as_ref()).unwrap();
		assert_eq!(head.pre_skip, enc.pre_skip);
		assert_eq!(head.pre_skip, 312);
	}

	#[test]
	fn opus_decoder_trims_encoder_lookahead_once() {
		let mut enc = Encoder::new(&Config::new(stereo_48k())).unwrap();
		let mut dec = Decoder::new(&enc.catalog()).unwrap();
		let frame = vec![0.0; enc.frame_size() * enc.codec_channels() as usize];

		let first = dec.decode(&enc.encode(&frame).unwrap()).unwrap();
		assert_eq!(
			first.len(),
			(enc.frame_size() - enc.pre_skip as usize) * enc.codec_channels() as usize
		);

		let second = dec.decode(&enc.encode(&frame).unwrap()).unwrap();
		assert_eq!(second.len(), frame.len());
	}

	#[test]
	fn opus_runtime_bitrate_updates_encoder_state() {
		let mut enc = Encoder::new(&Config {
			bitrate: Some(64_000),
			..Config::new(stereo_48k())
		})
		.unwrap();

		enc.set_bitrate(32_000).unwrap();
		assert_eq!(enc.bitrate(), 32_000);
		assert_eq!(enc.config().bitrate, Some(32_000));
		assert_eq!(
			Encoder::get_opus_ctl(enc.inner, unsafe_libopus::OPUS_GET_BITRATE_REQUEST, "OPUS_GET_BITRATE").unwrap(),
			32_000
		);
	}

	#[test]
	fn opus_runtime_bitrate_rejects_values_libopus_would_clamp() {
		let mut enc = Encoder::new(&Config::new(stereo_48k())).unwrap();
		let original = enc.bitrate();
		assert!(enc.set_bitrate(1).is_err());
		assert!(enc.set_bitrate(600_001).is_err());
		assert_eq!(enc.bitrate(), original);
	}

	#[test]
	fn opus_applies_fec_and_dtx_controls() {
		let enc = Encoder::new(&Config {
			fec: true,
			dtx: true,
			..Config::new(stereo_48k())
		})
		.unwrap();

		assert_eq!(
			Encoder::get_opus_ctl(
				enc.inner,
				unsafe_libopus::OPUS_GET_INBAND_FEC_REQUEST,
				"OPUS_GET_INBAND_FEC"
			)
			.unwrap(),
			1
		);
		assert_eq!(
			Encoder::get_opus_ctl(enc.inner, unsafe_libopus::OPUS_GET_DTX_REQUEST, "OPUS_GET_DTX").unwrap(),
			1
		);
	}

	#[test]
	fn codec_roundtrips_as_str() {
		assert_eq!(Codec::Opus.as_str(), "opus");
		assert_eq!(Codec::Opus.to_string(), "opus");
		assert_eq!("opus".parse::<Codec>().unwrap(), Codec::Opus);
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
		assert_eq!(enc.pre_skip, 312);
	}
}
