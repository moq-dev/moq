use bytes::Buf;

use crate::container::jitter::MinFrameDuration;
use crate::import::Renditions;

use super::FrameHeader;

/// A frame-based importer for raw VP9.
///
/// Like VP8, a VP9 elementary stream isn't self-delimiting, so the caller must
/// pass whole frames (or superframes), one per
/// [`decode_frame`](Self::decode_frame). The first key frame's header supplies
/// the catalog config, so [`catalog`](Self::catalog) is `None` until then. Build
/// it from a [`moq_net::TrackRequest`] ([`new`](Self::new)) or an existing track
/// ([`from_track`](Self::from_track)).
pub struct Import {
	// The track being produced.
	track: crate::container::Producer<crate::catalog::hang::Container>,

	// The standalone catalog, populated on the first key frame.
	catalog: hang::Catalog,

	// The resolved config, used to detect resolution / format changes.
	config: Option<hang::catalog::VideoConfig>,

	// Used to compute wall clock timestamps when the caller has none.
	zero: Option<tokio::time::Instant>,

	// Tracks the minimum frame duration and updates the catalog `jitter` field.
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
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			catalog: hang::Catalog::default(),
			config: None,
			zero: None,
			jitter: MinFrameDuration::new(),
		}
	}

	/// Initialize the importer.
	///
	/// VP9 has no out-of-band configuration record, so this is normally called
	/// with an empty buffer (gstreamer / ffi pass `Bytes::new()`) and the catalog
	/// is filled from the first key frame. If the caller does pass the first frame
	/// here, it's decoded so nothing is dropped.
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> crate::Result<()> {
		if buf.has_remaining() {
			self.decode_frame(buf, None)?;
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
		self.catalog
			.video
			.renditions
			.insert(self.track.name().to_string(), config.clone());
		self.config = Some(config);

		Ok(())
	}

	/// Decode a single VP9 frame (or superframe).
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: Option<moq_net::Timestamp>,
	) -> crate::Result<()> {
		let payload = buf.copy_to_bytes(buf.remaining());
		if payload.is_empty() {
			return Err(super::Error::EmptyFrame.into());
		}

		let header = FrameHeader::parse(&payload)?;
		if let Some(key) = header.key {
			self.init(key.to_catalog(), key.width, key.height)?;
		}

		let pts = self.pts(pts)?;
		self.track.write(crate::container::Frame {
			timestamp: pts,
			payload,
			keyframe: header.keyframe,
			duration: None,
		})?;

		if let Some(jitter) = self.jitter.observe(pts)
			&& let Some(c) = self.catalog.video.renditions.get_mut(self.track.name())
		{
			c.jitter = Some(jitter);
		}

		Ok(())
	}

	/// The standalone catalog once the first key frame is seen, else `None`.
	pub fn catalog(&self) -> Option<&hang::Catalog> {
		self.config.is_some().then_some(&self.catalog)
	}

	/// The underlying track producer.
	pub fn track(&self) -> &moq_net::TrackProducer {
		self.track.track()
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

	/// True once the first key frame has populated the catalog.
	pub fn is_initialized(&self) -> bool {
		self.config.is_some()
	}

	fn pts(&mut self, hint: Option<moq_net::Timestamp>) -> crate::Result<moq_net::Timestamp> {
		if let Some(pts) = hint {
			return Ok(pts);
		}

		let zero = self.zero.get_or_insert_with(tokio::time::Instant::now);
		Ok(moq_net::Timestamp::from_micros(zero.elapsed().as_micros() as u64)?)
	}
}

impl Renditions for Import {
	fn renditions(&self) -> &hang::Catalog {
		&self.catalog
	}
}

#[cfg(test)]
mod tests {
	use bytes::Bytes;

	use moq_net::Timestamp;

	// profile 0, 8-bit, CS_BT_601, studio range, 4:2:0, 320x240.
	const KEYFRAME: &[u8] = &[0x82, 0x49, 0x83, 0x42, 0x20, 0x13, 0xf0, 0x0e, 0xf0, 0x00];

	#[tokio::test(start_paused = true)]
	async fn imports_keyframe_then_interframe() {
		let mut import = super::Import::new(moq_net::TrackRequest::new("0.vp9"));

		import.initialize(&mut Bytes::new()).unwrap();
		assert!(!import.is_initialized());
		assert!(import.catalog().is_none());

		import
			.decode_frame(
				&mut Bytes::from_static(KEYFRAME),
				Some(Timestamp::from_micros(0).unwrap()),
			)
			.unwrap();

		assert!(import.is_initialized());
		let catalog = import.catalog().unwrap();
		let config = catalog.video.renditions.get(import.track().name()).unwrap();
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
		let mut import = super::Import::new(moq_net::TrackRequest::new("0.vp9"));

		let mut interframe = Bytes::from_static(&[0x84, 0x00, 0x00]);
		assert!(
			import
				.decode_frame(&mut interframe, Some(Timestamp::from_micros(0).unwrap()))
				.is_err()
		);
	}
}
