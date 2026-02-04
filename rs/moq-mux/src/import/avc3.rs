use super::annexb::{NalIterator, START_CODE};

use anyhow::Context;
use buf_list::BufList;
use bytes::{Buf, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt};

/// A decoder for H.264 with inline SPS/PPS.
pub struct Avc3 {
	// The broadcast being produced.
	broadcast: moq_lite::BroadcastProducer,

	// The catalog being produced.
	catalog: hang::catalog::CatalogProducer,

	// The track being produced.
	track: Option<hang::container::OrderedProducer>,

	// Whether the track has been initialized.
	// If it changes, then we'll reinitialize with a new track.
	config: Option<hang::catalog::VideoConfig>,

	// The current frame being built.
	current: Frame,

	// Used to compute wall clock timestamps if needed.
	zero: Option<tokio::time::Instant>,
}

impl Avc3 {
	pub fn new(broadcast: moq_lite::BroadcastProducer, catalog: hang::catalog::CatalogProducer) -> Self {
		Self {
			broadcast,
			catalog,
			track: None,
			config: None,
			current: Default::default(),
			zero: None,
		}
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

		let mut catalog = self.catalog.lock();

		if let Some(track) = &self.track.take() {
			tracing::debug!(name = ?track.info.name, "reinitializing track");
			catalog.video.remove_track(&track.info);
		}

		let track = catalog.video.create_track("avc3", config.clone());
		tracing::debug!(name = ?track.name, ?config, "starting track");

		let track = self.broadcast.create_track(track);

		self.config = Some(config);
		self.track = Some(track.into());

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
				let nal = h264_parser::nal::ebsp_to_rbsp(&nal[1..]);
				let sps = h264_parser::Sps::parse(&nal)?;
				self.init(&sps)?;
			}
			// TODO parse the SPS again and reinitialize the track if needed
			Some(NalType::Aud) | Some(NalType::Pps) | Some(NalType::Sei) => {
				self.maybe_start_frame(pts)?;
			}
			Some(NalType::IdrSlice) => {
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

		// Rather than keeping the original size of the start code, we replace it with a 4 byte start code.
		// It's just marginally easier and potentially more efficient down the line (JS player with MSE).
		// NOTE: This is ref-counted and static, so it's extremely cheap to clone.
		self.current.chunks.push_chunk(START_CODE.clone());
		self.current.chunks.push_chunk(nal);

		Ok(())
	}

	fn maybe_start_frame(&mut self, pts: Option<hang::container::Timestamp>) -> anyhow::Result<()> {
		// If we haven't seen any slices, we shouldn't flush yet.
		if !self.current.contains_slice {
			return Ok(());
		}

		let track = self.track.as_mut().context("expected SPS before any frames")?;
		let pts = pts.context("missing timestamp")?;

		let payload = std::mem::take(&mut self.current.chunks);
		let frame = hang::container::Frame {
			timestamp: pts,
			keyframe: self.current.contains_idr,
			payload,
		};

		track.write(frame)?;

		self.current.contains_idr = false;
		self.current.contains_slice = false;

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

impl Drop for Avc3 {
	fn drop(&mut self) {
		if let Some(track) = self.track.take() {
			tracing::debug!(name = ?track.info.name, "ending track");
			self.catalog.lock().video.remove_track(&track.info);
		}
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
	chunks: BufList,
	contains_idr: bool,
	contains_slice: bool,
}
