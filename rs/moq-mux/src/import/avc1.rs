use super::jitter::MinFrameDuration;

use anyhow::Context;
use bytes::Bytes;

/// A decoder for H.264 in AVCC format (length-prefixed NALUs with out-of-band SPS/PPS).
///
/// This is the "avc1" style where the decoder description (AVCDecoderConfigurationRecord)
/// is provided out-of-band via the catalog, and frames contain length-prefixed NAL units
/// without inline parameter sets.
pub struct Avc1 {
	broadcast: moq_net::BroadcastProducer,
	catalog: crate::catalog::Producer,
	track: Option<crate::container::Producer<crate::container::Hang>>,
	config: Option<hang::catalog::VideoConfig>,

	/// NALU length size from the AVCDecoderConfigurationRecord (typically 4).
	length_size: usize,

	/// Used to compute wall clock timestamps if needed.
	zero: Option<tokio::time::Instant>,

	/// Tracks the minimum frame duration and updates the catalog `jitter` field.
	jitter: MinFrameDuration,
}

impl Avc1 {
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: crate::catalog::Producer) -> Self {
		Self {
			broadcast,
			catalog,
			track: None,
			config: None,
			length_size: 4,
			zero: None,
			jitter: MinFrameDuration::new(),
		}
	}

	/// Initialize with an AVCDecoderConfigurationRecord (the extradata from the container).
	///
	/// Parses the SPS to extract profile/level/dimensions for the catalog,
	/// and stores the raw record as the WebCodecs `description`.
	/// The buffer is fully consumed.
	pub fn initialize<T: bytes::Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> anyhow::Result<()> {
		let avcc_bytes = buf.as_ref();
		let avcc = crate::codec::h264::Avcc::parse(avcc_bytes)?;
		self.length_size = avcc.length_size;

		let config = hang::catalog::VideoConfig {
			coded_width: avcc.coded_width,
			coded_height: avcc.coded_height,
			codec: hang::catalog::H264 {
				profile: avcc.profile,
				constraints: avcc.constraints,
				level: avcc.level,
				inline: false,
			}
			.into(),
			description: Some(Bytes::copy_from_slice(avcc_bytes)),
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

		let mut catalog = self.catalog.lock();

		if let Some(track) = &self.track.take() {
			tracing::debug!(name = ?track.name, "reinitializing avc1 track");
			catalog.video.renditions.remove(&track.name);
		}

		let track = self.broadcast.unique_track(".avc1")?;
		tracing::debug!(name = ?track.name, ?config, "starting avc1 track");
		catalog.video.renditions.insert(track.name.clone(), config.clone());

		self.config = Some(config);
		self.track = Some(crate::container::Producer::new(track, crate::container::Hang::Legacy));

		buf.advance(buf.remaining());

		Ok(())
	}

	/// Returns a reference to the underlying track producer.
	pub fn track(&self) -> anyhow::Result<&moq_net::TrackProducer> {
		Ok(&self.track.as_ref().context("not initialized")?.track)
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
		let track = self.track.as_mut().context("not initialized; call init() first")?;

		track.write(crate::container::Frame {
			timestamp: pts,
			payload: data.to_vec().into(),
			keyframe,
		})?;

		if let Some(jitter) = self.jitter.observe(pts)
			&& let Some(c) = self.catalog.lock().video.renditions.get_mut(&track.name)
		{
			c.jitter = Some(jitter);
		}

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
		let track = self.track.as_mut().context("not initialized")?;
		track.finish()?;
		Ok(())
	}

	pub fn is_initialized(&self) -> bool {
		self.track.is_some()
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
		if let Some(track) = self.track.take() {
			tracing::debug!(name = ?track.name, "ending avc1 track");
			self.catalog.lock().video.renditions.remove(&track.name);
		}
	}
}
