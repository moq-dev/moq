use super::annexb::{NalIterator, START_CODE};
use super::jitter::MinFrameDuration;

use anyhow::Context;
use bytes::{Buf, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt};

/// A decoder for H.264 with inline SPS/PPS.
pub struct Avc3 {
	// The catalog being produced.
	catalog: crate::catalog::Producer,

	// The track being produced.
	//
	// Created eagerly in `new()` so callers can monitor `used()`/`unused()`
	// before any frames arrive. The catalog rendition is added/updated lazily
	// in `init()` once the codec config is known from the SPS.
	track: crate::container::Producer<crate::container::Hang>,

	// Whether the track has been initialized.
	// If it changes, then we'll reinitialize with a new config.
	config: Option<hang::catalog::VideoConfig>,

	// The current frame being built.
	current: Frame,

	// Used to compute wall clock timestamps if needed.
	zero: Option<tokio::time::Instant>,

	// Cached parameter set NALs for re-insertion before keyframes.
	cached_sps: Option<Bytes>,
	cached_pps: Option<Bytes>,

	// Tracks the minimum frame duration and updates the catalog `jitter` field.
	jitter: MinFrameDuration,
}

impl Avc3 {
	pub fn new(mut broadcast: moq_lite::BroadcastProducer, catalog: crate::catalog::Producer) -> Self {
		// Create the track eagerly so callers can monitor used/unused before any frames arrive.
		// The catalog entry is added later in init() once the codec config is known.
		let track = broadcast.unique_track(".avc3").expect("failed to create avc3 track");

		Self {
			catalog,
			track: crate::container::Producer::new(track, crate::container::Hang::Legacy),
			config: None,
			current: Default::default(),
			zero: None,
			cached_sps: None,
			cached_pps: None,
			jitter: MinFrameDuration::new(),
		}
	}

	/// Returns a reference to the underlying track producer, e.g. for
	/// monitoring subscriber state via `used()`/`unused()`.
	pub fn track(&self) -> &moq_lite::TrackProducer {
		&self.track.track
	}

	fn init(&mut self, sps: &h264_parser::Sps) -> anyhow::Result<()> {
		let constraint_flags: u8 = ((sps.constraint_set0_flag as u8) << 7)
			| ((sps.constraint_set1_flag as u8) << 6)
			| ((sps.constraint_set2_flag as u8) << 5)
			| ((sps.constraint_set3_flag as u8) << 4)
			| ((sps.constraint_set4_flag as u8) << 3)
			| ((sps.constraint_set5_flag as u8) << 2);

		let config = hang::catalog::VideoConfig {
			coded_width: Some(sps.width),
			coded_height: Some(sps.height),
			codec: hang::catalog::H264 {
				profile: sps.profile_idc,
				constraints: constraint_flags,
				level: sps.level_idc,
				inline: true,
			}
			.into(),
			description: None,
			// TODO: populate these fields
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

		// Insert/update the catalog rendition (track was created eagerly in new()).
		let mut catalog = self.catalog.lock();
		catalog.video.renditions.insert(self.track.name.clone(), config.clone());
		tracing::debug!(name = ?self.track.name, ?config, "updated catalog");

		self.config = Some(config);

		Ok(())
	}

	/// Initialize the decoder with SPS/PPS and other non-slice NALs.
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> anyhow::Result<()> {
		let mut nals = NalIterator::new(buf);

		while let Some(nal) = nals.next().transpose()? {
			self.decode_nal(nal, None)?;
		}

		if let Some(nal) = nals.flush()? {
			self.decode_nal(nal, None)?;
		}

		Ok(())
	}

	/// Decode from an asynchronous reader.
	pub async fn decode_from<T: AsyncRead + Unpin>(&mut self, reader: &mut T) -> anyhow::Result<()> {
		let mut buffer = BytesMut::new();
		while reader.read_buf(&mut buffer).await? > 0 {
			self.decode_stream(&mut buffer, None)?;
		}

		Ok(())
	}

	/// Decode as much data as possible from the given buffer.
	///
	/// Unlike [Self::decode_frame], this method needs the start code for the next frame.
	/// This means it works for streaming media (ex. stdin) but adds a frame of latency.
	///
	/// TODO: This currently associates PTS with the *previous* frame, as part of `maybe_start_frame`.
	pub fn decode_stream<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: Option<hang::container::Timestamp>,
	) -> anyhow::Result<()> {
		let pts = self.pts(pts)?;

		// Iterate over the NAL units in the buffer based on start codes.
		let nals = NalIterator::new(buf);

		for nal in nals {
			self.decode_nal(nal?, Some(pts))?;
		}

		Ok(())
	}

	/// Decode all data in the buffer, assuming the buffer contains (the rest of) a frame.
	///
	/// Unlike [Self::decode_stream], this is called when we know NAL boundaries.
	/// This can avoid a frame of latency just waiting for the next frame's start code.
	/// This can also be used when EOF is detected to flush the final frame.
	///
	/// NOTE: The next decode will fail if it doesn't begin with a start code.
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: Option<hang::container::Timestamp>,
	) -> anyhow::Result<()> {
		let pts = self.pts(pts)?;
		// Iterate over the NAL units in the buffer based on start codes.
		let mut nals = NalIterator::new(buf);

		// Iterate over each NAL that is followed by a start code.
		while let Some(nal) = nals.next().transpose()? {
			self.decode_nal(nal, Some(pts))?;
		}

		// Assume the rest of the buffer is a single NAL.
		if let Some(nal) = nals.flush()? {
			self.decode_nal(nal, Some(pts))?;
		}

		// Flush the frame if we read a slice.
		self.maybe_start_frame(Some(pts))?;

		Ok(())
	}

	fn decode_nal(&mut self, nal: Bytes, pts: Option<hang::container::Timestamp>) -> anyhow::Result<()> {
		let header = nal.first().context("NAL unit is too short")?;
		let forbidden_zero_bit = (header >> 7) & 1;
		anyhow::ensure!(forbidden_zero_bit == 0, "forbidden zero bit is not zero");

		let nal_unit_type = header & 0b11111;
		let nal_type = NalType::try_from(nal_unit_type).ok();

		match nal_type {
			Some(NalType::Sps) => {
				self.maybe_start_frame(pts)?;

				// Try to reinitialize the track if the SPS has changed.
				let rbsp = h264_parser::nal::ebsp_to_rbsp(&nal[1..]);
				let sps = h264_parser::Sps::parse(&rbsp)?;
				self.init(&sps)?;

				// PPS is tied to SPS context; drop cached PPS when SPS changes.
				if self.cached_sps.as_ref().is_some_and(|cached| cached != &nal) {
					self.cached_pps = None;
					self.current.contains_pps = false;
				}

				self.cached_sps = Some(nal.clone());
				self.current.contains_sps = true;
			}
			Some(NalType::Pps) => {
				self.maybe_start_frame(pts)?;

				self.cached_pps = Some(nal.clone());
				self.current.contains_pps = true;
			}
			Some(NalType::Aud) | Some(NalType::Sei) => {
				self.maybe_start_frame(pts)?;
			}
			Some(NalType::IdrSlice) => {
				// Insert cached SPS/PPS before keyframes if not already present in this frame.
				if !self.current.contains_sps
					&& let Some(sps) = &self.cached_sps
				{
					self.current.chunks.extend_from_slice(&START_CODE);
					self.current.chunks.extend_from_slice(sps);
					self.current.contains_sps = true;
				}
				if !self.current.contains_pps
					&& let Some(pps) = &self.cached_pps
				{
					self.current.chunks.extend_from_slice(&START_CODE);
					self.current.chunks.extend_from_slice(pps);
					self.current.contains_pps = true;
				}

				self.current.contains_idr = true;
				self.current.contains_slice = true;
			}
			Some(NalType::NonIdrSlice)
			| Some(NalType::DataPartitionA)
			| Some(NalType::DataPartitionB)
			| Some(NalType::DataPartitionC) => {
				// first_mb_in_slice flag, means this is the first frame of a slice.
				if nal.get(1).context("NAL unit is too short")? & 0x80 != 0 {
					self.maybe_start_frame(pts)?;
				}

				self.current.contains_slice = true;
			}
			_ => {}
		}

		tracing::trace!(kind = ?nal_type, "parsed NAL");

		// Replace the original start code with a canonical 4-byte start code (marginally easier
		// for downstream players, e.g. MSE).
		self.current.chunks.extend_from_slice(&START_CODE);
		self.current.chunks.extend_from_slice(&nal);

		Ok(())
	}

	fn maybe_start_frame(&mut self, pts: Option<hang::container::Timestamp>) -> anyhow::Result<()> {
		// If we haven't seen any slices, we shouldn't flush yet.
		if !self.current.contains_slice {
			return Ok(());
		}

		let pts = pts.context("missing timestamp")?;

		let payload = std::mem::take(&mut self.current.chunks).freeze();

		let frame = crate::container::Frame {
			timestamp: pts,
			payload,
			keyframe: self.current.contains_idr,
		};

		self.track.write(frame)?;

		if let Some(jitter) = self.jitter.observe(pts)
			&& let Some(c) = self.catalog.lock().video.renditions.get_mut(&self.track.name)
		{
			c.jitter = Some(jitter);
		}

		self.current.contains_idr = false;
		self.current.contains_slice = false;
		self.current.contains_sps = false;
		self.current.contains_pps = false;

		Ok(())
	}

	/// Finish the track, flushing the current group.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		self.track.finish()?;
		Ok(())
	}

	pub fn is_initialized(&self) -> bool {
		self.config.is_some()
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

impl Drop for Avc3 {
	fn drop(&mut self) {
		tracing::debug!(name = ?self.track.name, "ending track");
		self.catalog.lock().video.renditions.remove(&self.track.name);
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, num_enum::TryFromPrimitive)]
#[repr(u8)]
enum NalType {
	Unspecified = 0,
	NonIdrSlice = 1,
	DataPartitionA = 2,
	DataPartitionB = 3,
	DataPartitionC = 4,
	IdrSlice = 5,
	Sei = 6,
	Sps = 7,
	Pps = 8,
	Aud = 9,
	EndOfSeq = 10,
	EndOfStream = 11,
	Filler = 12,
	SpsExt = 13,
	Prefix = 14,
	SubsetSps = 15,
	DepthParameterSet = 16,
}

#[derive(Default)]
struct Frame {
	chunks: BytesMut,
	contains_idr: bool,
	contains_slice: bool,
	contains_sps: bool,
	contains_pps: bool,
}
