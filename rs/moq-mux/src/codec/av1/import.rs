use crate::container::jitter::MinFrameDuration;

use bytes::BytesMut;
use bytes::{Buf, Bytes};
use scuffle_av1::seq::SequenceHeaderObu;

use super::Error;
use crate::Result;

/// A decoder for AV1 with inline sequence headers.
pub struct Import {
	// The broadcast being produced.
	broadcast: moq_net::BroadcastProducer,

	// The catalog being produced.
	catalog: crate::catalog::hang::Producer,

	// The track being produced.
	track: Option<crate::container::Producer<crate::catalog::hang::Container>>,

	// Whether the track has been initialized.
	config: Option<hang::catalog::VideoConfig>,

	// The current frame being built.
	current: Frame,

	// Used to compute wall clock timestamps if needed.
	zero: Option<tokio::time::Instant>,

	// Tracks the minimum frame duration and updates the catalog `jitter` field.
	jitter: MinFrameDuration,
}

#[derive(Default)]
struct Frame {
	chunks: BytesMut,
	contains_keyframe: bool,
	contains_frame: bool,
}

impl Import {
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: crate::catalog::hang::Producer) -> Self {
		Self {
			broadcast,
			catalog,
			track: None,
			config: None,
			current: Default::default(),
			zero: None,
			jitter: MinFrameDuration::new(),
		}
	}

	fn init(&mut self, seq_header: &SequenceHeaderObu) -> Result<()> {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::AV1 {
			profile: seq_header.seq_profile,
			level: seq_header
				.operating_points
				.first()
				.map(|op| op.seq_level_idx)
				.unwrap_or(0),
			tier: if seq_header
				.operating_points
				.first()
				.map(|op| op.seq_tier)
				.unwrap_or(false)
			{
				'H'
			} else {
				'M'
			},
			bitdepth: seq_header.color_config.bit_depth as u8,
			mono_chrome: seq_header.color_config.mono_chrome,
			chroma_subsampling_x: seq_header.color_config.subsampling_x,
			chroma_subsampling_y: seq_header.color_config.subsampling_y,
			chroma_sample_position: seq_header.color_config.chroma_sample_position,
			color_primaries: seq_header.color_config.color_primaries,
			transfer_characteristics: seq_header.color_config.transfer_characteristics,
			matrix_coefficients: seq_header.color_config.matrix_coefficients,
			full_range: seq_header.color_config.full_color_range,
		});
		config.coded_width = Some(seq_header.max_frame_width as u32);
		config.coded_height = Some(seq_header.max_frame_height as u32);
		config.container = hang::catalog::Container::Legacy;

		if let Some(old) = &self.config
			&& old == &config
		{
			return Ok(());
		}

		if let Some(track) = &self.track.take() {
			tracing::debug!(name = ?track.name, "reinitializing track");
			self.catalog.lock().video.renditions.remove(&track.name);
		}

		let track = self.broadcast.unique_track(".av01")?;
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

	/// Initialize with minimal config if sequence header parsing fails
	fn init_minimal(&mut self) -> Result<()> {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::AV1 {
			profile: 0,  // Main profile
			level: 0,    // Unknown
			tier: 'M',   // Main tier
			bitdepth: 8, // Assume 8-bit
			mono_chrome: false,
			chroma_subsampling_x: true, // 4:2:0
			chroma_subsampling_y: true,
			chroma_sample_position: 0,
			color_primaries: 2,          // Unspecified
			transfer_characteristics: 2, // Unspecified
			matrix_coefficients: 2,      // Unspecified
			full_range: false,
		});
		config.container = hang::catalog::Container::Legacy;

		let track = self.broadcast.unique_track(".av01")?;
		tracing::debug!(name = ?track.name, "starting track with minimal config");
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

	/// Initialize the decoder with sequence header and other metadata OBUs.
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		let data = buf.as_ref();

		// Handle av1C format (MP4/container initialization)
		// av1C box starts with 0x81 (marker=1, version=1) per ISO/IEC 14496-15
		if data.len() >= 4 && data[0] == 0x81 && data.len() >= 16 {
			self.init_from_av1c(data)?;
			buf.advance(data.len());
			return Ok(());
		}

		// Handle raw OBU format
		let mut obus = ObuIterator::new(buf);
		while let Some(obu) = obus.next().transpose()? {
			self.decode_obu(obu, None)?;
		}

		if let Some(obu) = obus.flush()? {
			self.decode_obu(obu, None)?;
		}

		Ok(())
	}

	fn init_from_av1c(&mut self, data: &[u8]) -> Result<()> {
		// Parse av1C box structure
		let seq_profile = (data[1] >> 5) & 0x07;
		let seq_level_idx = data[1] & 0x1F;
		let tier = ((data[2] >> 7) & 0x01) == 1;
		let high_bitdepth = ((data[2] >> 6) & 0x01) == 1;
		let twelve_bit = ((data[2] >> 5) & 0x01) == 1;

		// Resolution unknown from av1C - will be updated when first sequence header arrives
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::AV1 {
			profile: seq_profile,
			level: seq_level_idx,
			tier: if tier { 'H' } else { 'M' },
			bitdepth: super::bitdepth(twelve_bit, high_bitdepth),
			mono_chrome: ((data[2] >> 4) & 0x01) == 1,
			chroma_subsampling_x: ((data[2] >> 3) & 0x01) == 1,
			chroma_subsampling_y: ((data[2] >> 2) & 0x01) == 1,
			chroma_sample_position: data[2] & 0x03,
			color_primaries: 1,
			transfer_characteristics: 1,
			matrix_coefficients: 1,
			full_range: false,
		});
		config.container = hang::catalog::Container::Legacy;

		if let Some(old) = &self.config
			&& old == &config
		{
			return Ok(());
		}

		if let Some(track) = &self.track.take() {
			self.catalog.lock().video.renditions.remove(&track.name);
		}

		let track = self.broadcast.unique_track(".av01")?;
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

	/// Returns a reference to the underlying track producer.
	pub fn track(&self) -> Result<&moq_net::TrackProducer> {
		Ok(self.track.as_ref().ok_or(Error::NotInitialized)?.track())
	}

	/// Decode as much data as possible from the given buffer.
	pub fn decode_stream<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T, pts: Option<moq_net::Timestamp>) -> Result<()> {
		let obus = ObuIterator::new(buf);

		for obu in obus {
			// Generate PTS for each OBU to avoid reusing same timestamp
			let pts = self.pts(pts)?;
			self.decode_obu(obu?, Some(pts))?;
		}

		Ok(())
	}

	/// Decode all data in the buffer, assuming the buffer contains (the rest of) a frame.
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T, pts: Option<moq_net::Timestamp>) -> Result<()> {
		let pts = self.pts(pts)?;
		let mut obus = ObuIterator::new(buf);

		while let Some(obu) = obus.next().transpose()? {
			self.decode_obu(obu, Some(pts))?;
		}

		if let Some(obu) = obus.flush()? {
			self.decode_obu(obu, Some(pts))?;
		}

		self.maybe_start_frame(Some(pts))?;

		Ok(())
	}

	fn decode_obu(&mut self, obu_data: Bytes, pts: Option<moq_net::Timestamp>) -> Result<()> {
		if obu_data.is_empty() {
			return Err(Error::ObuTooShort.into());
		}

		// Parse OBU header - this consumes header + extension + LEB128 size
		let mut reader = &obu_data[..];
		let header = scuffle_av1::ObuHeader::parse(&mut reader)?;

		// Calculate payload offset by seeing how much the parser consumed
		let payload_offset = obu_data.len() - reader.len();

		// Match on the ObuType enum directly
		use scuffle_av1::ObuType;
		match header.obu_type {
			ObuType::SequenceHeader => {
				match SequenceHeaderObu::parse(header, &mut &obu_data[payload_offset..]) {
					Ok(seq_header) => {
						self.init(&seq_header)?;
					}
					Err(_) => {
						// Use minimal config so stream can work (catalog won't have full info)
						if self.track.is_none() {
							tracing::debug!("Sequence header parsing failed, initializing with minimal config");
							self.init_minimal()?;
						}
					}
				}

				self.current.contains_keyframe = true;
			}
			ObuType::TemporalDelimiter => {
				self.maybe_start_frame(pts)?;
			}
			ObuType::FrameHeader | ObuType::Frame => {
				let is_keyframe = if obu_data.len() > payload_offset {
					let data = &obu_data[payload_offset..];
					if data.is_empty() {
						false
					} else {
						let first_byte = data[0];

						let show_existing_frame = (first_byte >> 7) & 1;

						if show_existing_frame == 1 {
							self.current.contains_keyframe
						} else {
							let frame_type = (first_byte >> 5) & 0b11;

							frame_type == 0
						}
					}
				} else {
					tracing::warn!(
						"Frame OBU too short: {} bytes (payload_offset={})",
						obu_data.len(),
						payload_offset
					);
					false
				};

				if is_keyframe || self.current.contains_keyframe {
					self.current.contains_keyframe = true;
				}

				self.current.contains_frame = true;
			}
			ObuType::Metadata => {
				self.maybe_start_frame(pts)?;
			}
			ObuType::TileGroup | ObuType::TileList => {
				self.current.contains_frame = true;
			}
			_ => {
				// Other OBU types - just include them
			}
		}

		tracing::trace!(?header.obu_type, "parsed OBU");

		self.current.chunks.extend_from_slice(&obu_data);

		Ok(())
	}

	fn maybe_start_frame(&mut self, pts: Option<moq_net::Timestamp>) -> Result<()> {
		if !self.current.contains_frame {
			return Ok(());
		}

		let track = self.track.as_mut().ok_or(Error::MissingSequenceHeader)?;
		let pts = pts.ok_or(Error::MissingTimestamp)?;

		let payload = std::mem::take(&mut self.current.chunks).freeze();

		let frame = crate::container::Frame {
			timestamp: pts,
			payload,
			keyframe: self.current.contains_keyframe,
		};

		track.write(frame)?;

		if let Some(jitter) = self.jitter.observe(pts)
			&& let Some(c) = self.catalog.lock().video.renditions.get_mut(&track.name)
		{
			c.jitter = Some(jitter);
		}

		self.current.contains_keyframe = false;
		self.current.contains_frame = false;

		Ok(())
	}

	/// Finish the track, flushing the current group.
	pub fn finish(&mut self) -> Result<()> {
		let track = self.track.as_mut().ok_or(Error::NotInitialized)?;
		track.finish()?;
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	///
	/// Any in-flight access unit is dropped. Pre-seek OBUs would otherwise leak
	/// into the post-seek group with the wrong timestamp.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		self.current = Frame::default();
		let track = self.track.as_mut().ok_or(Error::NotInitialized)?;
		track.seek(sequence)?;
		Ok(())
	}

	pub fn is_initialized(&self) -> bool {
		self.track.is_some()
	}

	fn pts(&mut self, hint: Option<moq_net::Timestamp>) -> Result<moq_net::Timestamp> {
		if let Some(pts) = hint {
			return Ok(pts);
		}

		let zero = self.zero.get_or_insert_with(tokio::time::Instant::now);
		Ok(moq_net::Timestamp::from_micros(zero.elapsed().as_micros() as u64)?)
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

/// Iterator over AV1 Open Bitstream Units (OBUs)
struct ObuIterator<'a, T: Buf + AsRef<[u8]> + 'a> {
	buf: &'a mut T,
}

impl<'a, T: Buf + AsRef<[u8]> + 'a> ObuIterator<'a, T> {
	pub fn new(buf: &'a mut T) -> Self {
		Self { buf }
	}

	pub fn flush(self) -> Result<Option<Bytes>> {
		let remaining = self.buf.remaining();
		if remaining == 0 {
			return Ok(None);
		}

		let obu = self.buf.copy_to_bytes(remaining);
		Ok(Some(obu))
	}
}

impl<'a, T: Buf + AsRef<[u8]> + 'a> Iterator for ObuIterator<'a, T> {
	type Item = Result<Bytes>;

	fn next(&mut self) -> Option<Self::Item> {
		if self.buf.remaining() == 0 {
			return None;
		}

		// Parse OBU header to get size
		let data = self.buf.as_ref();
		if data.is_empty() {
			return None;
		}

		// OBU header format:
		// - obu_forbidden_bit (1)
		// - obu_type (4)
		// - obu_extension_flag (1)
		// - obu_has_size_field (1)
		// - obu_reserved_1bit (1)

		let header = data[0];
		let has_extension = (header >> 2) & 1 == 1;
		let has_size = (header >> 1) & 1 == 1;

		if !has_size {
			let remaining = self.buf.remaining();
			let obu = self.buf.copy_to_bytes(remaining);
			return Some(Ok(obu));
		}

		// LEB128 size field starts after header byte and optional extension byte
		let mut size: usize = 0;
		let mut offset = if has_extension { 2 } else { 1 };
		let mut shift = 0;

		loop {
			if offset >= data.len() {
				return None;
			}

			let byte = data[offset];
			offset += 1;

			size |= ((byte & 0x7F) as usize) << shift;
			shift += 7;

			if byte & 0x80 == 0 {
				break;
			}

			if shift >= 56 {
				return Some(Err(Error::ObuSizeTooLarge.into()));
			}
		}

		let total_size = offset + size;

		if total_size > self.buf.remaining() {
			return None;
		}

		let obu = self.buf.copy_to_bytes(total_size);
		Some(Ok(obu))
	}
}
