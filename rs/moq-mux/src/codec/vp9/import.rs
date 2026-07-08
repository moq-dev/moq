use crate::catalog::hang::CatalogExt;
use crate::container::Frame;

use super::FrameHeader;

/// A frame-based importer for raw VP9.
///
/// Like VP8, a VP9 elementary stream isn't self-delimiting, so the caller must
/// pass whole frames (or superframes), one per [`decode`](Self::decode). The first
/// key frame's header supplies the catalog config, so the rendition isn't published
/// until then. Build it with [`new`](Self::new), passing the track producer and the
/// [`catalog::Reserved`](crate::catalog::Reserved) it reserves its rendition from.
pub struct Import<E: CatalogExt = ()> {
	// The track being produced.
	track: crate::container::Producer<crate::catalog::hang::Container>,

	// This importer's catalog rendition, published on the first key frame.
	rendition: crate::catalog::VideoTrack<E>,

	// The resolved config, used to detect resolution / format changes.
	config: Option<hang::catalog::VideoConfig>,
}

impl<E: CatalogExt> Import<E> {
	/// Publish on an existing track producer, reserving the rendition from `reserved`.
	pub fn new(track: moq_net::track::Producer, reserved: crate::catalog::Reserved<E>) -> Self {
		let rendition = reserved.video(track.name());
		Self {
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			rendition,
			config: None,
		}
	}

	/// Initialize the importer.
	///
	/// VP9 has no out-of-band configuration record, so this is normally called with
	/// an empty slice (gstreamer / ffi pass `&[]`) and the catalog is filled from the
	/// first key frame. If the caller does pass the first frame here, it's decoded so
	/// nothing is dropped.
	pub fn initialize(&mut self, buf: &[u8]) -> crate::Result<()> {
		if !buf.is_empty() {
			self.decode(buf, None)?;
		}
		Ok(())
	}

	fn init(&mut self, vp9: hang::catalog::VP9, width: u16, height: u16) -> crate::Result<()> {
		let mut config = hang::catalog::VideoConfig::new(vp9);
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

	/// Decode a single VP9 frame (or superframe).
	pub fn decode<B: moq_net::IntoBytes>(&mut self, frame: B, pts: Option<moq_net::Timestamp>) -> crate::Result<()> {
		if frame.as_ref().is_empty() {
			return Err(super::Error::EmptyFrame.into());
		}

		let header = FrameHeader::parse(frame.as_ref())?;
		if let Some(key) = header.key {
			self.init(key.to_catalog(), key.width, key.height)?;
		}

		let pts = self.rendition.timestamp(pts)?;
		// A key frame starts a new group: close the previous one for the bitrate detector.
		if header.keyframe {
			self.rendition.record_group_end(Some(pts));
		}
		let bytes = frame.as_ref().len();
		self.track.write(Frame {
			timestamp: pts,
			payload: frame.into_bytes(),
			keyframe: header.keyframe,
			duration: None,
		})?;

		self.rendition.record_frame(pts, bytes);

		Ok(())
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
}

#[cfg(test)]
mod tests {
	use bytes::Bytes;

	use moq_net::Timestamp;

	// profile 0, 8-bit, CS_BT_601, studio range, 4:2:0, 320x240.
	const KEYFRAME: &[u8] = &[0x82, 0x49, 0x83, 0x42, 0x20, 0x13, 0xf0, 0x0e, 0xf0, 0x00];

	fn setup() -> (moq_net::track::Producer, crate::catalog::Producer) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let track = broadcast
			.create_track(
				"0.vp9",
				moq_net::track::Info::default().with_timescale(hang::container::TIMESCALE),
			)
			.unwrap();
		(track, catalog)
	}

	#[tokio::test(start_paused = true)]
	async fn imports_keyframe_then_interframe() {
		let (track, catalog) = setup();
		let mut import = super::Import::new(track, catalog.reserve());

		import.initialize(&[]).unwrap();
		assert!(catalog.snapshot().video.renditions.is_empty());

		import
			.decode(KEYFRAME, Some(Timestamp::from_micros(0).unwrap()))
			.unwrap();

		let snapshot = catalog.snapshot();
		let config = snapshot.video.renditions.get("0.vp9").unwrap();
		assert!(matches!(config.codec, hang::catalog::VideoCodec::VP9(_)));
		assert_eq!(config.coded_width, Some(320));
		assert_eq!(config.coded_height, Some(240));

		// Interframe: marker(10) profile(00) show_existing(0) frame_type(1) = 0x84.
		import
			.decode(&[0x84, 0x00, 0x00], Some(Timestamp::from_micros(33_000).unwrap()))
			.unwrap();

		import.finish().unwrap();
	}

	#[tokio::test(start_paused = true)]
	async fn rejects_interframe_first() {
		let (track, catalog) = setup();
		let mut import = super::Import::new(track, catalog.reserve());

		let interframe = Bytes::from_static(&[0x84, 0x00, 0x00]);
		assert!(
			import
				.decode(&interframe, Some(Timestamp::from_micros(0).unwrap()))
				.is_err()
		);
	}
}
