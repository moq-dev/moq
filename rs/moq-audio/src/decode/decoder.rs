//! Audio decoder front end.
//!
//! Mirror of [`encode::Encoder`](crate::encode::Encoder): opens a
//! [`Backend`](super::backend::Backend) for the catalog codec and trims its
//! startup delay, producing interleaved `f32` PCM.

use super::Decoded;
use super::backend::{self, Backend};
use crate::{Error, Layout};

/// Decoder backend selection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
	/// Prefer a platform decoder, falling back to software.
	#[default]
	Auto,
	/// Require a software backend.
	Software,
	/// Require the track to be this codec, by its lowercase name: `"opus"`,
	/// `"pcm"`, or `"aac"`. Any other codec is refused. The library still picks
	/// the backend, platform first.
	Named(String),
}

/// Low-level decoder configuration.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Backend selection policy.
	pub kind: Kind,
}

impl Config {
	/// Select the available backend automatically.
	pub fn new() -> Self {
		Self::default()
	}
}

/// Decodes codec packets into interleaved `f32` PCM.
///
/// The bring-your-own-payload layer under [`Consumer`](super::Consumer): use it
/// when the packets don't come from a plain track subscription.
pub struct Decoder {
	backend: Box<dyn Backend>,
	/// Startup delay in native-rate frames, and how much of it is left to trim.
	delay: usize,
	delay_remaining: usize,
}

impl Decoder {
	/// Build a decoder from a catalog [`AudioConfig`](hang::catalog::AudioConfig).
	///
	/// Opus decodes at 48 kHz with the pre-skip and gain its OpusHead
	/// `description` declares, refusing a malformed head; without a description
	/// it takes the catalog's channel count and applies neither. PCM uses the
	/// catalog fields directly and requires an absent `description`. AAC reads
	/// its AudioSpecificConfig, synthesizing one from the catalog when absent.
	pub fn new(catalog: &hang::catalog::AudioConfig, config: &Config) -> Result<Self, Error> {
		let backend = backend::open(catalog, config)?;
		let delay = backend.delay();
		Ok(Self {
			backend,
			delay,
			delay_remaining: delay,
		})
	}

	/// The decoder backend name in use, e.g. `"libopus"` or `"symphonia"`.
	pub fn name(&self) -> &str {
		self.backend.name()
	}

	/// The rate the codec decodes at, which may differ from the catalog's.
	pub fn sample_rate(&self) -> u32 {
		self.backend.sample_rate()
	}

	/// The PCM layout the codec decodes to.
	pub fn layout(&self) -> Layout {
		self.backend.layout()
	}

	/// Reset codec history and reapply startup delay for a new discontinuous epoch.
	pub fn reset(&mut self) -> Result<(), Error> {
		self.reset_prediction()?;
		self.reapply_delay();
		Ok(())
	}

	/// Reapply catalog startup delay for a new playhead epoch without resetting codec prediction.
	pub(super) fn reapply_delay(&mut self) {
		self.delay_remaining = self.delay;
	}

	/// Reset codec prediction after packet loss without reapplying stream startup delay.
	pub(super) fn reset_prediction(&mut self) -> Result<(), Error> {
		self.backend.reset()
	}

	/// How much startup delay is still to be trimmed, in native-rate frames.
	///
	/// Trimmed samples are media the packet covered even though nothing came out of
	/// it, so a caller tracking where a packet ends has to add back whatever this
	/// dropped across the call.
	pub(super) fn delay_remaining(&self) -> usize {
		self.delay_remaining
	}

	/// Decode one packet into interleaved `f32` PCM and report its codec activity.
	///
	/// An empty Opus packet marks one lost packet and conceals as much audio as the
	/// last packet that decoded held, so a lost 20 ms packet yields 20 ms. It is
	/// refused with [`Error::Decode`] before any packet has decoded, since there is
	/// no length to conceal. Loss during DTX remains classified as DTX, while loss
	/// during active audio remains active.
	pub fn decode(&mut self, packet: &[u8]) -> Result<Decoded, Error> {
		let mut decoded = self.backend.decode(packet)?;
		let channels = self.backend.layout().channels() as usize;
		let trim = self.delay_remaining.min(decoded.samples.len() / channels);
		if trim > 0 {
			decoded.samples.drain(..trim * channels);
			self.delay_remaining -= trim;
		}
		Ok(decoded)
	}
}

#[cfg(test)]
pub(crate) mod tests {
	use super::*;

	/// Three consecutive AAC-LC frames of a 440 Hz full-scale sine, mono at
	/// 44.1 kHz, and the AudioSpecificConfig that opens them. Generated with:
	///
	/// ```text
	/// ffmpeg -f lavfi -i "sine=frequency=440:sample_rate=44100:duration=0.2" -af volume=8 -ac 1 -c:a aac -b:a 32k -f adts sine.aac
	/// ```
	///
	/// then stripping the ADTS header off each frame, since the wire carries raw
	/// AAC. These are frames 2 to 4, past the encoder's priming. lavfi's sine is
	/// an eighth of full scale, which is what the volume filter is undoing.
	#[cfg(feature = "aac")]
	const AAC_DESCRIPTION: &[u8] = b"\x12\x08";

	#[cfg(feature = "aac")]
	const AAC_FRAMES: [&[u8]; 3] = [
		b"\x01\x52\xf2\x8b\x1a\xd7\x8e\x7b\xfd\xa7\xef\xe7\xe3\x55\xd3\x4d\x2f\x55\x2e\x47\x1c\x92\x49\x11\x20\x77\x3f\xbe\x74\xdd\x99\xb3\x7b\xfb\x90\xc9\xf0\x61\x9f\xdc\x0c\x9f\x06\x19\xfd\xe1\x1f\x1f\x00\x67\xf7\x03\x87\xc0\x19\xfd\xc0\xc9\xf0\x07",
		b"\x01\x1e\x32\x89\xe2\x9d\x6b\x33\xe7\xff\xe2\xfe\xbf\xfa\xff\xe7\x2f\x8b\xd5\xd5\xe7\x5f\x3f\x59\xeb\xf1\xcb\xba\xa5\x5e\x52\x4a\xbd\x8d\x74\x50\x8c\x08\xa8\xa0\xd4\x51\x40\xa1\x86\x5d\x06\xb4\x6c\x32\xe6\x25\x9a\x66\x75\xcd\xf9\xbf\x6f\x83\xb7\x53\x80",
		b"\x01\x1e\x32\x8a\x22\x7d\x40\x87\x48\xdb\xdf\xff\xf9\x4f\xff\x87\xde\xef\x8b\xeb\x1e\x77\x5d\xfc\x67\x8f\x8c\x77\x8a\xd6\x29\x96\x1f\x29\xe7\x39\xd4\x53\xcf\x3c\xf3\xce\x79\xd4\x27\x9c\xf5\x65\x2a\x9b\xe9\x80\xb7\xba\xa9\xf9\x58\xc7\x3c\x58\x27\x8a\x60\xa1\x57",
	];

	#[cfg(feature = "aac")]
	fn aac_catalog() -> hang::catalog::AudioConfig {
		let mut catalog = hang::catalog::AudioConfig::new(hang::catalog::AAC { profile: 2 }, 44_100, 1);
		catalog.description = Some(bytes::Bytes::from_static(AAC_DESCRIPTION));
		catalog
	}

	#[cfg(feature = "aac")]
	#[test]
	fn aac_decodes_a_sine() {
		let mut decoder = Decoder::new(&aac_catalog(), &Config::default()).unwrap();
		assert_eq!(decoder.name(), "symphonia");
		assert_eq!(decoder.sample_rate(), 44_100);
		assert_eq!(decoder.layout(), Layout::Mono);

		let decoded: Vec<Vec<f32>> = AAC_FRAMES
			.iter()
			.map(|frame| decoder.decode(frame).unwrap().samples)
			.collect();

		// AAC-LC frames are 1024 samples each, whatever the packet size.
		for pcm in &decoded {
			assert_eq!(pcm.len(), 1024);
		}

		// The first frame is missing the previous frame's overlap, so measure the
		// last one. ffmpeg decodes this same frame to 0.744 RMS, near the 0.707 of
		// an ideal full-scale sine.
		let last = decoded.last().unwrap();
		let rms = (last.iter().map(|s| s * s).sum::<f32>() / last.len() as f32).sqrt();
		assert!((0.65..0.8).contains(&rms), "expected a full-scale sine, got {rms} RMS");
	}

	#[cfg(feature = "aac")]
	#[test]
	fn aac_reports_a_truncated_packet_as_decode() {
		let mut decoder = Decoder::new(&aac_catalog(), &Config::default()).unwrap();

		let truncated = &AAC_FRAMES[0][..16];
		assert!(matches!(decoder.decode(truncated), Err(Error::Decode(_))));
	}

	#[cfg(feature = "aac")]
	#[test]
	fn aac_synthesizes_a_missing_description() {
		// An MSF catalog carries the shape in its own fields instead.
		let mut catalog = aac_catalog();
		catalog.description = None;

		let mut decoder = Decoder::new(&catalog, &Config::default()).unwrap();
		assert_eq!(decoder.sample_rate(), 44_100);
		assert_eq!(decoder.decode(AAC_FRAMES[0]).unwrap().samples.len(), 1024);
	}

	/// A packet libopus rejects is that packet's problem, not the
	/// configuration's. The distinction is what lets a consumer drop the frame and
	/// keep the subscription instead of ending the stream over one bad packet.
	#[test]
	fn opus_reports_a_rejected_packet_as_decode() {
		let head = moq_mux::codec::opus::Config::new(48_000, 2).encode().unwrap();
		let mut catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		catalog.description = Some(head);

		let mut decoder = Decoder::new(&catalog, &Config::default()).unwrap();

		// Not a valid TOC byte sequence: libopus reports OPUS_INVALID_PACKET.
		assert!(matches!(decoder.decode(&[0xFF; 3]), Err(Error::Decode(_))));
	}

	/// Real Opus: 20 ms packets of a continuous 440 Hz sine, mono, from libopus
	/// with its 312-sample lookahead.
	pub(crate) fn opus_packets(count: usize) -> Vec<bytes::Bytes> {
		let mut encoder = crate::encode::Encoder::new(&crate::encode::Settings::new(48_000, Layout::Mono)).unwrap();
		let frames = encoder.frame_size();
		(0..count)
			.map(|packet| {
				let pcm: Vec<f32> = (packet * frames..(packet + 1) * frames)
					.map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin() * 0.5)
					.collect();
				encoder.encode(&pcm).unwrap().payload
			})
			.collect()
	}

	/// A catalog shaped like an import's: the OpusHead input rate as the catalog rate.
	pub(crate) fn opus_catalog(head: moq_mux::codec::opus::Config) -> hang::catalog::AudioConfig {
		let mut catalog =
			hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, head.sample_rate, head.channel_count);
		catalog.description = Some(head.encode().unwrap());
		catalog
	}

	fn rms(samples: &[f32]) -> f32 {
		(samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
	}

	/// The OpusHead input rate is metadata: 44.1 kHz and unknown (0) are valid
	/// heads, and no input rate changes the 48 kHz clock the packets decode on.
	#[test]
	fn opus_decodes_at_48k_whatever_the_input_rate() {
		let packets = opus_packets(2);
		for input_rate in [0, 8_000, 24_000, 44_100, 48_000, 96_000] {
			let head = moq_mux::codec::opus::Config::new(input_rate, 1).with_pre_skip(312);
			let mut decoder = Decoder::new(&opus_catalog(head), &Config::default()).unwrap();
			assert_eq!(decoder.sample_rate(), 48_000, "input rate {input_rate}");

			// The pre-skip is 48 kHz samples, trimmed once.
			assert_eq!(decoder.decode(&packets[0]).unwrap().samples.len(), 960 - 312);
			assert_eq!(decoder.decode(&packets[1]).unwrap().samples.len(), 960);
		}
	}

	/// One lost packet conceals one packet's worth of audio, not the 120 ms
	/// libopus would fill given the whole buffer.
	#[test]
	fn opus_conceals_the_length_of_the_last_packet() {
		let packets = opus_packets(3);
		let mut decoder = Decoder::new(
			&opus_catalog(moq_mux::codec::opus::Config::new(48_000, 1)),
			&Config::default(),
		)
		.unwrap();

		// Nothing decoded yet, so there is no length to conceal.
		assert!(matches!(decoder.decode(&[]), Err(Error::Decode(_))));

		for packet in &packets {
			decoder.decode(packet).unwrap();
		}
		assert_eq!(decoder.decode(&[]).unwrap().samples.len(), 960);

		// Concealment doesn't change the length: the next loss is the same size.
		assert_eq!(decoder.decode(&[]).unwrap().samples.len(), 960);
	}

	#[test]
	fn opus_applies_the_declared_gain() {
		let packets = opus_packets(5);
		let decode = |output_gain: i16| {
			let mut head = moq_mux::codec::opus::Config::new(48_000, 1);
			head.output_gain = output_gain;
			let mut decoder = Decoder::new(&opus_catalog(head), &Config::default()).unwrap();
			let mut last = Vec::new();
			for packet in &packets {
				last = decoder.decode(packet).unwrap().samples;
			}
			// A reset after loss keeps the gain.
			decoder.reset().unwrap();
			let reset = decoder.decode(&packets[4]).unwrap().samples;
			(rms(&last), rms(&reset))
		};

		// -6.02 dB in Q7.8 halves the amplitude.
		let (plain, plain_reset) = decode(0);
		let (quiet, quiet_reset) = decode(-1541);
		assert!((quiet / plain - 0.5).abs() < 0.001, "gain ratio {}", quiet / plain);
		assert!(
			(quiet_reset / plain_reset - 0.5).abs() < 0.001,
			"gain ratio after reset {}",
			quiet_reset / plain_reset
		);
	}

	/// A present description is the stream's configuration, so a broken one is
	/// refused rather than replaced by the catalog's fields.
	#[test]
	fn opus_refuses_a_malformed_description() {
		let valid = moq_mux::codec::opus::Config::new(48_000, 2).encode().unwrap().to_vec();
		let mut version = valid.clone();
		version[8] = 16;
		let mut channels = valid.clone();
		channels[9] = 3;
		let mut signature = valid.clone();
		signature[0] = b'X';
		// Family 1 promising a table that is not there.
		let mut table = valid.clone();
		table[18] = 1;

		for (name, description) in [
			("truncated", valid[..18].to_vec()),
			("empty", Vec::new()),
			("signature", signature),
			("version", version),
			("channels", channels),
			("table", table),
		] {
			let mut catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
			catalog.description = Some(description.into());
			assert!(
				matches!(Decoder::new(&catalog, &Config::default()), Err(Error::Unsupported(_))),
				"{name}"
			);
		}
	}

	/// Without a description the catalog shapes the stream, which has no pre-skip.
	#[test]
	fn opus_decodes_without_a_description() {
		let packets = opus_packets(1);
		let catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 24_000, 1);
		let mut decoder = Decoder::new(&catalog, &Config::default()).unwrap();
		assert_eq!(decoder.sample_rate(), 48_000);
		assert_eq!(decoder.decode(&packets[0]).unwrap().samples.len(), 960);

		let catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 6);
		assert!(matches!(
			Decoder::new(&catalog, &Config::default()),
			Err(Error::Unsupported(_))
		));
	}

	#[test]
	fn pcm_rejects_incomplete_channel_frame() {
		let catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Pcm, 48_000, 2);
		let mut decoder = Decoder::new(&catalog, &Config::default()).unwrap();

		assert!(matches!(
			decoder.decode(&[]),
			Err(Error::Misaligned { got: 0, expected: 8 })
		));
		assert!(matches!(
			decoder.decode(&[0; 4]),
			Err(Error::Misaligned { got: 4, expected: 8 })
		));
	}

	#[test]
	fn decoder_rejects_unknown_codec() {
		let catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Unknown("future".into()), 48_000, 2);

		assert!(matches!(
			Decoder::new(&catalog, &Config::default()),
			Err(Error::Unsupported(_))
		));
	}

	#[test]
	fn pcm_rejects_incorrect_catalog_bitrate() {
		let mut catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Pcm, 48_000, 2);
		catalog.bitrate = Some(1);

		assert!(matches!(
			Decoder::new(&catalog, &Config::default()),
			Err(Error::Unsupported(_))
		));
	}

	/// Published moq-audio accepts the codec's name, not a backend's.
	#[cfg(feature = "aac")]
	#[test]
	fn named_aac_decodes_aac() {
		let config = Config {
			kind: Kind::Named("aac".into()),
		};
		assert!(Decoder::new(&aac_catalog(), &config).is_ok());

		let config = Config {
			kind: Kind::Named("opus".into()),
		};
		assert!(matches!(
			Decoder::new(&aac_catalog(), &config),
			Err(Error::Unsupported(_))
		));
	}

	#[test]
	fn refuses_unavailable_backend() {
		let catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Pcm, 48_000, 2);
		let config = Config {
			kind: Kind::Named("missing".into()),
		};
		assert!(matches!(Decoder::new(&catalog, &config), Err(Error::Unsupported(_))));
	}
}
