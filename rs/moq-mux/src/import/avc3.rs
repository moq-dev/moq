use super::annexb::NalIterator;
use super::jitter::MinFrameDuration;
use super::same_codec;

use anyhow::Context;
use bytes::{Buf, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt};

/// A decoder for H.264 with inline SPS/PPS.
pub struct Avc3 {
	// The broadcast we publish on. Retained so we can mint a fresh track on
	// codec changes (a mid-stream resolution/profile flip would otherwise
	// mix incompatible samples into the same track).
	broadcast: moq_net::BroadcastProducer,

	// The catalog being produced.
	catalog: crate::catalog::Producer,

	// The track currently carrying frames.
	//
	// Created eagerly in `new()` so callers can monitor `used()`/`unused()`
	// before any frames arrive. The catalog rendition is added in `init()`
	// once the codec config is known from the first SPS, and the track is
	// replaced in `init()` if the codec config later changes.
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
	pub fn new(mut broadcast: moq_net::BroadcastProducer, catalog: crate::catalog::Producer) -> Self {
		// Create the track eagerly so callers can monitor used/unused before any frames arrive.
		// The catalog entry is added later in init() once the codec config is known.
		let track = broadcast.unique_track(".avc3").expect("failed to create avc3 track");

		Self {
			broadcast,
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
	pub fn track(&self) -> &moq_net::TrackProducer {
		&self.track.track
	}

	fn init(&mut self, sps: &h264_parser::Sps) -> anyhow::Result<()> {
		let constraint_flags: u8 = ((sps.constraint_set0_flag as u8) << 7)
			| ((sps.constraint_set1_flag as u8) << 6)
			| ((sps.constraint_set2_flag as u8) << 5)
			| ((sps.constraint_set3_flag as u8) << 4)
			| ((sps.constraint_set4_flag as u8) << 3)
			| ((sps.constraint_set5_flag as u8) << 2);

		// avcC is emitted as soon as both SPS and PPS NALs have been observed,
		// giving downstream consumers (e.g. MKV/CMAF muxers) an out-of-band
		// AVCDecoderConfigurationRecord without having to scrape it from the
		// keyframe themselves.
		let description = match (&self.cached_sps, &self.cached_pps) {
			(Some(sps_nal), Some(pps_nal)) => Some(build_avcc(sps_nal, pps_nal)?),
			_ => None,
		};

		let config = hang::catalog::VideoConfig {
			coded_width: Some(sps.width),
			coded_height: Some(sps.height),
			codec: hang::catalog::H264 {
				profile: sps.profile_idc,
				constraints: constraint_flags,
				level: sps.level_idc,
				// We now strip inline SPS/PPS from samples and ship them via
				// `description` (avcC) instead — that's the avc1 contract.
				inline: false,
			}
			.into(),
			description,
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

		// Codec-bearing fields determine track identity. A description-only
		// update (e.g. cached_pps just arrived) keeps the existing track so
		// downstream subscribers don't have to re-fetch on every catalog tick.
		// The first SPS (None → Some) also reuses the eagerly-created track.
		let needs_retrack = self.config.as_ref().is_some_and(|old| !same_codec(old, &config));

		// Mint the replacement track BEFORE touching the catalog. If
		// unique_track fails we leave self.track and the catalog untouched —
		// no orphaned rendition, no lost track.
		let new_producer = if needs_retrack {
			Some(crate::container::Producer::new(
				self.broadcast.unique_track(".avc3")?,
				crate::container::Hang::Legacy,
			))
		} else {
			None
		};

		let mut catalog = self.catalog.lock();

		if let Some(new) = new_producer {
			let old_name = self.track.name.clone();
			tracing::debug!(?old_name, new_name = ?new.name, "codec changed; replacing track");
			catalog.video.renditions.remove(&old_name);
			self.track = new;
		}

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

		// SPS/PPS are stored in the catalog `description` (avcC) and stripped
		// from sample data — that's the avc1 contract. The rest of the NALs
		// are emitted length-prefixed (4-byte big-endian, matching the
		// `lengthSizeMinusOne = 3` we write in build_avcc).
		let emit = match nal_type {
			Some(NalType::Sps) => {
				self.maybe_start_frame(pts)?;

				let rbsp = h264_parser::nal::ebsp_to_rbsp(&nal[1..]);
				let sps = h264_parser::Sps::parse(&rbsp)?;

				// PPS is tied to SPS context; drop cached PPS when SPS changes.
				if self.cached_sps.as_ref().is_some_and(|cached| cached != &nal) {
					self.cached_pps = None;
				}

				// Cache before init() so the avcC builder can see the latest SPS.
				self.cached_sps = Some(nal.clone());

				self.init(&sps)?;
				false
			}
			Some(NalType::Pps) => {
				self.maybe_start_frame(pts)?;

				self.cached_pps = Some(nal.clone());

				// First PPS after an SPS unlocks avcC emission — republish the
				// catalog with a populated description.
				if let Some(sps_nal) = self.cached_sps.clone() {
					let rbsp = h264_parser::nal::ebsp_to_rbsp(&sps_nal[1..]);
					if let Ok(sps) = h264_parser::Sps::parse(&rbsp) {
						self.init(&sps)?;
					}
				}
				false
			}
			Some(NalType::Aud) | Some(NalType::Sei) => {
				self.maybe_start_frame(pts)?;
				true
			}
			Some(NalType::IdrSlice) => {
				self.current.contains_idr = true;
				self.current.contains_slice = true;
				true
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
				true
			}
			_ => true,
		};

		tracing::trace!(kind = ?nal_type, ?emit, "parsed NAL");

		if emit {
			let len = u32::try_from(nal.len()).context("NAL too large for 4-byte length prefix")?;
			self.current.chunks.extend_from_slice(&len.to_be_bytes());
			self.current.chunks.extend_from_slice(&nal);
		}

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
}

/// Build an AVCDecoderConfigurationRecord (ISO/IEC 14496-15 §5.3.3.1.2) from a
/// single SPS and PPS NAL. The high-profile extension fields are intentionally
/// omitted — players that need them re-derive them from the SPS we ship inline.
///
/// Errors if either NAL is too large to fit avcC's 16-bit length fields.
fn build_avcc(sps_nal: &[u8], pps_nal: &[u8]) -> anyhow::Result<Bytes> {
	use bytes::BufMut;

	anyhow::ensure!(
		sps_nal.len() <= u16::MAX as usize,
		"SPS too large for avcC length field ({} > {})",
		sps_nal.len(),
		u16::MAX
	);
	anyhow::ensure!(
		pps_nal.len() <= u16::MAX as usize,
		"PPS too large for avcC length field ({} > {})",
		pps_nal.len(),
		u16::MAX
	);

	// SPS NAL: byte 0 is the NAL header; bytes 1..4 are profile_idc,
	// constraint flags, and level_idc respectively.
	let profile_idc = sps_nal.get(1).copied().unwrap_or(0);
	let constraints = sps_nal.get(2).copied().unwrap_or(0);
	let level_idc = sps_nal.get(3).copied().unwrap_or(0);

	let mut out = BytesMut::with_capacity(11 + sps_nal.len() + pps_nal.len());
	out.put_u8(1); // configurationVersion
	out.put_u8(profile_idc); // AVCProfileIndication
	out.put_u8(constraints); // profile_compatibility
	out.put_u8(level_idc); // AVCLevelIndication
	out.put_u8(0xff); // reserved (6 bits) | lengthSizeMinusOne (2 bits, = 3)
	out.put_u8(0xe1); // reserved (3 bits) | numOfSequenceParameterSets (5 bits, = 1)
	out.put_u16(sps_nal.len() as u16);
	out.put_slice(sps_nal);
	out.put_u8(1); // numOfPictureParameterSets
	out.put_u16(pps_nal.len() as u16);
	out.put_slice(pps_nal);
	Ok(out.freeze())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn avcc_layout_matches_iso_14496_15() {
		// SPS NAL header byte (0x67 = nal_unit_type 7) followed by profile_idc,
		// constraint flags, level_idc, then trailing rbsp bytes. The avcC
		// builder only reads the first four bytes; the rest is just copied.
		let sps = [0x67, 0x42, 0xc0, 0x1f, 0xde, 0xad];
		let pps = [0x68, 0xce, 0x3c, 0x80];
		let avcc = build_avcc(&sps, &pps).expect("avcC build");

		// Manually reconstruct the expected layout.
		let mut expected = vec![
			1, // configurationVersion
			0x42, // AVCProfileIndication
			0xc0, // profile_compatibility (matches sps[2])
			0x1f, // AVCLevelIndication
			0xff, // reserved | lengthSizeMinusOne=3
			0xe1, // reserved | numOfSequenceParameterSets=1
			0, sps.len() as u8,
		];
		expected.extend_from_slice(&sps);
		expected.push(1); // numOfPictureParameterSets
		expected.extend_from_slice(&[0, pps.len() as u8]);
		expected.extend_from_slice(&pps);

		assert_eq!(avcc.as_ref(), expected.as_slice());
	}

	#[test]
	fn avcc_errors_on_oversized_sps() {
		let sps = vec![0u8; u16::MAX as usize + 1];
		let pps = vec![0x68, 0xce, 0x3c, 0x80];
		let err = build_avcc(&sps, &pps).expect_err("oversized SPS should error");
		assert!(err.to_string().contains("SPS too large"), "got: {err}");
	}

	#[test]
	fn avcc_errors_on_oversized_pps() {
		let sps = vec![0x67, 0x42, 0xc0, 0x1f];
		let pps = vec![0u8; u16::MAX as usize + 1];
		let err = build_avcc(&sps, &pps).expect_err("oversized PPS should error");
		assert!(err.to_string().contains("PPS too large"), "got: {err}");
	}

	#[test]
	fn avcc_accepts_max_sized_nal() {
		let sps = vec![0x67, 0x42, 0xc0, 0x1f]
			.into_iter()
			.chain(std::iter::repeat_n(0u8, u16::MAX as usize - 4))
			.collect::<Vec<_>>();
		let pps = vec![0x68, 0xce, 0x3c, 0x80];
		assert!(build_avcc(&sps, &pps).is_ok());
	}
}
