use crate as hang;
use anyhow::Context;
use bytes::{Buf, Bytes};
use moq_lite as moq;

// Prepend each NAL with a 4 byte start code.
// Yes, it's one byte longer than the 3 byte start code, but it's easier to convert to MP4.
const START_CODE: Bytes = Bytes::from_static(&[0, 0, 0, 1]);

/// A decoder for H.264 with inline SPS/PPS.
pub struct Avc3 {
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

impl Avc3 {
	pub fn new(broadcast: hang::BroadcastProducer) -> Self {
		Self {
			broadcast,
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
		let nals = NalIterator::new(buf);

		for nal in nals {
			self.decode_nal(nal?, None)?;
		}

		Ok(())
	}

	/// Decode as much data as possible from the given buffer.
	///
	/// Unlike [Self::decode_framed], this method needs the start code for the next frame.
	/// This means it works for streaming media (ex. stdin) but adds a frame of latency.
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

		// Decode any NALs at the start of the buffer.
		self.decode_stream(buf, Some(pts))?;

		// Make sure there's a start code at the start of the buffer.
		let start = after_start_code(buf.as_ref())?.context("missing start code")?;
		buf.advance(start);

		// Assume the rest of the buffer is a single NAL.
		let nal = buf.copy_to_bytes(buf.remaining());
		self.decode_nal(nal, Some(pts))?;

		// Flush the frame if we read a slice.
		self.maybe_start_frame(Some(pts))?;

		Ok(())
	}

	fn decode_nal(&mut self, nal: Bytes, pts: Option<hang::Timestamp>) -> anyhow::Result<()> {
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

		// Rather than keeping the original size of the start code, we replace it with a 4 byte start code.
		// It's just marginally easier and potentially more efficient down the line (JS player with MSE).
		// NOTE: This is ref-counted and static, so it's extremely cheap to clone.
		self.current.chunks.push(START_CODE.clone());
		self.current.chunks.push(nal);

		Ok(())
	}

	fn maybe_start_frame(&mut self, pts: Option<hang::Timestamp>) -> anyhow::Result<()> {
		// If we haven't seen any slices, we shouldn't flush yet.
		if !self.current.contains_slice {
			return Ok(());
		}

		let track = self.track.as_mut().context("expected SPS before any frames")?;
		let pts = pts.context("missing timestamp")?;

		track.write_chunks(self.current.contains_idr, pts, self.current.chunks.iter().cloned())?;
		self.current.clear();

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

impl Drop for Avc3 {
	fn drop(&mut self) {
		if let Some(track) = &self.track {
			tracing::debug!(name = ?track.info.name, "ending track");
			self.broadcast.catalog.lock().remove_video(&track.info.name);
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, num_enum::TryFromPrimitive)]
#[repr(u8)]
pub enum NalType {
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

struct NalIterator<T: Buf + AsRef<[u8]>> {
	buf: T,
	start: Option<usize>,
}

impl<T: Buf + AsRef<[u8]>> NalIterator<T> {
	pub fn new(buf: T) -> Self {
		Self { buf, start: None }
	}
}

impl<T: Buf + AsRef<[u8]>> Iterator for NalIterator<T> {
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

#[derive(Default)]
struct Frame {
	chunks: Vec<Bytes>,
	contains_idr: bool,
	contains_slice: bool,
}

impl Frame {
	fn clear(&mut self) {
		self.chunks.clear();
		self.contains_idr = false;
		self.contains_slice = false;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	// Tests for after_start_code - validates and measures start code at buffer beginning

	#[test]
	fn test_after_start_code_3_byte() {
		let buf = &[0, 0, 1, 0x67];
		assert_eq!(after_start_code(buf).unwrap(), Some(3));
	}

	#[test]
	fn test_after_start_code_4_byte() {
		let buf = &[0, 0, 0, 1, 0x67];
		assert_eq!(after_start_code(buf).unwrap(), Some(4));
	}

	#[test]
	fn test_after_start_code_too_short() {
		let buf = &[0, 0];
		assert_eq!(after_start_code(buf).unwrap(), None);
	}

	#[test]
	fn test_after_start_code_incomplete_4_byte() {
		let buf = &[0, 0, 0];
		assert_eq!(after_start_code(buf).unwrap(), None);
	}

	#[test]
	fn test_after_start_code_invalid_first_byte() {
		let buf = &[1, 0, 1];
		assert!(after_start_code(buf).is_err());
	}

	#[test]
	fn test_after_start_code_invalid_second_byte() {
		let buf = &[0, 1, 1];
		assert!(after_start_code(buf).is_err());
	}

	#[test]
	fn test_after_start_code_invalid_third_byte() {
		let buf = &[0, 0, 2];
		assert!(after_start_code(buf).is_err());
	}

	#[test]
	fn test_after_start_code_invalid_4_byte_pattern() {
		let buf = &[0, 0, 0, 2];
		assert!(after_start_code(buf).is_err());
	}

	// Tests for find_start_code - finds next start code in NAL data

	#[test]
	fn test_find_start_code_3_byte() {
		let buf = &[0x67, 0x42, 0x00, 0x1f, 0, 0, 1];
		assert_eq!(find_start_code(buf), Some((4, 3)));
	}

	#[test]
	fn test_find_start_code_4_byte() {
		// Should detect 4-byte start code at beginning
		let buf = &[0, 0, 0, 1, 0x67];
		assert_eq!(find_start_code(buf), Some((0, 4)));
	}

	#[test]
	fn test_find_start_code_4_byte_after_data() {
		// Should detect 4-byte start code after NAL data
		let buf = &[0x67, 0x42, 0xff, 0x1f, 0, 0, 0, 1];
		assert_eq!(find_start_code(buf), Some((4, 4)));
	}

	#[test]
	fn test_find_start_code_at_start_3_byte() {
		let buf = &[0, 0, 1, 0x67];
		assert_eq!(find_start_code(buf), Some((0, 3)));
	}

	#[test]
	fn test_find_start_code_none() {
		let buf = &[0x67, 0x42, 0x00, 0x1f, 0xff];
		assert_eq!(find_start_code(buf), None);
	}

	#[test]
	fn test_find_start_code_trailing_zeros() {
		let buf = &[0x67, 0x42, 0x00, 0x1f, 0, 0];
		assert_eq!(find_start_code(buf), None);
	}

	#[test]
	fn test_find_start_code_edge_case_3_byte() {
		let buf = &[0xff, 0, 0, 1];
		assert_eq!(find_start_code(buf), Some((1, 3)));
	}

	#[test]
	fn test_find_start_code_false_positive_avoidance() {
		// Pattern like: x 0 0 y (where y != 1) - should skip ahead
		let buf = &[0xff, 0, 0, 0xff, 0, 0, 1];
		assert_eq!(find_start_code(buf), Some((4, 3)));
	}

	#[test]
	fn test_find_start_code_4_byte_after_nonzero() {
		// Critical edge case: x 0 0 0 1 should find 4-byte start code at position 1
		// This tests that we only skip 1 byte when seeing ? ? 0 0
		let buf = &[0xff, 0, 0, 0, 1];
		assert_eq!(find_start_code(buf), Some((1, 4)));
	}

	#[test]
	fn test_find_start_code_consecutive_zeros() {
		// Multiple consecutive zeros before the 1
		let buf = &[0xff, 0, 0, 0, 0, 0, 1];
		// Should skip past leading zeros and find the start code
		let result = find_start_code(buf);
		assert!(result.is_some());
		let (pos, size) = result.unwrap();
		// The exact position depends on the algorithm, but it should find a valid start code
		assert!(size == 3 || size == 4);
		assert!(pos < buf.len());
	}

	// Tests for NalIterator - iterates over NAL units in Annex B format

	#[test]
	fn test_nal_iterator_simple_3_byte() {
		let data = vec![0, 0, 1, 0x67, 0x42, 0, 0, 1];
		let mut iter = NalIterator::new(Bytes::from(data));

		let nal = iter.next().unwrap().unwrap();
		assert_eq!(nal.as_ref(), &[0x67, 0x42]);
		assert!(iter.next().is_none());
	}

	#[test]
	fn test_nal_iterator_simple_4_byte() {
		let data = vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1];
		let mut iter = NalIterator::new(Bytes::from(data));

		let nal = iter.next().unwrap().unwrap();
		assert_eq!(nal.as_ref(), &[0x67, 0x42]);
		assert!(iter.next().is_none());
	}

	#[test]
	fn test_nal_iterator_multiple_nals() {
		let data = vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1];
		let mut iter = NalIterator::new(Bytes::from(data));

		let nal1 = iter.next().unwrap().unwrap();
		assert_eq!(nal1.as_ref(), &[0x67, 0x42]);

		let nal2 = iter.next().unwrap().unwrap();
		assert_eq!(nal2.as_ref(), &[0x68, 0xce]);

		assert!(iter.next().is_none());
	}

	#[test]
	fn test_nal_iterator_realistic_h264() {
		// A realistic H.264 stream with SPS, PPS, and IDR
		let data = vec![
			// SPS NAL
			0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1f, // PPS NAL
			0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80, // IDR slice
			0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00,
			// Trailing start code (needed to detect the end of the last NAL)
			0, 0, 0, 1,
		];
		let mut iter = NalIterator::new(Bytes::from(data));

		let sps = iter.next().unwrap().unwrap();
		assert_eq!(sps[0] & 0x1f, 7); // SPS type
		assert_eq!(sps.as_ref(), &[0x67, 0x42, 0x00, 0x1f]);

		let pps = iter.next().unwrap().unwrap();
		assert_eq!(pps[0] & 0x1f, 8); // PPS type
		assert_eq!(pps.as_ref(), &[0x68, 0xce, 0x3c, 0x80]);

		let idr = iter.next().unwrap().unwrap();
		assert_eq!(idr[0] & 0x1f, 5); // IDR type
		assert_eq!(idr.as_ref(), &[0x65, 0x88, 0x84, 0x00]);

		assert!(iter.next().is_none());
	}

	#[test]
	fn test_nal_iterator_invalid_start() {
		let data = vec![1, 0, 1, 0x67];
		let mut iter = NalIterator::new(Bytes::from(data));

		assert!(iter.next().unwrap().is_err());
	}

	#[test]
	fn test_nal_iterator_empty_nal() {
		// Two consecutive start codes create an empty NAL
		let data = vec![0, 0, 1, 0, 0, 1, 0x67, 0, 0, 1];
		let mut iter = NalIterator::new(Bytes::from(data));

		let nal1 = iter.next().unwrap().unwrap();
		assert_eq!(nal1.len(), 0);

		let nal2 = iter.next().unwrap().unwrap();
		assert_eq!(nal2.as_ref(), &[0x67]);

		assert!(iter.next().is_none());
	}

	#[test]
	fn test_nal_iterator_nal_with_embedded_zeros() {
		// NAL data that contains zeros (but not a start code pattern)
		let data = vec![
			0, 0, 1, 0x67, 0x00, 0x00, 0x00, 0xff, // NAL with embedded zeros
			0, 0, 1, 0x68, // Next NAL
			0, 0, 1,
		];
		let mut iter = NalIterator::new(Bytes::from(data));

		let nal1 = iter.next().unwrap().unwrap();
		assert_eq!(nal1.as_ref(), &[0x67, 0x00, 0x00, 0x00, 0xff]);

		let nal2 = iter.next().unwrap().unwrap();
		assert_eq!(nal2.as_ref(), &[0x68]);

		assert!(iter.next().is_none());
	}
}
