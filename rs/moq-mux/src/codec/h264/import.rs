//! H.264 importer.
//!
//! Publishes split H.264 frames on a single moq track and tracks the catalog
//! rendition. Byte parsing lives in [`Split`]; this type drives it for the
//! convenience byte APIs ([`decode_frame`](Import::decode_frame) /
//! [`decode_stream`](Import::decode_stream)) and writes the resulting frames,
//! and also implements [`FrameDecode`] so a caller that runs its own [`Split`]
//! can publish frames directly.

use bytes::{Buf, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{Mode, Split};
use crate::Result;
use crate::container::Frame;
use crate::container::jitter::MinFrameDuration;
use crate::publish::{FrameDecode, Renditions};

/// H.264 importer. Handles both avc1 (length-prefixed) and avc3 (Annex-B)
/// input streams; the shape is detected from the first bytes the caller
/// supplies, or forced explicitly via [`with_mode`](Self::with_mode).
///
/// Build it from a [`moq_net::TrackRequest`] ([`new`](Self::new), the on-demand
/// path) or an existing [`moq_net::TrackProducer`] ([`from_track`](Self::from_track),
/// the broadcast-push / fixed-track path). The catalog rendition fills in lazily
/// once the codec config is known (avcC for avc1, the first SPS for avc3); read it
/// via [`catalog`](Self::catalog) or attach the importer to a broadcast catalog
/// with [`crate::publish::Published`].
pub struct Import {
	split: Split,
	track: crate::container::Producer<crate::catalog::hang::Container>,
	catalog: hang::Catalog,
	config: Option<hang::catalog::VideoConfig>,
	jitter: MinFrameDuration,
}

impl Import {
	/// Serve a track request, accepting it at the microsecond timescale.
	pub fn new(request: moq_net::TrackRequest) -> Self {
		let info = moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE);
		Self::from_track(request.accept(info))
	}

	/// Publish on an existing track producer.
	pub fn from_track(track: moq_net::TrackProducer) -> Self {
		Self {
			split: Split::new(),
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy).with_lenient_start(),
			catalog: hang::Catalog::default(),
			config: None,
			jitter: MinFrameDuration::new(),
		}
	}

	/// Pin the wire shape ahead of time; skips the leading-bytes auto-detect.
	pub fn with_mode(mut self, mode: Mode) -> Result<Self> {
		self.split = Split::with_mode(mode)?;
		Ok(self)
	}

	/// Initialize from the codec's leading bytes (avcC for avc1, SPS/PPS for avc3).
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		self.split.initialize(buf)?;
		self.pull_config();
		Ok(())
	}

	/// The standalone catalog once the codec config is known, else `None`.
	pub fn catalog(&self) -> Option<&hang::Catalog> {
		self.config.is_some().then_some(&self.catalog)
	}

	/// The underlying track producer.
	pub fn track(&self) -> &moq_net::TrackProducer {
		self.track.track()
	}

	/// True once the codec config is known and the catalog rendition is published.
	pub fn is_initialized(&self) -> bool {
		self.config.is_some()
	}

	/// Decode from an asynchronous reader (avc3 streaming input).
	pub async fn decode_from<T: AsyncRead + Unpin>(&mut self, reader: &mut T) -> Result<()> {
		let mut buffer = BytesMut::new();
		while reader.read_buf(&mut buffer).await? > 0 {
			self.decode_stream(&mut buffer, None)?;
		}
		Ok(())
	}

	/// Decode a buffer holding (the rest of) a single frame.
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T, pts: Option<moq_net::Timestamp>) -> Result<()> {
		let frames = self.split.decode_frame(buf, pts)?;
		self.pull_config();
		self.write_frames(frames)
	}

	/// Decode a buffer where frame boundaries are unknown (avc3 streaming input).
	pub fn decode_stream<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T, pts: Option<moq_net::Timestamp>) -> Result<()> {
		let frames = self.split.decode_stream(buf, pts)?;
		self.pull_config();
		self.write_frames(frames)
	}

	/// Finish the track, flushing any buffered data.
	pub fn finish(&mut self) -> Result<()> {
		self.track.finish()?;
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	///
	/// Any in-flight avc3 access unit is dropped. Pre-seek NALs would otherwise
	/// leak into the post-seek group with the wrong timestamp.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		self.split.reset();
		self.track.seek(sequence)?;
		Ok(())
	}

	/// Apply a newly-resolved config from the splitter to the catalog rendition.
	fn pull_config(&mut self) {
		if let Some(config) = self.split.take_config() {
			self.catalog
				.video
				.renditions
				.insert(self.track.name().to_string(), config.clone());
			self.config = Some(config);
		}
	}

	/// Write split frames to the track, refining the catalog jitter as it goes.
	fn write_frames(&mut self, frames: impl IntoIterator<Item = Frame>) -> Result<()> {
		for frame in frames {
			let pts = frame.timestamp;
			self.track.write(frame)?;

			if let Some(jitter) = self.jitter.observe(pts)
				&& let Some(c) = self.catalog.video.renditions.get_mut(self.track.name())
			{
				c.jitter = Some(jitter);
			}
		}
		Ok(())
	}
}

impl FrameDecode for Import {
	fn decode<I: IntoIterator<Item = Frame>>(&mut self, frames: I) -> Result<()> {
		self.write_frames(frames)
	}
}

impl Renditions for Import {
	fn renditions(&self) -> &hang::Catalog {
		&self.catalog
	}
}
