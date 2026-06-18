use bytes::Bytes;

use crate::catalog::hang::CatalogExt;
use crate::container::Frame;
use crate::container::jitter::MinFrameDuration;

use super::FrameHeader;

/// A frame-based importer for raw VP8.
///
/// A VP8 elementary stream isn't self-delimiting, so the caller must pass whole
/// frames, one per [`decode`](Self::decode). The first key frame's header supplies
/// the catalog dimensions, so the rendition isn't published until then. Build it
/// with [`new`](Self::new), passing the track producer and the
/// [`catalog::Producer`](crate::catalog::Producer) it publishes into.
pub struct Import<E: CatalogExt = ()> {
	// The track being produced.
	track: crate::container::Producer<crate::catalog::hang::Container>,

	// This importer's catalog rendition, published on the first key frame.
	rendition: crate::catalog::VideoTrack<E>,

	// The resolved config, used to detect resolution changes.
	config: Option<hang::catalog::VideoConfig>,

	// Tracks the minimum frame duration and updates the catalog `jitter` field.
	jitter: MinFrameDuration,
}

impl<E: CatalogExt> Import<E> {
	/// Publish on an existing track producer, registering the rendition in `catalog`.
	pub fn new(track: moq_net::TrackProducer, catalog: crate::catalog::Producer<E>) -> Self {
		let rendition = catalog.video_track(track.name());
		Self {
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			rendition,
			config: None,
			jitter: MinFrameDuration::new(),
		}
	}

	/// Initialize the importer.
	///
	/// VP8 has no out-of-band configuration record, so this is normally called with
	/// an empty slice (gstreamer / ffi pass `&[]`) and the catalog is filled from the
	/// first key frame. If the caller does pass the first frame here, it's decoded so
	/// nothing is dropped.
	pub fn initialize(&mut self, buf: &[u8]) -> crate::Result<()> {
		if !buf.is_empty() {
			self.decode(buf, None)?;
		}
		Ok(())
	}

	fn init(&mut self, width: u16, height: u16) -> crate::Result<()> {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.coded_width = Some(width as u32);
		config.coded_height = Some(height as u32);
		config.container = hang::catalog::Container::Legacy;

		if self.config.as_ref() == Some(&config) {
			return Ok(());
		}

		tracing::debug!(name = ?self.track.name(), ?config, "starting track");
		self.rendition.set(config.clone());
		self.config = Some(config);

		Ok(())
	}

	/// Decode a single VP8 frame.
	pub fn decode(&mut self, frame: &[u8], pts: Option<moq_net::Timestamp>) -> crate::Result<()> {
		if frame.is_empty() {
			return Err(super::Error::EmptyFrame.into());
		}
		let payload = Bytes::copy_from_slice(frame);

		let header = FrameHeader::parse(&payload)?;
		if let Some((width, height)) = header.dimensions {
			self.init(width, height)?;
		}

		let pts = self.rendition.timestamp(pts)?;
		self.track.write(Frame {
			timestamp: pts,
			payload,
			keyframe: header.keyframe,
			duration: None,
		})?;

		if let Some(jitter) = self.jitter.observe(pts) {
			self.rendition.update(|c| c.jitter = Some(jitter));
		}

		Ok(())
	}

	/// A watch-only handle to this track's subscriber demand.
	pub fn demand(&self) -> moq_net::TrackDemand {
		self.track.track().demand()
	}

	/// Finish the track, flushing the current group.
	pub fn finish(&mut self) -> crate::Result<()> {
		self.track.finish()?;
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> crate::Result<()> {
		self.track.seek(sequence)?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use bytes::Bytes;

	use moq_net::Timestamp;

	fn setup() -> (moq_net::TrackProducer, crate::catalog::Producer) {
		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let track = broadcast
			.create_track(
				"0.vp8",
				moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
			)
			.unwrap();
		(track, catalog)
	}

	/// A 320x240 key frame followed by an interframe should create a single VP8
	/// rendition with the right dimensions and emit both frames.
	#[tokio::test(start_paused = true)]
	async fn imports_keyframe_then_interframe() {
		let (track, catalog) = setup();
		let mut import = super::Import::new(track, catalog.clone());

		// Empty init buffer: the catalog is filled on the first key frame.
		import.initialize(&[]).unwrap();
		assert!(catalog.snapshot().video.renditions.is_empty());

		let keyframe = Bytes::from_static(&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00]);
		import
			.decode(&keyframe, Some(Timestamp::from_micros(0).unwrap()))
			.unwrap();

		let snapshot = catalog.snapshot();
		let config = snapshot.video.renditions.get("0.vp8").unwrap();
		assert_eq!(config.codec, hang::catalog::VideoCodec::VP8);
		assert_eq!(config.coded_width, Some(320));
		assert_eq!(config.coded_height, Some(240));

		// Interframe: no start code or dimensions, but still a valid frame.
		let interframe = Bytes::from_static(&[0x31, 0x00, 0x00, 0xaa, 0xbb]);
		import
			.decode(&interframe, Some(Timestamp::from_micros(33_000).unwrap()))
			.unwrap();

		import.finish().unwrap();
	}

	/// An interframe before any key frame has no dimensions, so the Producer
	/// rejects a non-keyframe as the first frame in a group.
	#[tokio::test(start_paused = true)]
	async fn rejects_interframe_first() {
		let (track, catalog) = setup();
		let mut import = super::Import::new(track, catalog);

		let interframe = Bytes::from_static(&[0x31, 0x00, 0x00, 0xaa, 0xbb]);
		assert!(
			import
				.decode(&interframe, Some(Timestamp::from_micros(0).unwrap()))
				.is_err()
		);
	}
}
