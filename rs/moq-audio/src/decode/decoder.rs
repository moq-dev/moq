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
	/// Require a backend by its stable lowercase name: `"libopus"`, `"pcm"`, or
	/// `"symphonia"`.
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
	/// Opus parses the OpusHead `description` if present, falling back to the
	/// catalog's declared sample rate and channel count. PCM uses those catalog
	/// fields directly and requires an absent `description`. AAC reads its
	/// AudioSpecificConfig, synthesizing one from the catalog when absent.
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
	/// Empty Opus packets invoke packet-loss concealment. Loss during DTX remains
	/// classified as DTX, while loss during active audio remains active.
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
mod tests {
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

	#[test]
	fn refuses_unavailable_backend() {
		let catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Pcm, 48_000, 2);
		let config = Config {
			kind: Kind::Named("missing".into()),
		};
		assert!(matches!(Decoder::new(&catalog, &config), Err(Error::Unsupported(_))));
	}
}
