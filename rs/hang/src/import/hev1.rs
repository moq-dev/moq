use crate as hang;
use anyhow::Context;
use buf_list::BufList;
use bytes::{Buf, Bytes};
use moq_lite as moq;
use scuffle_h265::SpsNALUnit;

// Prepend each NAL with a 4 byte start code.
// Yes, it's one byte longer than the 3 byte start code, but it's easier to convert to MP4.
const START_CODE: Bytes = Bytes::from_static(&[0, 0, 0, 1]);

/// A decoder for H.265 with inline SPS/PPS.
/// Only supports single layer streams, ignores VPS.
pub struct Hev1 {
	// The broadcast being produced.
	// This `hang` variant includes a catalog.
	broadcast: hang::BroadcastProducer,

	// The track being produced.
	track: Option<hang::TrackProducer>,

	// Whether the track has been initialized.
	// If it changes, then we'll reinitialize with a new track.
	config: Option<hang::catalog::VideoConfig>,

	// The current frame being built.
	current: Frame,

	// Used to compute wall clock timestamps if needed.
	zero: Option<tokio::time::Instant>,
}

impl Hev1 {
	pub fn new(broadcast: hang::BroadcastProducer) -> Self {
		Self {
			broadcast,
			track: None,
			config: None,
			current: Default::default(),
			zero: None,
		}
	}

	fn init(&mut self, sps: &SpsNALUnit) -> anyhow::Result<()> {
		let profile = &sps.rbsp.profile_tier_level.general_profile;

		let config = hang::catalog::VideoConfig {
			coded_width: Some(sps.rbsp.cropped_width() as u32),
			coded_height: Some(sps.rbsp.cropped_height() as u32),
			codec: hang::catalog::H265 {
				in_band: true, // We only support `hev1` with inline SPS/PPS for now
				profile_space: profile.profile_space,
				profile_idc: profile.profile_idc,
				profile_compatibility_flags: profile.profile_compatibility_flag.bits().to_be_bytes(),
				tier_flag: profile.tier_flag,
				level_idc: profile.level_idc.context("missing level_idc in SPS")?,
				constraint_flags: pack_constraint_flags(profile),
			}
			.into(),
			description: None,
			// TODO: populate these fields from sps.rbsp.vui_parameters
			framerate: None,
			bitrate: None,
			display_ratio_width: None,
			display_ratio_height: None,
			optimize_for_latency: None,
		};

		if let Some(old) = &self.config {
			if old == &config {
				return Ok(());
			}
		}

		if let Some(track) = &self.track.take() {
			tracing::debug!(name = ?track.info.name, "reinitializing track");
			self.broadcast.catalog.lock().remove_video(&track.info.name);
		}

		let track = moq::Track {
			name: self.broadcast.track_name("video"),
			priority: 2,
		};

		tracing::debug!(name = ?track.name, ?config, "starting track");

		{
			let mut catalog = self.broadcast.catalog.lock();
			let video = catalog.insert_video(track.name.clone(), config.clone());
			video.priority = 2;
		}

		let track = track.produce();
		self.broadcast.insert_track(track.consumer);

		self.config = Some(config);
		self.track = Some(track.producer.into());

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

	/// Decode as much data as possible from the given buffer.
	///
	/// Unlike [Self::decode_frame], this method needs the start code for the next frame.
	/// This means it works for streaming media (ex. stdin) but adds a frame of latency.
	///
	/// TODO: This currently associates PTS with the *previous* frame, as part of `maybe_start_frame`.
	pub fn decode_stream<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: Option<hang::Timestamp>,
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
		pts: Option<hang::Timestamp>,
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

	fn decode_nal(&mut self, nal: Bytes, pts: Option<hang::Timestamp>) -> anyhow::Result<()> {
		anyhow::ensure!(nal.len() >= 2, "NAL unit is too short");
		// u16 header: [forbidden_zero_bit(1) | nal_unit_type(6) | nuh_layer_id(6) | nuh_temporal_id_plus1(3)]
		let header = nal.first().context("NAL unit is too short")?;

		let forbidden_zero_bit = (header >> 7) & 1;
		anyhow::ensure!(forbidden_zero_bit == 0, "forbidden zero bit is not zero");

		// Bits 1-6: nal_unit_type
		let nal_unit_type = (header >> 1) & 0b111111;
		let nal_type = HevcNalType::try_from(nal_unit_type).ok();

		match nal_type {
			Some(HevcNalType::Sps) => {
				self.maybe_start_frame(pts)?;

				// Try to reinitialize the track if the SPS has changed.
				let sps = SpsNALUnit::parse(&mut &nal[..]).context("failed to parse SPS NAL unit")?;
				self.init(&sps)?;
			}
			// TODO parse the SPS again and reinitialize the track if needed
			Some(HevcNalType::Aud | HevcNalType::Pps | HevcNalType::SeiPrefix | HevcNalType::SeiSuffix) => {
				self.maybe_start_frame(pts)?;
			}
			Some(
				HevcNalType::IdrWRadl
				| HevcNalType::IdrNLp
				| HevcNalType::BlaNLp
				| HevcNalType::BlaWRadl
				| HevcNalType::BlaWLp
				| HevcNalType::Cra,
			) => {
				self.current.contains_idr = true;
				self.current.contains_slice = true;
			}
			// All slice types (both N and R variants)
			Some(
				HevcNalType::TrailN
				| HevcNalType::TrailR
				| HevcNalType::TsaN
				| HevcNalType::TsaR
				| HevcNalType::StsaN
				| HevcNalType::StsaR
				| HevcNalType::RadlN
				| HevcNalType::RadlR
				| HevcNalType::RaslN
				| HevcNalType::RaslR,
			) => {
				// Check first_slice_segment_in_pic_flag (bit 7 of third byte, after 2-byte header)
				if nal.get(2).context("NAL unit is too short")? & 0x80 != 0 {
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

	fn maybe_start_frame(&mut self, pts: Option<hang::Timestamp>) -> anyhow::Result<()> {
		// If we haven't seen any slices, we shouldn't flush yet.
		if !self.current.contains_slice {
			return Ok(());
		}

		let track = self.track.as_mut().context("expected SPS before any frames")?;
		let pts = pts.context("missing timestamp")?;

		let payload = std::mem::take(&mut self.current.chunks);
		let frame = hang::Frame {
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

	fn pts(&mut self, hint: Option<hang::Timestamp>) -> anyhow::Result<hang::Timestamp> {
		if let Some(pts) = hint {
			return Ok(pts);
		}

		let zero = self.zero.get_or_insert_with(tokio::time::Instant::now);
		Ok(hang::Timestamp::from_micros(zero.elapsed().as_micros() as u64)?)
	}
}

impl Drop for Hev1 {
	fn drop(&mut self) {
		if let Some(track) = &self.track {
			tracing::debug!(name = ?track.info.name, "ending track");
			self.broadcast.catalog.lock().remove_video(&track.info.name);
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, num_enum::TryFromPrimitive)]
#[repr(u8)]
pub enum HevcNalType {
	TrailN = 0,
	TrailR = 1,
	TsaN = 2,
	TsaR = 3,
	StsaN = 4,
	StsaR = 5,
	RadlN = 6,
	RadlR = 7,
	RaslN = 8,
	RaslR = 9,
	// 10 -> 15 reserved
	BlaWLp = 16,
	BlaWRadl = 17,
	BlaNLp = 18,
	IdrWRadl = 19,
	IdrNLp = 20,
	Cra = 21,
	// 22 -> 31 reserved
	Vps = 32,
	Sps = 33,
	Pps = 34,
	Aud = 35,
	EndOfSequence = 36,
	EndOfBitstream = 37,
	Filler = 38,
	SeiPrefix = 39,
	SeiSuffix = 40,
} // ITU H.265 V10 Table 7-1 – NAL unit type codes and NAL unit type classes

struct NalIterator<'a, T: Buf + AsRef<[u8]> + 'a> {
	buf: &'a mut T,
	start: Option<usize>,
}

impl<'a, T: Buf + AsRef<[u8]> + 'a> NalIterator<'a, T> {
	pub fn new(buf: &'a mut T) -> Self {
		Self { buf, start: None }
	}

	/// Assume the buffer ends with a NAL unit and flush it.
	/// This is more efficient because we cache the last "start" code position.
	pub fn flush(self) -> anyhow::Result<Option<Bytes>> {
		let start = match self.start {
			Some(start) => start,
			None => match after_start_code(self.buf.as_ref())? {
				Some(start) => start,
				None => return Ok(None),
			},
		};

		self.buf.advance(start);

		let nal = self.buf.copy_to_bytes(self.buf.remaining());
		Ok(Some(nal))
	}
}

impl<'a, T: Buf + AsRef<[u8]> + 'a> Iterator for NalIterator<'a, T> {
	type Item = anyhow::Result<Bytes>;

	fn next(&mut self) -> Option<Self::Item> {
		let start = match self.start {
			Some(start) => start,
			None => match after_start_code(self.buf.as_ref()).transpose()? {
				Ok(start) => start,
				Err(err) => return Some(Err(err)),
			},
		};

		let (size, new_start) = find_start_code(&self.buf.as_ref()[start..])?;
		self.buf.advance(start);

		let nal = self.buf.copy_to_bytes(size);
		self.start = Some(new_start);
		Some(Ok(nal))
	}
}

// Return the size of the start code at the start of the buffer.
fn after_start_code(b: &[u8]) -> anyhow::Result<Option<usize>> {
	if b.len() < 3 {
		return Ok(None);
	}

	// NOTE: We have to check every byte, so the `find_start_code` optimization doesn't matter.
	anyhow::ensure!(b[0] == 0, "missing Annex B start code");
	anyhow::ensure!(b[1] == 0, "missing Annex B start code");

	match b[2] {
		0 if b.len() < 4 => Ok(None),
		0 if b[3] != 1 => anyhow::bail!("missing Annex B start code"),
		0 => Ok(Some(4)),
		1 => Ok(Some(3)),
		_ => anyhow::bail!("invalid Annex B start code"),
	}
}

// Return the number of bytes until the next start code, and the size of that start code.
fn find_start_code(mut b: &[u8]) -> Option<(usize, usize)> {
	// Okay this is over-engineered because this was my interview question.
	// We need to find either a 3 byte or 4 byte start code.
	// 3-byte: 0 0 1
	// 4-byte: 0 0 0 1
	//
	// You fail the interview if you call string.split twice or something.
	// You get a pass if you do index += 1 and check the next 3-4 bytes.
	// You get my eternal respect if you check the 3rd byte first.
	// What?
	//
	// If we check the 3rd byte and it's not a 0 or 1, then we immediately index += 3
	// Sometimes we might only skip 1 or 2 bytes, but it's still better than checking every byte.
	//
	// TODO Is this the type of thing that SIMD could further improve?
	// If somebody can figure that out, I'll buy you a beer.
	let size = b.len();

	while b.len() >= 3 {
		// ? ? ?
		match b[2] {
			// ? ? 0
			0 if b.len() >= 4 => match b[3] {
				// ? ? 0 1
				1 => match b[1] {
					// ? 0 0 1
					0 => match b[0] {
						// 0 0 0 1
						0 => return Some((size - b.len(), 4)),
						// ? 0 0 1
						_ => return Some((size - b.len() + 1, 3)),
					},
					// ? x 0 1
					_ => b = &b[4..],
				},
				// ? ? 0 0 - skip only 1 byte to check for potential 0 0 0 1
				0 => b = &b[1..],
				// ? ? 0 x
				_ => b = &b[4..],
			},
			// ? ? 0 FIN
			0 => return None,
			// ? ? 1
			1 => match b[1] {
				// ? 0 1
				0 => match b[0] {
					// 0 0 1
					0 => return Some((size - b.len(), 3)),
					// ? 0 1
					_ => b = &b[3..],
				},
				// ? x 1
				_ => b = &b[3..],
			},
			// ? ? x
			_ => b = &b[3..],
		}
	}

	None
}

// Packs the constraint flags from ITU H.265 V10 Section 7.3.3 Profile, tier and level syntax
fn pack_constraint_flags(profile: &scuffle_h265::Profile) -> [u8; 6] {
	let mut flags = [0u8; 6];
	flags[0] = ((profile.progressive_source_flag as u8) << 7)
		| ((profile.interlaced_source_flag as u8) << 6)
		| ((profile.non_packed_constraint_flag as u8) << 5)
		| ((profile.frame_only_constraint_flag as u8) << 4);

	// @todo: pack the rest of the optional flags in profile.additional_flags
	return flags;
}

#[derive(Default)]
struct Frame {
	chunks: BufList,
	contains_idr: bool,
	contains_slice: bool,
}
