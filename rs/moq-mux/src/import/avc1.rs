use bytes::Bytes;

/// A decoder for H.264 in AVCC format (length-prefixed NALUs with out-of-band SPS/PPS).
///
/// This is the "avc1" style where the decoder description (AVCDecoderConfigurationRecord)
/// is provided out-of-band via the catalog, and frames contain length-prefixed NAL units
/// without inline parameter sets.
pub struct Avc1 {
	catalog: crate::CatalogProducer,
	track: hang::container::OrderedProducer,
	config: Option<hang::catalog::VideoConfig>,

	/// NALU length size from the AVCDecoderConfigurationRecord (typically 4).
	length_size: usize,

	/// Used to compute wall clock timestamps if needed.
	zero: Option<tokio::time::Instant>,

	// Jitter tracking: minimum duration between consecutive frames.
	last_timestamp: Option<hang::container::Timestamp>,
	min_duration: Option<hang::container::Timestamp>,
	jitter: Option<hang::container::Timestamp>,
}

impl Avc1 {
	// TODO: Make this fallible (return Result) instead of panicking — breaking change, do on `dev` branch.
	pub fn new(mut broadcast: moq_lite::BroadcastProducer, catalog: crate::CatalogProducer) -> Self {
		let track = broadcast.unique_track(".avc1").expect("failed to create avc1 track");

		Self {
			catalog,
			track: track.into(),
			config: None,
			length_size: 4,
			zero: None,
			last_timestamp: None,
			min_duration: None,
			jitter: None,
		}
	}

	/// Initialize with an AVCDecoderConfigurationRecord (the extradata from the container).
	///
	/// Parses the SPS to extract profile/level/dimensions for the catalog,
	/// and stores the raw record as the WebCodecs `description`.
	/// The buffer is fully consumed.
	pub fn initialize<T: bytes::Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> anyhow::Result<()> {
		let avcc = buf.as_ref();
		anyhow::ensure!(avcc.len() >= 6, "AVCDecoderConfigurationRecord too short");

		let profile = avcc[1];
		let constraints = avcc[2];
		let level = avcc[3];
		self.length_size = (avcc[4] & 0x03) as usize + 1;
		let num_sps = avcc[5] & 0x1f;

		let mut offset = 6usize;
		let mut width = 0u32;
		let mut height = 0u32;

		if num_sps > 0 && offset + 2 <= avcc.len() {
			let sps_len = u16::from_be_bytes([avcc[offset], avcc[offset + 1]]) as usize;
			offset += 2;

			if offset + sps_len <= avcc.len() && !avcc[offset..].is_empty() {
				let sps_nalu = &avcc[offset..offset + sps_len];
				let rbsp = h264_parser::nal::ebsp_to_rbsp(&sps_nalu[1..]);
				if let Ok(sps) = h264_parser::Sps::parse(&rbsp) {
					width = sps.width;
					height = sps.height;
				}
			}
		}

		let config = hang::catalog::VideoConfig {
			coded_width: if width > 0 { Some(width) } else { None },
			coded_height: if height > 0 { Some(height) } else { None },
			codec: hang::catalog::H264 {
				profile,
				constraints,
				level,
				inline: false,
			}
			.into(),
			description: Some(Bytes::copy_from_slice(avcc)),
			framerate: None,
			bitrate: None,
			display_ratio_width: None,
			display_ratio_height: None,
			optimize_for_latency: None,
			container: hang::catalog::Container::Legacy,
			jitter: None,
		};

		if let Some(old) = &self.config
			&& old == &config
		{
			return Ok(());
		}

		// Update the catalog entry (track was created eagerly in new()).
		let mut catalog = self.catalog.lock();
		catalog
			.video
			.renditions
			.insert(self.track.info.name.clone(), config.clone());

		tracing::debug!(name = ?self.track.info.name, ?config, "updated catalog");

		self.config = Some(config);

		buf.advance(buf.remaining());

		Ok(())
	}

	/// Decode an AVCC-formatted H.264 packet (length-prefixed NALUs).
	///
	/// If `pts` is `None`, the wall clock time is used.
	/// Keyframes are detected automatically from the NAL unit types.
	/// The buffer is fully consumed.
	pub fn decode<T: bytes::Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: Option<hang::container::Timestamp>,
	) -> anyhow::Result<()> {
		let data = buf.as_ref();
		let pts = self.pts(pts)?;
		let keyframe = self.is_keyframe(data);
		let track = &mut self.track;

		if keyframe {
			track.keyframe()?;
		}

		track.write(hang::container::Frame {
			timestamp: pts,
			payload: data.to_vec().into(),
		})?;

		// Track the minimum frame duration and update catalog jitter.
		if let Some(last) = self.last_timestamp
			&& let Ok(duration) = pts.checked_sub(last)
			&& duration < self.min_duration.unwrap_or(hang::container::Timestamp::MAX)
		{
			self.min_duration = Some(duration);

			if duration < self.jitter.unwrap_or(hang::container::Timestamp::MAX) {
				self.jitter = Some(duration);

				if let Ok(jitter) = duration.convert() {
					if let Some(c) = self.catalog.lock().video.renditions.get_mut(&self.track.info.name) {
						c.jitter = Some(jitter);
					}
				}
			}
		}
		self.last_timestamp = Some(pts);

		buf.advance(buf.remaining());

		Ok(())
	}

	/// Detect if an AVCC packet contains an IDR (keyframe) by scanning the NAL types.
	fn is_keyframe(&self, data: &[u8]) -> bool {
		let mut offset = 0;
		while offset + self.length_size <= data.len() {
			let nal_len = match self.length_size {
				1 => data[offset] as usize,
				2 => u16::from_be_bytes([data[offset], data[offset + 1]]) as usize,
				3 => u32::from_be_bytes([0, data[offset], data[offset + 1], data[offset + 2]]) as usize,
				4 => u32::from_be_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]]) as usize,
				_ => return false,
			};

			offset += self.length_size;
			if offset + nal_len > data.len() {
				break;
			}

			if nal_len > 0 {
				let nal_type = data[offset] & 0x1f;
				if nal_type == 5 {
					// IDR slice
					return true;
				}
			}

			offset += nal_len;
		}

		false
	}

	/// Finish the track.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		self.track.finish()?;
		Ok(())
	}

	/// Returns true if the codec config has been detected and inserted into the catalog.
	pub fn is_initialized(&self) -> bool {
		self.config.is_some()
	}

	/// Returns a reference to the underlying track producer.
	pub fn track(&self) -> &moq_lite::TrackProducer {
		&self.track
	}

	fn pts(&mut self, hint: Option<hang::container::Timestamp>) -> anyhow::Result<hang::container::Timestamp> {
		if let Some(pts) = hint {
			return Ok(pts);
		}

		let zero = self.zero.get_or_insert_with(tokio::time::Instant::now);
		Ok(hang::container::Timestamp::from_micros(
			zero.elapsed().as_micros() as u64
		)?)
	}
}

impl Drop for Avc1 {
	fn drop(&mut self) {
		tracing::debug!(name = ?self.track.info.name, "ending avc1 track");
		self.catalog.lock().video.remove(&self.track.info.name);
	}
}
