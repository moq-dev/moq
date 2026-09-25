//! Audio encoder front end.
//!
//! [`Encoder`] checks [`Settings`] against the codec, opens a
//! [`Backend`](super::backend::Backend) for it, and owns what every backend of a
//! codec shares: framing, the terminal drain, and the catalog entry.

use std::str::FromStr;
use std::time::Duration;

use bytes::Bytes;

use super::Encoded;
use super::backend::{self, Backend};
use crate::opus;
use crate::pcm;
use crate::{Error, Format, Layout};

/// Samples per channel in one AAC-LC frame.
const AAC_FRAME_SIZE: usize = 1024;

/// The audioObjectType of AAC-LC (ISO 14496-3 Table 1.17), `mp4a.40.2`.
const AAC_LC: u8 = 2;

/// The widest sample rate an AudioSpecificConfig can name: the escape from the
/// frequency table is a 24-bit field.
const AAC_MAX_SAMPLE_RATE: u32 = 0xFF_FFFF;

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
	/// AAC-LC (`mp4a.40.2`), through the platform's encoder. A host without one
	/// refuses it at construction.
	Aac,
}

impl Codec {
	/// Canonical lowercase identifier, matching the WebCodecs / RFC catalog
	/// string. Used as the wire/FFI codec name everywhere.
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Opus => "opus",
			Self::Pcm => "pcm",
			Self::Aac => "aac",
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
			"aac" => Ok(Self::Aac),
			other => Err(Error::Unsupported(format!("unknown codec: {other}"))),
		}
	}
}

/// Encoder backend selection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
	/// Prefer a platform encoder, falling back to software.
	#[default]
	Auto,
	/// Require a software backend.
	Software,
	/// Require a backend by its stable lowercase name: `"libopus"` or `"pcm"`.
	Named(String),
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

/// Audio codec settings shared by [`Encoder`] and [`Producer`](super::Producer).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Settings {
	/// Output codec. Defaults to [`Codec::Opus`].
	pub codec: Codec,
	/// Sample rate accepted by the codec.
	pub sample_rate: u32,
	/// Layout accepted by the codec.
	///
	/// AAC takes the layouts its channelConfiguration names: mono, stereo, 3.0,
	/// 4.0, 5.0, 5.1, and 7.1.
	pub layout: Layout,
	/// Bitrate in bits per second. `None` lets the codec pick. PCM requires
	/// `None` because its bitrate is fixed by the sample rate and channel count.
	///
	/// Rates too low for Opus to code anything at the chosen
	/// [`frame_duration`](Self::frame_duration) are rejected. The floor is 1200
	/// bps at the default 20 ms, rises for shorter frames, and is 2400 bps for
	/// frames of 10 ms and longer.
	pub bitrate: Option<moq_net::bandwidth::Rate>,
	/// Enable Opus discontinuous transmission during silence.
	pub dtx: bool,
	/// Encoded frame duration. Opus accepts 2.5 / 5 / 10 / 20 / 40 / 60 ms.
	/// PCM accepts any duration containing a whole number of samples. AAC frames
	/// are 1024 samples, so it accepts the duration that rounds to that at
	/// [`sample_rate`](Self::sample_rate), which [`from_input`](Self::from_input)
	/// fills in.
	pub frame_duration: Duration,
	/// Which encoder implementation to use.
	pub kind: Kind,
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
			kind: Kind::Auto,
		}
	}

	/// Derive concrete codec settings from source PCM.
	pub fn from_input(codec: Codec, input: &Input) -> Self {
		let sample_rate = match codec {
			Codec::Opus => opus::pick_rate(input.sample_rate),
			Codec::Pcm | Codec::Aac => input.sample_rate,
		};
		let defaults = Self::new(sample_rate, input.layout);
		let frame_duration = match codec {
			Codec::Aac => aac_frame_duration(sample_rate),
			Codec::Opus | Codec::Pcm => defaults.frame_duration,
		};
		Self {
			codec,
			frame_duration,
			..defaults
		}
	}

	/// Check the settings against the codec, returning its frame size.
	///
	/// Codec rules live here rather than in a backend, so every backend of a
	/// codec refuses the same settings.
	fn frame_size(&self) -> Result<usize, Error> {
		self.layout.validate()?;
		let (rate, channels) = (self.sample_rate, self.layout.channels());

		match self.codec {
			Codec::Opus => {
				opus::validate_rate(rate)?;
				if !matches!(self.layout, Layout::Mono | Layout::Stereo) {
					return Err(Error::Unsupported("opus requires a named mono or stereo layout".into()));
				}
				opus::frame_size(rate, self.frame_duration)
			}
			Codec::Pcm => {
				if self.bitrate.is_some() {
					return Err(Error::Unsupported(
						"pcm bitrate is fixed; leave Settings::bitrate unset".into(),
					));
				}
				if self.dtx {
					return Err(Error::Unsupported(
						"pcm does not support discontinuous transmission".into(),
					));
				}
				if rate == 0 {
					return Err(Error::Unsupported("pcm sample rate must be greater than zero".into()));
				}
				let frame_size = pcm::frame_size(rate, self.frame_duration)?;
				pcm::frame_bytes(frame_size, channels)?;
				pcm::bitrate(rate, channels)?;
				Ok(frame_size)
			}
			Codec::Aac => {
				if self.dtx {
					return Err(Error::Unsupported(
						"aac does not support discontinuous transmission".into(),
					));
				}
				aac_config(self)?;
				let frames = (self.frame_duration.as_nanos() * u128::from(rate) + 500_000_000) / 1_000_000_000;
				if frames != AAC_FRAME_SIZE as u128 {
					return Err(Error::Unsupported(format!(
						"aac frames are {AAC_FRAME_SIZE} samples, {:?} at {rate} Hz (got {:?})",
						aac_frame_duration(rate),
						self.frame_duration
					)));
				}
				Ok(AAC_FRAME_SIZE)
			}
		}
	}
}

impl Default for Settings {
	fn default() -> Self {
		Self::new(48_000, Layout::Stereo)
	}
}

/// One AAC frame at `sample_rate`, to the nearest nanosecond.
fn aac_frame_duration(sample_rate: u32) -> Duration {
	if sample_rate == 0 {
		return Duration::ZERO;
	}
	let rate = u64::from(sample_rate);
	Duration::from_nanos((AAC_FRAME_SIZE as u64 * 1_000_000_000 + rate / 2) / rate)
}

/// The AudioSpecificConfig fields for AAC-LC at the settings' rate and layout.
///
/// Only layouts with a channelConfiguration are accepted, since synthesizing
/// one from a bare count would mislabel the rest: config 3 is 3.0 where the
/// count's default layout is 2.1, and 6.1 has no config the encoder writes.
/// 7.1 takes config 7, the one every decoder reads as eight channels.
fn aac_config(settings: &Settings) -> Result<moq_mux::codec::aac::Config, Error> {
	let layout = settings.layout;
	if !matches!(
		layout,
		Layout::Mono
			| Layout::Stereo
			| Layout::ThreePointZero
			| Layout::FourPointZero
			| Layout::FivePointZero
			| Layout::FivePointOne
			| Layout::SevenPointOne
	) {
		return Err(Error::Unsupported(format!(
			"aac has no channelConfiguration for {layout:?}; use mono, stereo, 3.0, 4.0, 5.0, 5.1, or 7.1"
		)));
	}

	let sample_rate = settings.sample_rate;
	if !(1..=AAC_MAX_SAMPLE_RATE).contains(&sample_rate) {
		return Err(Error::Unsupported(format!(
			"aac sample rate must be between 1 and {AAC_MAX_SAMPLE_RATE} Hz (got {sample_rate})"
		)));
	}

	Ok(moq_mux::codec::aac::Config {
		profile: AAC_LC,
		sample_rate,
		channel_count: layout.channels(),
	})
}

/// Audio encoder over codec-sized interleaved `f32` PCM.
///
/// Build one with [`Encoder::new`], feed full PCM frames via
/// [`encode`](Self::encode), then pass the trailing partial frame to
/// [`finish`](Self::finish). Publish every packet either call returns and apply
/// the terminal [`Finish::discard_padding`] when the container supports it.
pub struct Encoder {
	backend: Box<dyn Backend>,
	settings: Settings,
	frame_size: usize,
	/// The catalog description, synthesized from the settings at construction so
	/// the rendition can be registered before the first packet exists.
	description: Option<Bytes>,
	/// Whether input has reached the codec, since a fresh encoder owes no drain.
	started: bool,
}

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
	/// Open an encoder for `settings`, refusing a codec no backend on this host
	/// encodes.
	pub fn new(settings: &Settings) -> Result<Self, Error> {
		let frame_size = settings.frame_size()?;
		let backend = backend::open(settings)?;

		let description = match settings.codec {
			Codec::Opus => {
				// OpusHead carries the lookahead in the 48 kHz timebase.
				let lookahead = backend.delay() as u64;
				let pre_skip = u16::try_from((lookahead * 48_000) / u64::from(settings.sample_rate))
					.map_err(|_| Error::Unsupported(format!("Opus lookahead {lookahead} does not fit in OpusHead")))?;
				let head = moq_mux::codec::opus::Config::new(settings.sample_rate, settings.layout.channels())
					.with_pre_skip(pre_skip)
					.encode()
					.map_err(moq_mux::Error::from)?;
				Some(head)
			}
			Codec::Aac => Some(aac_config(settings)?.encode()),
			Codec::Pcm => None,
		};

		Ok(Self {
			backend,
			settings: settings.clone(),
			frame_size,
			description,
			started: false,
		})
	}

	/// The encoder backend name in use, e.g. `"libopus"`.
	pub fn name(&self) -> &str {
		self.backend.name()
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
		self.settings.sample_rate
	}

	/// Channel count the codec actually runs at, which is
	/// [`Settings::layout`]'s channel count.
	pub fn codec_channels(&self) -> u32 {
		self.settings.layout.channels()
	}

	/// Number of samples per channel the codec consumes per call to
	/// [`encode`](Self::encode).
	pub fn frame_size(&self) -> usize {
		self.frame_size
	}

	/// Current target bitrate.
	pub fn bitrate(&self) -> moq_net::bandwidth::Rate {
		moq_net::bandwidth::Rate::from_bps(self.backend.bitrate())
	}

	/// Retune the live encoder to `bitrate`.
	///
	/// # Errors
	///
	/// Returns [`Error::Unsupported`] when the codec's rate is fixed (PCM) or the
	/// backend can't change it mid-stream. The encoder keeps running at its
	/// opening rate, so a caller driving a control loop should stop adapting
	/// rather than stop encoding.
	pub fn set_bitrate(&mut self, bitrate: moq_net::bandwidth::Rate) -> Result<(), Error> {
		let previous = self.backend.bitrate();
		self.backend.set_bitrate(bitrate.as_bps())?;
		if bitrate.as_bps() != previous {
			self.settings.bitrate = Some(bitrate);
		}
		Ok(())
	}

	/// Drop all codec history so a later epoch cannot emit audio from this one.
	pub(super) fn reset(&mut self) {
		self.backend.reset();
		self.started = false;
	}

	/// Whether this epoch has submitted audio to the codec.
	pub(super) fn started(&self) -> bool {
		self.started
	}

	/// Codec priming the catalog can't signal, in codec-rate frames, which the
	/// producer folds into its timestamps instead.
	///
	/// Opus declares its lookahead as OpusHead pre-skip, which the decoder trims,
	/// so nothing is folded. An AudioSpecificConfig has no such field, so each
	/// AAC packet is stamped that much earlier and the priming lands before the
	/// first input sample rather than delaying it.
	pub(super) fn folded_delay(&self) -> usize {
		match self.settings.codec {
			Codec::Aac => self.backend.delay(),
			Codec::Opus | Codec::Pcm => 0,
		}
	}

	/// Encode one frame of interleaved `f32` PCM at [`codec_rate`](Self::codec_rate).
	///
	/// `pcm.len()` must equal `frame_size() * codec_channels()`. The
	/// [`Producer`](super::Producer) handles format conversion and resampling
	/// before calling this; for direct use, the caller does the same.
	pub fn encode(&mut self, pcm: &[f32]) -> Result<Encoded, Error> {
		let expected = self.frame_size * self.codec_channels() as usize;
		if pcm.len() != expected {
			return Err(Error::Misaligned {
				got: std::mem::size_of_val(pcm),
				expected: expected * std::mem::size_of::<f32>(),
			});
		}
		let encoded = self.backend.encode(pcm)?;
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
		let channels = self.codec_channels() as usize;
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

		let lookahead = self.backend.delay();
		let drain = lookahead.saturating_sub(padding);
		let silence = vec![0.0; frame_samples];
		for _ in 0..drain.div_ceil(self.frame_size) {
			packets.push(self.encode(&silence)?);
		}

		let discard_padding = packets
			.len()
			.saturating_mul(self.frame_size)
			.saturating_sub(lookahead)
			.saturating_sub(source_frames);

		Ok(Finish {
			packets,
			discard_padding,
		})
	}

	/// hang catalog entry describing this encoder's output stream.
	pub fn catalog(&self) -> hang::catalog::AudioConfig {
		let (rate, channels) = (self.codec_rate(), self.codec_channels());
		let (codec, bitrate): (hang::catalog::AudioCodec, _) = match self.settings.codec {
			Codec::Opus => (
				hang::catalog::AudioCodec::Opus,
				self.settings.bitrate.map(moq_net::bandwidth::Rate::as_bps),
			),
			Codec::Pcm => (
				hang::catalog::AudioCodec::Pcm,
				Some(pcm::bitrate(rate, channels).expect("pcm encoder bitrate validated at construction")),
			),
			Codec::Aac => (
				hang::catalog::AAC { profile: AAC_LC }.into(),
				self.settings.bitrate.map(moq_net::bandwidth::Rate::as_bps),
			),
		};

		let mut config = hang::catalog::AudioConfig::new(codec, rate, channels);
		config.bitrate = bitrate;
		config.description = self.description.clone();
		config.container = hang::catalog::Container::Legacy;
		config
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
			(enc.frame_size() - enc.backend.delay()) * enc.codec_channels() as usize
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
	fn codec_roundtrips_as_str() {
		assert_eq!(Codec::Opus.as_str(), "opus");
		assert_eq!(Codec::Opus.to_string(), "opus");
		assert_eq!("opus".parse::<Codec>().unwrap(), Codec::Opus);
		assert_eq!(Codec::Pcm.as_str(), "pcm");
		assert_eq!(Codec::Pcm.to_string(), "pcm");
		assert_eq!("pcm".parse::<Codec>().unwrap(), Codec::Pcm);
		assert_eq!(Codec::Aac.as_str(), "aac");
		assert_eq!(Codec::Aac.to_string(), "aac");
		assert_eq!("aac".parse::<Codec>().unwrap(), Codec::Aac);
		assert!("mp3".parse::<Codec>().is_err());
	}

	#[test]
	fn settings_fix_the_codec_rate() {
		let enc = Encoder::new(&Settings::new(24_000, Layout::Mono)).unwrap();
		assert_eq!(enc.codec_rate(), 24_000);
		let catalog = enc.catalog();
		assert_eq!(catalog.sample_rate, 24_000);
		let head = moq_mux::codec::opus::Config::parse(&mut catalog.description.unwrap().as_ref()).unwrap();
		assert_eq!(head.pre_skip, 312);
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

	/// The catalog carries only a count, so a discrete layout comes back as the
	/// count's default one, with the samples untouched.
	#[test]
	fn pcm_passes_discrete_multichannel_samples_through() {
		let settings = Settings {
			codec: Codec::Pcm,
			..Settings::new(48_000, Layout::Discrete(3))
		};
		let mut encoder = Encoder::new(&settings).unwrap();
		let catalog = encoder.catalog();
		let mut decoder = Decoder::new(&catalog, &DecodeConfig::default()).unwrap();
		let input = [0.1, 0.2, 0.3].repeat(encoder.frame_size());
		let output = decoder.decode(&encoder.encode(&input).unwrap().payload).unwrap();

		assert_eq!(decoder.layout(), Layout::TwoPointOne);
		assert_eq!(output.samples, input);
	}

	#[test]
	fn opus_refuses_discrete_layout() {
		let settings = Settings::new(48_000, Layout::Discrete(2));
		assert!(matches!(Encoder::new(&settings), Err(Error::Unsupported(_))));
	}

	/// AAC settings routed to the test stub, since this host has no AAC encoder.
	fn aac(layout: Layout) -> Settings {
		Settings {
			kind: Kind::Named(backend::stub::NAME.into()),
			..Settings::from_input(Codec::Aac, &Input::new(48_000, layout))
		}
	}

	/// The ASC is synthesized from the settings, so it exists before any packet.
	#[test]
	fn aac_catalog_carries_the_synthesized_asc() {
		let enc = Encoder::new(&aac(Layout::Stereo)).unwrap();
		assert_eq!(enc.name(), backend::stub::NAME);
		assert_eq!(enc.frame_size(), 1024);

		let catalog = enc.catalog();
		assert_eq!(catalog.codec, hang::catalog::AAC { profile: 2 }.into());
		assert_eq!(catalog.codec.to_string(), "mp4a.40.2");
		assert_eq!(catalog.sample_rate, 48_000);
		assert_eq!(catalog.channel_count, 2);
		assert_eq!(catalog.container, hang::catalog::Container::Legacy);
		// AAC-LC (2), 48 kHz (index 3), stereo (config 2).
		assert_eq!(catalog.description.as_deref(), Some(&[0x11, 0x90][..]));
	}

	#[test]
	fn aac_takes_the_layouts_with_a_channel_configuration() {
		for (layout, config) in [
			(Layout::Mono, 1),
			(Layout::Stereo, 2),
			(Layout::ThreePointZero, 3),
			(Layout::FourPointZero, 4),
			(Layout::FivePointZero, 5),
			(Layout::FivePointOne, 6),
			(Layout::SevenPointOne, 7),
		] {
			let catalog = Encoder::new(&aac(layout)).unwrap().catalog();
			let description = catalog.description.unwrap();
			assert_eq!(description[1] >> 3 & 0xF, config, "{layout:?}");
			assert_eq!(catalog.channel_count, layout.channels(), "{layout:?}");
		}

		for layout in [
			Layout::TwoPointOne,
			Layout::Quad,
			Layout::SixPointOne,
			Layout::Discrete(2),
		] {
			assert!(
				matches!(Encoder::new(&aac(layout)), Err(Error::Unsupported(_))),
				"{layout:?}"
			);
		}
	}

	/// AAC frames are 1024 samples however the duration is spelled.
	#[test]
	fn aac_frame_duration_is_the_codecs() {
		assert_eq!(aac(Layout::Stereo).frame_duration, Duration::from_nanos(21_333_333));
		let settings = Settings {
			frame_duration: Duration::from_micros(21_333),
			..aac(Layout::Stereo)
		};
		assert_eq!(Encoder::new(&settings).unwrap().frame_size(), 1024);

		let settings = Settings {
			frame_duration: Duration::from_millis(20),
			..aac(Layout::Stereo)
		};
		let err = Encoder::new(&settings).err().expect("20 ms is 960 samples");
		assert!(err.to_string().contains("1024"), "{err}");
	}

	#[test]
	fn aac_refuses_dtx() {
		let settings = Settings {
			dtx: true,
			..aac(Layout::Stereo)
		};
		assert!(matches!(Encoder::new(&settings), Err(Error::Unsupported(_))));
	}

	/// A backend that can't retune keeps its opening rate.
	#[test]
	fn fixed_rate_backend_keeps_its_opening_rate() {
		let mut enc = Encoder::new(&Settings {
			bitrate: Some(moq_net::bandwidth::Rate::from_bps(96_000)),
			..aac(Layout::Stereo)
		})
		.unwrap();

		let err = enc.set_bitrate(moq_net::bandwidth::Rate::from_bps(64_000));
		assert!(matches!(err, Err(Error::Unsupported(_))));
		assert_eq!(enc.bitrate(), moq_net::bandwidth::Rate::from_bps(96_000));
		assert_eq!(enc.settings().bitrate, Some(moq_net::bandwidth::Rate::from_bps(96_000)));
		assert_eq!(enc.catalog().bitrate, Some(96_000));
	}

	/// The drain pushes the encoder delay out through whole silent frames.
	#[test]
	fn aac_finish_drains_the_encoder_delay() {
		let mut enc = Encoder::new(&aac(Layout::Mono)).unwrap();
		assert_eq!(enc.folded_delay(), backend::stub::DELAY);
		enc.encode(&[0.0; 1024]).unwrap();

		// 2112 frames of delay take three 1024-frame packets.
		let finish = enc.finish(&[]).unwrap();
		assert_eq!(finish.packets().len(), 3);
		assert_eq!(finish.discard_padding(), 3 * 1024 - backend::stub::DELAY);
	}

	/// Opus signals its lookahead as pre-skip, so the producer folds none of it.
	#[test]
	fn opus_folds_no_delay() {
		assert_eq!(Encoder::new(&Settings::default()).unwrap().folded_delay(), 0);
	}
}
