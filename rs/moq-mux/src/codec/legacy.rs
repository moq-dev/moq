//! Legacy broadcast audio (MP2, AC-3, E-AC-3) carried verbatim.
//!
//! These codecs share one model: every frame is whole and self-describing
//! (framing header included), published as one hang frame in its own group,
//! never decoded. Verbatim is byte-exact for complete, well-formed frames;
//! malformed or out-of-scope input is rejected, never mis-described. Each
//! codec contributes only a header parser and a [`Descriptor`]; this module
//! owns the track lifecycle.

use moq_net::Timestamp;

use crate::catalog::hang::CatalogExt;
use crate::container::Frame;

/// Legacy audio (MP2 / AC-3 / E-AC-3) header parsing errors.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error("AC-3 header needs 7 bytes")]
	Ac3HeaderTooShort,

	#[error("missing AC-3 sync word")]
	Ac3MissingSyncWord,

	#[error("invalid AC-3 frame size code")]
	Ac3InvalidFrameSizeCode,

	#[error("unsupported AC-3 bsid {0}")]
	Ac3UnsupportedBsid(u8),

	#[error("reserved AC-3 sample-rate code")]
	Ac3ReservedSampleRate,

	#[error("E-AC-3 header needs 6 bytes")]
	Eac3HeaderTooShort,

	#[error("missing E-AC-3 sync word")]
	Eac3MissingSyncWord,

	#[error("not an E-AC-3 bitstream (bsid {0})")]
	Eac3NotEac3Bsid(u8),

	#[error("reserved E-AC-3 stream type")]
	Eac3ReservedStreamType,

	#[error("E-AC-3 dependent substream (7.1+ layout) is not supported; only a single independent substream")]
	Eac3DependentSubstream,

	#[error("E-AC-3 additional substream {0} is not supported; only a single independent substream")]
	Eac3AdditionalSubstream(u8),

	#[error("E-AC-3 frame length {0} shorter than its header")]
	Eac3FrameShorterThanHeader(usize),

	#[error("reserved E-AC-3 sample-rate code")]
	Eac3ReservedSampleRate,

	#[error("MP2 header needs 4 bytes")]
	Mp2HeaderTooShort,

	#[error("missing MP2 frame sync")]
	Mp2MissingSync,

	#[error("reserved or MPEG-2.5 audio version")]
	Mp2ReservedVersion,

	#[error("not MPEG Layer II")]
	Mp2NotLayerII,

	#[error("reserved MP2 sample-rate index")]
	Mp2ReservedSampleRate,

	#[error("free-format or invalid MP2 bitrate")]
	Mp2InvalidBitrate,
}

/// A Result type alias for legacy audio header parsing.
pub type Result<T> = std::result::Result<T, Error>;

/// A parsed legacy-audio frame header.
#[derive(Debug)]
pub(crate) struct Header {
	/// Whole-frame size in bytes (header included).
	pub len: usize,
	pub sample_rate: u32,
	pub channel_count: u32,
	/// Samples in this frame. Per-frame, not per-codec: E-AC-3 varies it
	/// (256 x numblks) while MP2/AC-3 keep it constant.
	pub samples: u64,
}

/// What distinguishes one legacy codec from another.
pub(crate) struct Descriptor {
	/// Track name suffix, e.g. ".mp2".
	pub track_suffix: &'static str,
	/// Catalog codec for the rendition.
	pub codec: hang::catalog::AudioCodec,
	/// Bytes needed to attempt a header parse.
	pub min_header_len: usize,
	/// Parse one frame header at the start of the slice.
	pub parse: fn(&[u8]) -> Result<Header>,
}

/// Catalog config for a legacy audio track. Both fields come from the frame
/// header, never the TS stream_type.
pub(crate) struct Config {
	pub sample_rate: u32,
	pub channel_count: u32,
}

/// Legacy audio importer.
///
/// Publishes each whole frame as one hang frame in its own group, so the relay
/// forwards it immediately. The audio is never decoded; the catalog carries the
/// codec, sample rate and channel count read from the frame header.
pub(crate) struct Import<E: CatalogExt = ()> {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	rendition: crate::catalog::AudioTrack<E>,
}

impl<E: CatalogExt> Import<E> {
	/// Publish on an existing track, reserving the rendition from `reserved`. Mint the
	/// track at the descriptor's suffix and the microsecond
	/// [`hang::container::TIMESCALE`] (e.g. via [`crate::import::unique_track`]).
	pub fn new(
		descriptor: &'static Descriptor,
		track: moq_net::track::Producer,
		reserved: crate::catalog::Reserved<E>,
		config: Config,
	) -> Self {
		let mut audio_config =
			hang::catalog::AudioConfig::new(descriptor.codec.clone(), config.sample_rate, config.channel_count);
		audio_config.container = hang::catalog::Container::Legacy;
		// description stays None: legacy frames are self-describing and no in-repo
		// consumer needs out-of-band config (TS export self-describes; WebCodecs
		// cannot decode these codecs). Fill it only if a real consumer ever needs it.

		tracing::debug!(name = ?track.name(), config = ?audio_config, "starting track");

		let mut rendition = reserved.audio(track.name());
		rendition.set(audio_config);

		Self {
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			rendition,
		}
	}

	/// The MoQ track name.
	pub fn name(&self) -> &str {
		self.track.name()
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

	/// Publish one whole frame as a hang frame in its own group.
	pub fn decode<B: moq_net::IntoBytes>(&mut self, frame: B, pts: Option<Timestamp>) -> crate::Result<()> {
		let timestamp = self.rendition.timestamp(pts)?;
		self.rendition.record_group_end(Some(timestamp));
		let bytes = frame.as_ref().len();
		self.track.write(Frame {
			timestamp,
			duration: None,
			payload: frame.into_bytes(),
			keyframe: true,
		})?;
		self.track.finish_group()?;
		self.rendition.record_frame(timestamp, bytes);
		Ok(())
	}
}
