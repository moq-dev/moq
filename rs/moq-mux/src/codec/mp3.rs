//! MP3 (MPEG-1/2/2.5 Audio Layer III).
//!
//! Audio carried verbatim: each frame is published whole. The header is parsed
//! only for the catalog config (sample rate, channels); the audio is never
//! decoded and there is no out-of-band configuration record. [`Import`] publishes
//! raw MP3 frames to a moq broadcast.

use crate::catalog::hang::CatalogExt;
use crate::container::Frame;
use moq_net::Timestamp;

/// MP3 parsing errors.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The buffer was shorter than the 4-byte MPEG audio frame header.
	#[error("MP3 frame header must be at least 4 bytes")]
	HeaderTooShort,

	/// The 11-bit frame sync (`0xFFE`) was missing.
	#[error("missing MP3 frame sync")]
	MissingSync,

	/// The MPEG version field was the reserved value `01`.
	#[error("reserved MPEG version")]
	ReservedVersion,

	/// The layer field was not Layer III, so this is not an MP3 frame (Layer I/II
	/// are MP1/MP2, the reserved value is invalid).
	#[error("not an MPEG Layer III (MP3) frame")]
	NotLayer3,

	/// The sample-rate index was the reserved value `11`.
	#[error("reserved MP3 sample rate")]
	ReservedSampleRate,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Typed MP3 configuration parsed from an MPEG audio frame header.
pub struct Config {
	/// Sampling frequency in Hz.
	pub sample_rate: u32,
	/// Channel count (1 for the mono channel mode, 2 otherwise).
	pub channel_count: u32,
}

impl Config {
	/// Parse the catalog config from the start of an MPEG Layer III frame.
	///
	/// Reads the 4-byte frame header (ISO/IEC 11172-3 §2.4.1.2): verifies the
	/// frame sync and that the layer is III, then derives the sample rate from
	/// the version + sample-rate index and the channel count from the channel
	/// mode. The buffer is not advanced; the frame is published whole.
	pub fn parse(data: &[u8]) -> Result<Self> {
		if data.len() < 4 {
			return Err(Error::HeaderTooShort);
		}

		// 11-bit frame sync: all of byte 0 plus the top 3 bits of byte 1.
		if data[0] != 0xFF || (data[1] & 0xE0) != 0xE0 {
			return Err(Error::MissingSync);
		}

		let version = (data[1] >> 3) & 0x03;
		let layer = (data[1] >> 1) & 0x03;
		// Layer is encoded inverted: 0b01 == Layer III.
		if layer != 0b01 {
			return Err(Error::NotLayer3);
		}

		let sr_index = ((data[2] >> 2) & 0x03) as usize;
		if sr_index == 0b11 {
			return Err(Error::ReservedSampleRate);
		}

		let sample_rate = match version {
			0b11 => [44100, 48000, 32000][sr_index], // MPEG-1
			0b10 => [22050, 24000, 16000][sr_index], // MPEG-2
			0b00 => [11025, 12000, 8000][sr_index],  // MPEG-2.5
			_ => return Err(Error::ReservedVersion),
		};

		// Channel mode 0b11 is single channel (mono); the rest are two-channel.
		let channel_count = if (data[3] >> 6) & 0x03 == 0b11 { 1 } else { 2 };

		Ok(Self {
			sample_rate,
			channel_count,
		})
	}
}

/// MP3 importer.
///
/// Publishes raw MP3 frames to a single moq track. Build it with [`new`](Self::new),
/// passing the track producer and the [`catalog::Reserved`](crate::catalog::Reserved)
/// it reserves its rendition from.
///
/// Each frame handed to [`decode`](Self::decode) is published in its own group so the
/// relay can forward it immediately. MP3 carries its config in band, so the rendition
/// has no out-of-band description.
pub struct Import<E: CatalogExt = ()> {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	rendition: crate::catalog::AudioTrack<E>,
}

impl<E: CatalogExt> Import<E> {
	/// Publish on an existing track producer, reserving the rendition from `reserved`.
	pub fn new(
		track: moq_net::track::Producer,
		reserved: crate::catalog::Reserved<E>,
		config: Config,
	) -> crate::Result<Self> {
		let mut audio =
			hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Mp3, config.sample_rate, config.channel_count);
		audio.container = hang::catalog::Container::Legacy;

		tracing::debug!(name = ?track.name(), config = ?audio, "starting track");

		let mut rendition = reserved.audio(track.name());
		rendition.set(audio);

		Ok(Self {
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			rendition,
		})
	}

	/// A watch-only handle to this track's subscriber demand.
	pub fn demand(&self) -> moq_net::track::Demand {
		self.track.track().demand()
	}

	/// Finish the track, flushing the current group.
	pub fn finish(&mut self) -> crate::Result<()> {
		self.rendition.record_group_end(None);
		self.track.finish()?;
		Ok(())
	}

	/// Abort the track with `err` instead of finishing it cleanly, so subscribers
	/// see the real cause rather than [`moq_net::Error::Dropped`].
	pub fn abort(&mut self, err: moq_net::Error) {
		self.track.abort(err);
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> crate::Result<()> {
		self.rendition.record_group_end(None);
		self.track.seek(sequence)?;
		Ok(())
	}

	/// Publish one MP3 frame as its own group, stamping `pts` or a wall clock when absent.
	pub fn decode<B: moq_net::IntoBytes>(&mut self, frame: B, pts: Option<Timestamp>) -> crate::Result<()> {
		let timestamp = self.rendition.timestamp(pts)?;
		self.rendition.record_group_end(Some(timestamp));
		let bytes = frame.as_ref().len();
		self.track.write(Frame {
			timestamp,
			payload: frame.into_bytes(),
			keyframe: true,
			duration: None,
		})?;
		self.track.finish_group()?;
		self.rendition.record_frame(timestamp, bytes);
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_mpeg1_stereo() {
		// MPEG-1 Layer III, 128 kbps, 44.1 kHz, joint stereo.
		let header = [0xFF, 0xFB, 0x90, 0x44];
		let cfg = Config::parse(&header).unwrap();
		assert_eq!(cfg.sample_rate, 44100);
		assert_eq!(cfg.channel_count, 2);
	}

	#[test]
	fn parses_mpeg1_mono() {
		// Same header but channel mode 0b11 (mono) in the top bits of byte 3.
		let header = [0xFF, 0xFB, 0x90, 0xC4];
		let cfg = Config::parse(&header).unwrap();
		assert_eq!(cfg.channel_count, 1);
	}

	#[test]
	fn parses_mpeg2_sample_rate() {
		// MPEG-2 (version 0b10), Layer III, sample-rate index 0 -> 22.05 kHz.
		let header = [0xFF, 0xF3, 0x90, 0x44];
		let cfg = Config::parse(&header).unwrap();
		assert_eq!(cfg.sample_rate, 22050);
	}

	#[test]
	fn rejects_layer2() {
		// Layer II is 0b10, i.e. an MP2 (not MP3) frame.
		let header = [0xFF, 0xFD, 0x90, 0x44];
		assert!(matches!(Config::parse(&header), Err(Error::NotLayer3)));
	}

	#[test]
	fn rejects_missing_sync() {
		assert!(matches!(
			Config::parse(&[0x00, 0x00, 0x00, 0x00]),
			Err(Error::MissingSync)
		));
	}

	#[test]
	fn rejects_short() {
		assert!(matches!(Config::parse(&[0xFF, 0xFB]), Err(Error::HeaderTooShort)));
	}
}
