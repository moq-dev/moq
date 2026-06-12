use anyhow::Context;
use bytes::Buf;

use crate::container::jitter::MinFrameDuration;

use super::FrameHeader;

/// A frame-based importer for raw VP8.
///
/// A VP8 elementary stream isn't self-delimiting, so the caller must pass whole
/// frames, one per [`decode_frame`](Self::decode_frame). The first key frame's
/// header supplies the catalog dimensions; the track is created lazily so the
/// importer can be constructed before any media arrives.
pub struct Import {
	// The broadcast being produced.
	broadcast: moq_net::BroadcastProducer,

	// The catalog being produced.
	catalog: crate::catalog::Producer,

	// The track being produced, created on the first key frame.
	track: Option<crate::container::Producer<crate::catalog::hang::Container>>,

	// The resolved config, used to detect resolution changes.
	config: Option<hang::catalog::VideoConfig>,

	// Used to compute wall clock timestamps when the caller has none.
	zero: Option<tokio::time::Instant>,

	// Tracks the minimum frame duration and updates the catalog `jitter` field.
	jitter: MinFrameDuration,
}

impl Import {
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: crate::catalog::Producer) -> Self {
		Self {
			broadcast,
			catalog,
			track: None,
			config: None,
			zero: None,
			jitter: MinFrameDuration::new(),
		}
	}

	/// Initialize the importer.
	///
	/// VP8 has no out-of-band configuration record, so this is normally called
	/// with an empty buffer (gstreamer / ffi pass `Bytes::new()`) and the track
	/// is created lazily from the first key frame. If the caller does pass the
	/// first frame here, it's decoded so nothing is dropped.
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> anyhow::Result<()> {
		if buf.has_remaining() {
			self.decode_frame(buf, None)?;
		}
		Ok(())
	}

	fn init(&mut self, width: u16, height: u16) -> anyhow::Result<()> {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.coded_width = Some(width as u32);
		config.coded_height = Some(height as u32);
		config.container = hang::catalog::Container::Legacy;

		if self.config.as_ref() == Some(&config) {
			return Ok(());
		}

		if let Some(track) = self.track.take() {
			tracing::debug!(name = ?track.name, "reinitializing track");
			self.catalog.lock().video.renditions.remove(&track.name);
		}

		let track = self.broadcast.unique_track(".vp8")?;
		tracing::debug!(name = ?track.name, ?config, "starting track");
		self.catalog
			.lock()
			.video
			.renditions
			.insert(track.name.clone(), config.clone());

		self.config = Some(config);
		self.track = Some(crate::container::Producer::new(
			track,
			crate::catalog::hang::Container::Legacy,
		));

		Ok(())
	}

	/// Decode a single VP8 frame.
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: Option<crate::container::Timestamp>,
	) -> anyhow::Result<()> {
		let payload = buf.copy_to_bytes(buf.remaining());
		anyhow::ensure!(!payload.is_empty(), "empty VP8 frame");

		let header = FrameHeader::parse(&payload)?;
		if let Some((width, height)) = header.dimensions {
			self.init(width, height)?;
		}

		// Resolve the timestamp before borrowing `track` so `pts` doesn't hold a
		// `&mut self` across the track write.
		let pts = self.pts(pts)?;
		let track = self
			.track
			.as_mut()
			.context("expected a VP8 key frame before any interframe")?;

		track.write(crate::container::Frame {
			timestamp: pts,
			payload,
			keyframe: header.keyframe,
		})?;

		if let Some(jitter) = self.jitter.observe(pts)
			&& let Some(c) = self.catalog.lock().video.renditions.get_mut(&track.name)
		{
			c.jitter = Some(jitter);
		}

		Ok(())
	}

	/// Returns a reference to the underlying track producer.
	pub fn track(&self) -> anyhow::Result<&moq_net::TrackProducer> {
		Ok(self.track.as_ref().context("not initialized")?.track())
	}

	/// Finish the track, flushing the current group.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		let track = self.track.as_mut().context("not initialized")?;
		track.finish()?;
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> anyhow::Result<()> {
		let track = self.track.as_mut().context("not initialized")?;
		track.seek(sequence)?;
		Ok(())
	}

	pub fn is_initialized(&self) -> bool {
		self.track.is_some()
	}

	fn pts(&mut self, hint: Option<crate::container::Timestamp>) -> anyhow::Result<crate::container::Timestamp> {
		if let Some(pts) = hint {
			return Ok(pts);
		}

		let zero = self.zero.get_or_insert_with(tokio::time::Instant::now);
		Ok(crate::container::Timestamp::from_micros(
			zero.elapsed().as_micros() as u64
		)?)
	}
}

impl Drop for Import {
	fn drop(&mut self) {
		if let Some(track) = self.track.take() {
			tracing::debug!(name = ?track.name, "ending track");
			self.catalog.lock().video.renditions.remove(&track.name);
		}
	}
}

#[cfg(test)]
mod tests {
	use bytes::Bytes;

	use crate::container::Timestamp;

	/// A 320x240 key frame followed by an interframe should create a single VP8
	/// rendition with the right dimensions and emit both frames.
	#[tokio::test(start_paused = true)]
	async fn imports_keyframe_then_interframe() {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast.clone(), catalog.clone());

		// Empty init buffer: the track is created lazily on the first key frame.
		import.initialize(&mut Bytes::new()).unwrap();
		assert!(!import.is_initialized());

		let keyframe = Bytes::from_static(&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00]);
		import
			.decode_frame(&mut keyframe.clone(), Some(Timestamp::from_micros(0).unwrap()))
			.unwrap();

		assert!(import.is_initialized());
		let name = import.track().unwrap().name.clone();
		let config = catalog.lock().video.renditions.get(&name).cloned().unwrap();
		assert_eq!(config.codec, hang::catalog::VideoCodec::VP8);
		assert_eq!(config.coded_width, Some(320));
		assert_eq!(config.coded_height, Some(240));

		// Interframe: no start code or dimensions, but still a valid frame.
		let interframe = Bytes::from_static(&[0x31, 0x00, 0x00, 0xaa, 0xbb]);
		import
			.decode_frame(&mut interframe.clone(), Some(Timestamp::from_micros(33_000).unwrap()))
			.unwrap();

		import.finish().unwrap();
	}

	/// An interframe before any key frame has no dimensions, so the track can't
	/// be created and the Producer rejects a non-keyframe first frame.
	#[tokio::test(start_paused = true)]
	async fn rejects_interframe_first() {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast.clone(), catalog);

		let mut interframe = Bytes::from_static(&[0x31, 0x00, 0x00, 0xaa, 0xbb]);
		assert!(
			import
				.decode_frame(&mut interframe, Some(Timestamp::from_micros(0).unwrap()))
				.is_err()
		);
	}
}
