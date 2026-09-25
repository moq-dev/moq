//! Audio encoder front end.
//!
//! [`Encoder`] dispatches over the closed [`Codec`] set. Opus wraps libopus
//! 1.3.1 via [`unsafe_libopus`], while PCM serializes interleaved `f32` samples
//! directly.

use std::str::FromStr;
use std::time::Duration;

use bytes::Bytes;
use unsafe_libopus::{
	OPUS_APPLICATION_AUDIO, OPUS_GET_BITRATE_REQUEST, OPUS_GET_LOOKAHEAD_REQUEST, OPUS_OK, OPUS_RESET_STATE,
	OPUS_SET_BITRATE_REQUEST, OPUS_SET_DTX_REQUEST, OpusEncoder, opus_encode_float, opus_encoder_create,
	opus_encoder_ctl_impl, opus_encoder_destroy, varargs,
};

use super::Encoded;
use crate::opus;
use crate::pcm;
use crate::{Error, Format, Layout};

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

/// PCM supplied to [`Producer::write`](super::Producer::write).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Input {
	/// How samples are packed in each buffer.
	pub format: Format,
	/// Samples per second per channel.
	pub sample_rate: u32,
	/// Speaker meaning and channel order.
	pub layout: Layout,
}

impl Input {
	/// Describe interleaved `f32` PCM at `sample_rate` in `layout`.
	pub fn new(sample_rate: u32, layout: Layout) -> Self {
		Self {
			format: Format::F32,
			sample_rate,
			layout,
		}
	}
}

impl Default for Input {
	fn default() -> Self {
		Self::new(48_000, Layout::Stereo)
	}
}

/// How an encoder trades latency for compression, applied with
/// [`Settings::with_preset`].
///
/// Mirrors `moq_video::encode::Preset`. A preset sets packetization only:
/// codec, sample rate, layout, bitrate, and DTX stay as configured. The
/// packet duration is a packetization setting, not a delay guarantee: Opus adds
/// its own 6.5 ms lookahead, and transport and playout buffering are separate.
///
/// `#[non_exhaustive]` so a later policy can be added without breaking a
/// `match`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Preset {
	/// 10 ms packets: the shortest packetization Opus codes with its full
	/// toolset, at twice the packet rate.
	#[default]
	LowLatency,
	/// 20 ms packets, the Opus default.
	Balanced,
	/// 20 ms packets. Identical to [`Balanced`](Self::Balanced) today:
	/// libopus already runs at full complexity, and a longer packet would only
	/// add delay.
	Quality,
}

impl Preset {
	/// The packet duration this preset encodes.
	pub fn frame_duration(self) -> Duration {
		match self {
			Self::LowLatency => Duration::from_millis(10),
			Self::Balanced | Self::Quality => Duration::from_millis(20),
		}
	}
}

/// Audio codec settings shared by [`Encoder`] and [`Producer`](super::Producer).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Settings {
	/// Output codec. Defaults to [`Codec::Opus`].
	pub codec: Codec,
	/// Sample rate accepted by the codec.
	pub sample_rate: u32,
	/// Layout accepted by the codec.
	pub layout: Layout,
	/// Bitrate in bits per second. `None` lets Opus pick. PCM requires `None`
	/// because its bitrate is fixed by the sample rate and channel count.
	///
	/// Rates too low for Opus to code anything at the chosen
	/// [`frame_duration`](Self::frame_duration) are rejected. The floor is 1200
	/// bps at the default 20 ms, rises for shorter frames, and is 2400 bps for
	/// frames of 10 ms and longer.
	pub bitrate: Option<moq_net::bandwidth::Rate>,
	/// Enable Opus discontinuous transmission during silence.
	pub dtx: bool,
	/// Encoded frame duration. Opus accepts 2.5 / 5 / 10 / 20 / 40 / 60 ms.
	/// PCM accepts any duration containing a whole number of samples.
	pub frame_duration: Duration,
}

impl Settings {
	/// Build default Opus settings for `sample_rate` and `layout`.
	pub fn new(sample_rate: u32, layout: Layout) -> Self {
		Self {
			codec: Codec::default(),
			sample_rate,
			layout,
			bitrate: None,
			dtx: false,
			frame_duration: Duration::from_millis(20),
		}
	}

	/// Apply `preset`'s packetization, keeping every other setting.
	pub fn with_preset(mut self, preset: Preset) -> Self {
		self.frame_duration = preset.frame_duration();
		self
	}

	/// Derive concrete codec settings from source PCM.
	pub fn from_input(codec: Codec, input: &Input) -> Self {
		let sample_rate = match codec {
			Codec::Opus => opus::pick_rate(input.sample_rate),
			Codec::Pcm => input.sample_rate,
		};
		Self {
			codec,
			sample_rate,
			layout: input.layout,
			..Self::default()
		}
	}
}

impl Default for Settings {
	fn default() -> Self {
		Self::new(48_000, Layout::Stereo)
	}
}

/// Audio encoder over codec-sized interleaved `f32` PCM.
///
/// Build one with [`Encoder::new`], feed full PCM frames via
/// [`encode`](Self::encode), then pass the trailing partial frame to
/// [`finish`](Self::finish). Publish every packet either call returns and apply
/// the terminal [`Finish::discard_padding`] when the container supports it.
pub struct Encoder {
	backend: Backend,
	settings: Settings,
	/// Codec sample rate.
	codec_rate: u32,
	/// Codec channel count.
	codec_channels: u32,
	/// Current libopus target bitrate.
	bitrate: u64,
	/// Encoder lookahead expressed in the OpusHead 48 kHz timebase.
	pre_skip: u16,
	/// Encoder lookahead in codec-rate frames.
	lookahead: usize,
	frame_size: usize,
	/// Whether input has reached the codec, since a fresh encoder owes no drain.
	started: bool,
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

/// Packets emitted by [`Encoder::finish`] and the decoded padding at their end.
pub struct Finish {
	packets: Vec<Encoded>,
	discard_padding: usize,
}

impl Finish {
	/// Encoded packets in decode order.
	pub fn packets(&self) -> &[Encoded] {
		&self.packets
	}

	/// Decoded frames per channel to discard from the end of the final packet.
	pub fn discard_padding(&self) -> usize {
		self.discard_padding
	}

	/// Consume the result and return its encoded packets.
	pub fn into_packets(self) -> Vec<Encoded> {
		self.packets
	}
}

impl Encoder {
	/// Open an encoder for `settings`.
	pub fn new(settings: &Settings) -> Result<Self, Error> {
		settings.layout.validate()?;
		match settings.codec {
			Codec::Opus => Self::new_opus(settings.clone()),
			Codec::Pcm => Self::new_pcm(settings.clone()),
		}
	}

	fn new_opus(settings: Settings) -> Result<Self, Error> {
		let codec_rate = settings.sample_rate;
		opus::validate_rate(codec_rate)?;

		let codec_channels = settings.layout.channels();
		if !matches!(settings.layout, Layout::Mono | Layout::Stereo) {
			return Err(Error::Unsupported("opus requires a named mono or stereo layout".into()));
		}
		let channels = opus::validate_channels(codec_channels)?;

		let frame_size = opus::frame_size(codec_rate, settings.frame_duration)?;

		let mut err = 0i32;
		// SAFETY: out-pointer `err` is valid; inner is checked for null below.
		let inner = unsafe { opus_encoder_create(codec_rate as i32, channels, OPUS_APPLICATION_AUDIO, &mut err) };
		if err != OPUS_OK || inner.is_null() {
			return Err(opus::error(err, "opus_encoder_create"));
		}

		let configured = Self::configure_opus(inner, &settings, codec_rate, codec_channels, frame_size);
		let (bitrate, lookahead, pre_skip) = match configured {
			Ok(configured) => configured,
			Err(err) => {
				// SAFETY: `inner` was created above and not yet handed out.
				unsafe { opus_encoder_destroy(inner) };
				return Err(err);
			}
		};

		Ok(Self {
			backend: Backend::Opus(Opus {
				inner,
				scratch: vec![0u8; MAX_PACKET_BYTES],
			}),
			settings,
			codec_rate,
			codec_channels,
			bitrate,
			pre_skip,
			lookahead,
			frame_size,
			started: false,
		})
	}

	fn new_pcm(settings: Settings) -> Result<Self, Error> {
		if settings.bitrate.is_some() {
			return Err(Error::Unsupported(
				"pcm bitrate is fixed; leave Settings::bitrate unset".into(),
			));
		}
		if settings.dtx {
			return Err(Error::Unsupported(
				"pcm does not support discontinuous transmission".into(),
			));
		}

		let codec_rate = settings.sample_rate;
		if codec_rate == 0 {
			return Err(Error::Unsupported("pcm sample rate must be greater than zero".into()));
		}

		let codec_channels = settings.layout.channels();
		if codec_channels == 0 {
			return Err(Error::Unsupported("pcm channel count must be greater than zero".into()));
		}
		let frame_size = pcm::frame_size(codec_rate, settings.frame_duration)?;
		pcm::frame_bytes(frame_size, codec_channels)?;
		let bitrate = pcm::bitrate(codec_rate, codec_channels)?;
		Ok(Self {
			backend: Backend::Pcm,
			settings,
			codec_rate,
			codec_channels,
			bitrate,
			pre_skip: 0,
			lookahead: 0,
			frame_size,
			started: false,
		})
	}

	fn configure_opus(
		inner: *mut OpusEncoder,
		settings: &Settings,
		codec_rate: u32,
		codec_channels: u32,
		frame_size: usize,
	) -> Result<(u64, usize, u16), Error> {
		if let Some(bitrate) = settings.bitrate {
			Self::set_opus_bitrate(inner, codec_channels, bitrate.as_bps(), codec_rate, frame_size)?;
		}
		Self::set_opus_ctl(inner, OPUS_SET_DTX_REQUEST, i32::from(settings.dtx), "OPUS_SET_DTX")?;

		let bitrate = Self::get_opus_ctl(inner, OPUS_GET_BITRATE_REQUEST, "OPUS_GET_BITRATE")?;
		let bitrate = u64::try_from(bitrate)
			.map_err(|_| Error::Unsupported(format!("Opus reported negative bitrate {bitrate}")))?;
		let lookahead = Self::get_opus_ctl(inner, OPUS_GET_LOOKAHEAD_REQUEST, "OPUS_GET_LOOKAHEAD")?;
		let lookahead = u64::try_from(lookahead)
			.map_err(|_| Error::Unsupported(format!("Opus reported negative lookahead {lookahead}")))?;
		let pre_skip = u16::try_from((lookahead * 48_000) / codec_rate as u64)
			.map_err(|_| Error::Unsupported(format!("Opus lookahead {lookahead} does not fit in OpusHead")))?;
		let lookahead = usize::try_from(lookahead)
			.map_err(|_| Error::Unsupported(format!("Opus lookahead {lookahead} does not fit in memory")))?;

		Ok((bitrate, lookahead, pre_skip))
	}

	fn set_opus_bitrate(
		inner: *mut OpusEncoder,
		channels: u32,
		bitrate: u64,
		codec_rate: u32,
		frame_size: usize,
	) -> Result<(), Error> {
		let max = 300_000 * channels as u64;
		let min = opus::bitrate_floor(codec_rate, frame_size).max(500);
		if !(min..=max).contains(&bitrate) {
			return Err(Error::Unsupported(format!(
				"Opus bitrate must be between {min} and {max} bits per second for {channels} channel(s) at {frame_size} samples, got {bitrate}"
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

	/// The encoder settings, including the latest accepted runtime bitrate.
	pub fn settings(&self) -> &Settings {
		&self.settings
	}

	/// The codec this encoder emits. A [`Producer`](super::Producer) must be
	/// built for the same codec to publish its packets.
	pub fn codec(&self) -> Codec {
		self.settings.codec
	}

	/// Sample rate the codec actually runs at, which is
	/// [`Settings::sample_rate`].
	pub fn codec_rate(&self) -> u32 {
		self.codec_rate
	}

	/// Channel count the codec actually runs at, which is
	/// [`Settings::layout`]'s channel count.
	pub fn codec_channels(&self) -> u32 {
		self.codec_channels
	}

	/// Number of samples per channel the codec consumes per call to
	/// [`encode`](Self::encode).
	pub fn frame_size(&self) -> usize {
		self.frame_size
	}

	/// Current target bitrate.
	pub fn bitrate(&self) -> moq_net::bandwidth::Rate {
		moq_net::bandwidth::Rate::from_bps(self.bitrate)
	}

	/// Retune the live Opus encoder to `bitrate`.
	pub fn set_bitrate(&mut self, bitrate: moq_net::bandwidth::Rate) -> Result<(), Error> {
		let Backend::Opus(opus) = &mut self.backend else {
			return Err(Error::Unsupported("pcm bitrate is fixed".into()));
		};
		if bitrate.as_bps() != self.bitrate {
			Self::set_opus_bitrate(
				opus.inner,
				self.codec_channels,
				bitrate.as_bps(),
				self.codec_rate,
				self.frame_size,
			)?;
			self.bitrate = bitrate.as_bps();
			self.settings.bitrate = Some(bitrate);
		}
		Ok(())
	}

	/// Drop all codec history so a later epoch cannot emit audio from this one.
	pub(super) fn reset(&mut self) {
		if let Backend::Opus(opus) = &mut self.backend {
			// SAFETY: `inner` owns a live encoder and OPUS_RESET_STATE takes no arguments.
			let rc = unsafe { opus_encoder_ctl_impl(opus.inner, OPUS_RESET_STATE, varargs![]) };
			debug_assert_eq!(rc, OPUS_OK, "OPUS_RESET_STATE failed with {rc}");
		}
		self.started = false;
	}

	/// Whether this epoch has submitted audio to the codec.
	pub(super) fn started(&self) -> bool {
		self.started
	}

	/// Encode one frame of interleaved `f32` PCM at [`codec_rate`](Self::codec_rate).
	///
	/// `pcm.len()` must equal `frame_size() * codec_channels()`. The
	/// [`Producer`](super::Producer) handles format conversion and resampling
	/// before calling this; for direct use, the caller does the same.
	pub fn encode(&mut self, pcm: &[f32]) -> Result<Encoded, Error> {
		let expected = self.frame_size * self.codec_channels as usize;
		if pcm.len() != expected {
			return Err(Error::Misaligned {
				got: std::mem::size_of_val(pcm),
				expected: expected * std::mem::size_of::<f32>(),
			});
		}
		let encoded = match &mut self.backend {
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
				let payload = Bytes::copy_from_slice(&opus.scratch[..n as usize]);
				let activity = crate::opus::activity(&payload, false);
				Encoded { payload, activity }
			}
			Backend::Pcm => {
				let mut payload = Vec::with_capacity(std::mem::size_of_val(pcm));
				for sample in pcm {
					payload.extend_from_slice(&sample.to_le_bytes());
				}
				Encoded::new(payload.into())
			}
		};
		self.started = true;
		Ok(encoded)
	}

	/// Finish encoding, zero-padding `pcm` as the final partial frame and
	/// returning every packet needed to drain codec lookahead.
	///
	/// `pcm` is interleaved at [`codec_rate`](Self::codec_rate), may be empty,
	/// and must contain at most one frame. Silence added here only drains
	/// audio already supplied; [`Finish::discard_padding`] reports how much of
	/// the decoded tail is artificial and must not count as source duration.
	/// Consuming the encoder prevents encoding across the artificial terminal
	/// padding.
	pub fn finish(mut self, pcm: &[f32]) -> Result<Finish, Error> {
		self.drain(pcm)
	}

	/// Same drain as [`finish`](Self::finish), without consuming the encoder.
	pub(super) fn drain(&mut self, pcm: &[f32]) -> Result<Finish, Error> {
		let channels = self.codec_channels as usize;
		let frame_samples = self.frame_size * channels;
		if pcm.len() > frame_samples || !pcm.len().is_multiple_of(channels) {
			return Err(Error::Misaligned {
				got: std::mem::size_of_val(pcm),
				expected: if pcm.len() > frame_samples {
					frame_samples * std::mem::size_of::<f32>()
				} else {
					pcm.len().next_multiple_of(channels) * std::mem::size_of::<f32>()
				},
			});
		}

		let source_frames = pcm.len() / channels;
		let mut packets = Vec::new();
		let padding = if pcm.is_empty() {
			0
		} else {
			let mut frame = Vec::with_capacity(frame_samples);
			frame.extend_from_slice(pcm);
			frame.resize(frame_samples, 0.0);
			let padding = (frame_samples - pcm.len()) / channels;
			packets.push(self.encode(&frame)?);
			padding
		};

		if !self.started {
			return Ok(Finish {
				packets,
				discard_padding: 0,
			});
		}

		let drain = self.lookahead.saturating_sub(padding);
		let silence = vec![0.0; frame_samples];
		for _ in 0..drain.div_ceil(self.frame_size) {
			packets.push(self.encode(&silence)?);
		}

		let discard_padding = packets
			.len()
			.saturating_mul(self.frame_size)
			.saturating_sub(self.lookahead)
			.saturating_sub(source_frames);

		Ok(Finish {
			packets,
			discard_padding,
		})
	}

	/// hang catalog entry describing this encoder's output stream.
	pub fn catalog(&self) -> hang::catalog::AudioConfig {
		match self.settings.codec {
			Codec::Opus => {
				// `codec_channels` is validated to mono/stereo at encoder construction,
				// so the OpusHead (channel mapping family 0) always encodes.
				let head = moq_mux::codec::opus::Config::new(self.codec_rate, self.codec_channels)
					.with_pre_skip(self.pre_skip)
					.encode()
					.expect("opus encoder channels validated to mono/stereo");

				let mut config = hang::catalog::AudioConfig::new(
					hang::catalog::AudioCodec::Opus,
					self.codec_rate,
					self.codec_channels,
				);
				config.bitrate = self.settings.bitrate.map(moq_net::bandwidth::Rate::as_bps);
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
					pcm::bitrate(self.codec_rate, self.codec_channels)
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

#[cfg(test)]
mod tests {
	use super::*;
	use crate::decode::{Config as DecodeConfig, Decoder};

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

	fn opus_inner(encoder: &Encoder) -> *mut OpusEncoder {
		let Backend::Opus(opus) = &encoder.backend else {
			panic!("expected Opus encoder");
		};
		opus.inner
	}

	#[test]
	fn opus_encode_then_decode_keeps_signal_close() {
		let mut enc = Encoder::new(&Settings {
			bitrate: Some(moq_net::bandwidth::Rate::from_bps(96_000)),
			..Settings::default()
		})
		.unwrap();

		let cfg = enc.catalog();
		let mut dec = Decoder::new(&cfg, &DecodeConfig::default()).unwrap();

		let frame = sine(440.0, 48_000, 2, enc.frame_size());
		for _ in 0..5 {
			let pkt = enc.encode(&frame).unwrap();
			let _ = dec.decode(&pkt.payload).unwrap();
		}

		let pkt = enc.encode(&frame).unwrap();
		let decoded = dec.decode(&pkt.payload).unwrap();
		assert_eq!(decoded.samples.len(), frame.len());

		let energy_in: f32 = frame.iter().map(|s| s * s).sum();
		let energy_out: f32 = decoded.samples.iter().map(|s| s * s).sum();
		let ratio = energy_out / energy_in;
		assert!(
			(0.5..2.0).contains(&ratio),
			"output energy ratio {ratio:.3} should be close to 1"
		);
	}

	#[test]
	fn opus_rejects_unsupported_frame_duration() {
		let err = Encoder::new(&Settings {
			frame_duration: Duration::from_millis(15),
			..Settings::default()
		});
		assert!(matches!(err, Err(Error::Unsupported(_))));
	}

	/// A preset has to reach the codec as its packet duration, read back off the
	/// packet's TOC rather than our own settings, and leave everything else alone.
	#[test]
	fn preset_sets_the_packet_duration_and_keeps_the_rest() {
		for (preset, samples) in [
			(Preset::LowLatency, 480),
			(Preset::Balanced, 960),
			(Preset::Quality, 960),
		] {
			let settings = Settings {
				bitrate: Some(moq_net::bandwidth::Rate::from_bps(96_000)),
				layout: Layout::Mono,
				..Settings::default()
			}
			.with_preset(preset);
			let mut enc = Encoder::new(&settings).unwrap();
			assert_eq!(enc.settings().layout, Layout::Mono);
			assert_eq!(enc.bitrate().as_bps(), 96_000);
			assert_eq!(enc.frame_size(), samples);

			let packet = enc.encode(&vec![0.0f32; samples]).unwrap();
			// SAFETY: a non-empty packet from the encoder above; the TOC is its first byte.
			let coded = unsafe { unsafe_libopus::opus_packet_get_samples_per_frame(packet.payload.as_ptr(), 48_000) };
			assert_eq!(coded as usize, samples, "{preset:?} packet");
		}
	}

	#[test]
	fn opus_rejects_misaligned_input() {
		let mut enc = Encoder::new(&Settings::default()).unwrap();
		assert!(matches!(enc.encode(&[0.0f32; 100]), Err(Error::Misaligned { .. })));
	}

	#[test]
	fn opus_catalog_includes_opushead() {
		let enc = Encoder::new(&Settings {
			bitrate: Some(moq_net::bandwidth::Rate::from_bps(64_000)),
			..Settings::default()
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
		let mut enc = Encoder::new(&Settings::default()).unwrap();
		let mut dec = Decoder::new(&enc.catalog(), &DecodeConfig::default()).unwrap();
		let frame = vec![0.0; enc.frame_size() * enc.codec_channels() as usize];

		let first = dec.decode(&enc.encode(&frame).unwrap().payload).unwrap();
		assert_eq!(
			first.samples.len(),
			(enc.frame_size() - enc.pre_skip as usize) * enc.codec_channels() as usize
		);

		let second = dec.decode(&enc.encode(&frame).unwrap().payload).unwrap();
		assert_eq!(second.samples.len(), frame.len());
	}

	#[test]
	fn opus_finish_accounts_for_partial_frame_padding() {
		let enc = Encoder::new(&Settings::new(48_000, Layout::Mono)).unwrap();

		// The 360 frames of terminal padding exceed the 312-frame lookahead,
		// so the partial packet itself completes the drain.
		let packets = enc.finish(&vec![0.0; 600]).unwrap();
		assert_eq!(packets.packets().len(), 1);
		assert_eq!(packets.discard_padding(), 48);
	}

	#[test]
	fn opus_finish_drains_lookahead_across_multiple_short_packets() {
		let mut enc = Encoder::new(&Settings {
			frame_duration: Duration::from_micros(2_500),
			..Settings::new(48_000, Layout::Mono)
		})
		.unwrap();
		let frame = vec![0.0; enc.frame_size()];
		enc.encode(&frame).unwrap();

		// Three 120-frame packets are required to push out 312 frames.
		let packets = enc.finish(&[]).unwrap();
		assert_eq!(packets.packets().len(), 3);
		assert_eq!(packets.discard_padding(), 48);
	}

	#[test]
	fn reset_drops_pending_opus_lookahead() {
		let mut enc = Encoder::new(&Settings::new(48_000, Layout::Mono)).unwrap();
		let mut old = vec![0.0; enc.frame_size()];
		old[enc.frame_size() - 1] = 1.0;
		enc.encode(&old).unwrap();

		enc.reset();
		let next = vec![0.0; enc.frame_size()];
		let actual = enc.encode(&next).unwrap();
		let mut decoder = Decoder::new(&enc.catalog(), &DecodeConfig::default()).unwrap();
		let decoded = decoder.decode(&actual.payload).unwrap();
		let peak = decoded
			.samples
			.iter()
			.fold(0.0f32, |peak, sample| peak.max(sample.abs()));
		assert!(peak < 0.001, "pre-reset impulse leaked into the next epoch: {peak}");

		enc.reset();
		let finish = enc.finish(&[]).unwrap();
		assert!(finish.packets().is_empty());
		assert_eq!(finish.discard_padding(), 0);
	}

	#[test]
	fn opus_runtime_bitrate_updates_encoder_state() {
		let mut enc = Encoder::new(&Settings {
			bitrate: Some(moq_net::bandwidth::Rate::from_bps(64_000)),
			..Settings::default()
		})
		.unwrap();

		enc.set_bitrate(moq_net::bandwidth::Rate::from_bps(32_000)).unwrap();
		assert_eq!(enc.bitrate(), moq_net::bandwidth::Rate::from_bps(32_000));
		assert_eq!(enc.settings().bitrate, Some(moq_net::bandwidth::Rate::from_bps(32_000)));
		assert_eq!(
			Encoder::get_opus_ctl(
				opus_inner(&enc),
				unsafe_libopus::OPUS_GET_BITRATE_REQUEST,
				"OPUS_GET_BITRATE"
			)
			.unwrap(),
			32_000
		);
	}

	#[test]
	fn opus_runtime_bitrate_rejects_values_libopus_would_clamp() {
		let mut enc = Encoder::new(&Settings::default()).unwrap();
		let original = enc.bitrate();
		assert!(enc.set_bitrate(moq_net::bandwidth::Rate::from_bps(1)).is_err());
		assert!(enc.set_bitrate(moq_net::bandwidth::Rate::from_bps(600_001)).is_err());
		assert_eq!(enc.bitrate(), original);
	}

	#[test]
	fn opus_applies_dtx_control() {
		let enc = Encoder::new(&Settings {
			dtx: true,
			..Settings::default()
		})
		.unwrap();

		assert_eq!(
			Encoder::get_opus_ctl(opus_inner(&enc), unsafe_libopus::OPUS_GET_DTX_REQUEST, "OPUS_GET_DTX").unwrap(),
			1
		);
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
	fn settings_fix_the_codec_rate() {
		let enc = Encoder::new(&Settings::new(24_000, Layout::Mono)).unwrap();
		assert_eq!(enc.codec_rate(), 24_000);
		assert_eq!(enc.catalog().sample_rate, 24_000);
		assert_eq!(enc.pre_skip, 312);
	}

	#[test]
	fn pcm_roundtrip_is_lossless() {
		let mut enc = Encoder::new(&Settings {
			codec: Codec::Pcm,
			..Settings::default()
		})
		.unwrap();
		let mut dec = Decoder::new(&enc.catalog(), &DecodeConfig::default()).unwrap();
		let input = sine(440.0, enc.codec_rate(), enc.codec_channels(), enc.frame_size());

		let packet = enc.encode(&input).unwrap();
		let output = dec.decode(&packet.payload).unwrap();

		assert_eq!(output.samples, input);
	}

	#[test]
	fn pcm_catalog_declares_fixed_bitrate() {
		let enc = Encoder::new(&Settings {
			codec: Codec::Pcm,
			..Settings::default()
		})
		.unwrap();
		let catalog = enc.catalog();

		assert_eq!(catalog.codec, hang::catalog::AudioCodec::Pcm);
		assert_eq!(catalog.bitrate, Some(48_000 * 2 * 32));
		assert_eq!(catalog.description, None);
	}

	#[test]
	fn pcm_rejects_runtime_bitrate_change() {
		let mut enc = Encoder::new(&Settings {
			codec: Codec::Pcm,
			..Settings::default()
		})
		.unwrap();
		let bitrate = enc.bitrate();

		assert!(matches!(enc.set_bitrate(bitrate), Err(Error::Unsupported(_))));
		assert_eq!(enc.bitrate(), bitrate);
	}

	#[test]
	fn pcm_rejects_fractional_sample_frame_duration() {
		let err = Encoder::new(&Settings {
			codec: Codec::Pcm,
			frame_duration: Duration::from_micros(2_500),
			..Settings::new(44_100, Layout::Stereo)
		});
		assert!(matches!(err, Err(Error::Unsupported(_))));
	}

	#[test]
	fn pcm_rejects_bitrate_overflow() {
		let err = Encoder::new(&Settings {
			codec: Codec::Pcm,
			frame_duration: Duration::from_secs(1),
			..Settings::new(u32::MAX, Layout::Discrete(u32::MAX))
		});
		assert!(matches!(err, Err(Error::Unsupported(_))));
	}

	#[test]
	fn pcm_rejects_opus_only_settings() {
		let settings = Settings {
			codec: Codec::Pcm,
			dtx: true,
			..Settings::default()
		};
		assert!(matches!(Encoder::new(&settings), Err(Error::Unsupported(_))));
	}

	#[test]
	fn pcm_preserves_discrete_multichannel_layout() {
		let settings = Settings {
			codec: Codec::Pcm,
			..Settings::new(48_000, Layout::Discrete(3))
		};
		let mut encoder = Encoder::new(&settings).unwrap();
		let catalog = encoder.catalog();
		let mut decoder = Decoder::new(&catalog, &DecodeConfig::default()).unwrap();
		let input = [0.1, 0.2, 0.3].repeat(encoder.frame_size());
		let output = decoder.decode(&encoder.encode(&input).unwrap().payload).unwrap();

		assert_eq!(decoder.layout(), Layout::Discrete(3));
		assert_eq!(output.samples, input);
	}

	#[test]
	fn opus_refuses_discrete_layout() {
		let settings = Settings::new(48_000, Layout::Discrete(2));
		assert!(matches!(Encoder::new(&settings), Err(Error::Unsupported(_))));
	}
}
