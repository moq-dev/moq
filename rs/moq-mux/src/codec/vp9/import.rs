use anyhow::Context;
use bytes::Buf;

use crate::container::jitter::MinFrameDuration;

use super::FrameHeader;

/// A frame-based importer for raw VP9.
///
/// Like VP8, a VP9 elementary stream isn't self-delimiting, so the caller must
/// pass whole frames (or superframes), one per
/// [`decode_frame`](Self::decode_frame). The first key frame's header supplies
/// the catalog config; the track is created lazily.
pub struct Import {
	// The broadcast being produced.
	broadcast: moq_net::BroadcastProducer,

	// The catalog being produced.
	catalog: crate::catalog::Producer,

	// The track being produced, created on the first key frame.
	track: Option<crate::container::Producer<crate::catalog::hang::Container>>,

	// The resolved config, used to detect resolution / format changes.
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
	/// VP9 has no out-of-band configuration record, so this is normally called
	/// with an empty buffer (gstreamer / ffi pass `Bytes::new()`) and the track
	/// is created lazily from the first key frame. If the caller does pass the
	/// first frame here, it's decoded so nothing is dropped.
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> anyhow::Result<()> {
		if buf.has_remaining() {
			self.decode_frame(buf, None)?;
		}
		Ok(())
	}

	fn init(&mut self, vp9: hang::catalog::VP9, width: u16, height: u16) -> anyhow::Result<()> {
		let mut config = hang::catalog::VideoConfig::new(vp9);
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

		let track = self.broadcast.unique_track(".vp09")?;
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

	/// Decode a single VP9 frame (or superframe).
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: Option<crate::container::Timestamp>,
	) -> anyhow::Result<()> {
		let payload = buf.copy_to_bytes(buf.remaining());
		anyhow::ensure!(!payload.is_empty(), "empty VP9 frame");

		let header = FrameHeader::parse(&payload)?;
		if let Some(key) = header.key {
			self.init(key.to_catalog(), key.width, key.height)?;
		}

		// Resolve the timestamp before borrowing `track` so `pts` doesn't hold a
		// `&mut self` across the track write.
		let pts = self.pts(pts)?;
		let track = self
			.track
			.as_mut()
			.context("expected a VP9 key frame before any interframe")?;

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

	// profile 0, 8-bit, CS_BT_601, studio range, 4:2:0, 320x240.
	const KEYFRAME: &[u8] = &[0x82, 0x49, 0x83, 0x42, 0x20, 0x13, 0xf0, 0x0e, 0xf0, 0x00];

	#[tokio::test(start_paused = true)]
	async fn imports_keyframe_then_interframe() {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast.clone(), catalog.clone());

		import.initialize(&mut Bytes::new()).unwrap();
		assert!(!import.is_initialized());

		import
			.decode_frame(
				&mut Bytes::from_static(KEYFRAME),
				Some(Timestamp::from_micros(0).unwrap()),
			)
			.unwrap();

		assert!(import.is_initialized());
		let name = import.track().unwrap().name.clone();
		let config = catalog.lock().video.renditions.get(&name).cloned().unwrap();
		assert!(matches!(config.codec, hang::catalog::VideoCodec::VP9(_)));
		assert_eq!(config.coded_width, Some(320));
		assert_eq!(config.coded_height, Some(240));

		// Interframe: marker(10) profile(00) show_existing(0) frame_type(1) = 0x84.
		import
			.decode_frame(
				&mut Bytes::from_static(&[0x84, 0x00, 0x00]),
				Some(Timestamp::from_micros(33_000).unwrap()),
			)
			.unwrap();

		import.finish().unwrap();
	}

	#[tokio::test(start_paused = true)]
	async fn rejects_interframe_first() {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast.clone(), catalog);

		let mut interframe = Bytes::from_static(&[0x84, 0x00, 0x00]);
		assert!(
			import
				.decode_frame(&mut interframe, Some(Timestamp::from_micros(0).unwrap()))
				.is_err()
		);
	}
}
