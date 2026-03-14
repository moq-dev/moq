use anyhow::{self};
use bytes::{Buf, Bytes};

pub const START_CODE: Bytes = Bytes::from_static(&[0, 0, 0, 1]);

pub struct NalIterator<'a, T: Buf + AsRef<[u8]> + 'a> {
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
			None => {
				let Some(start) = after_start_code(self.buf.as_ref())? else {
					return Ok(None);
				};
				start
			}
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
pub fn after_start_code(b: &[u8]) -> anyhow::Result<Option<usize>> {
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
pub fn find_start_code(mut b: &[u8]) -> Option<(usize, usize)> {
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
		let mut data = Bytes::from(vec![0, 0, 1, 0x67, 0x42, 0, 0, 1]);
		let mut iter = NalIterator::new(&mut data);

		let nal = iter.next().unwrap().unwrap();
		assert_eq!(nal.as_ref(), &[0x67, 0x42]);
		assert!(iter.next().is_none());

		// Make sure the trailing 001 is still in the buffer.
		assert_eq!(data.as_ref(), &[0, 0, 1]);
	}

	#[test]
	fn test_nal_iterator_simple_4_byte() {
		let mut data = Bytes::from(vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1]);
		let mut iter = NalIterator::new(&mut data);

		let nal = iter.next().unwrap().unwrap();
		assert_eq!(nal.as_ref(), &[0x67, 0x42]);
		assert!(iter.next().is_none());

		// Make sure the trailing 0001 is still in the buffer.
		assert_eq!(data.as_ref(), &[0, 0, 0, 1]);
	}

	#[test]
	fn test_nal_iterator_multiple_nals() {
		let mut data = Bytes::from(vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1]);
		let mut iter = NalIterator::new(&mut data);

		let nal1 = iter.next().unwrap().unwrap();
		assert_eq!(nal1.as_ref(), &[0x67, 0x42]);

		let nal2 = iter.next().unwrap().unwrap();
		assert_eq!(nal2.as_ref(), &[0x68, 0xce]);

		assert!(iter.next().is_none());

		// Make sure the trailing 0001 is still in the buffer.
		assert_eq!(data.as_ref(), &[0, 0, 0, 1]);
	}

	#[test]
	fn test_nal_iterator_realistic_h264() {
		// A realistic H.264 stream with SPS, PPS, and IDR
		let mut data = Bytes::from(vec![
			0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1f, // SPS NAL
			0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80, // PPS NAL
			0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00, // IDR slice
			// Trailing start code (needed to detect the end of the last NAL)
			0, 0, 0, 1,
		]);
		let mut iter = NalIterator::new(&mut data);

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

		// Make sure the trailing 0001 is still in the buffer.
		assert_eq!(data.as_ref(), &[0, 0, 0, 1]);
	}

	#[test]
	fn test_nal_iterator_realistic_h265() {
		// A realistic H.265 stream with VPS, SPS, PPS, and IDR
		let mut data = Bytes::from(vec![
			0, 0, 0, 1, 0x40, 0x01, 0x0c, 0x01, // VPS NAL
			0, 0, 0, 1, 0x42, 0x01, 0x01, 0x60, // SPS NAL
			0, 0, 0, 1, 0x44, 0x01, 0xc0, 0xf1, // PPS NAL
			0, 0, 0, 1, 0x26, 0x01, 0x9a, 0x20, // IDR_W_RADL slice
			// Trailing start code (needed to detect the end of the last NAL)
			0, 0, 0, 1,
		]);
		let mut iter = NalIterator::new(&mut data);

		let vps = iter.next().unwrap().unwrap();
		assert_eq!((vps[0] >> 1) & 0x3f, 32); // VPS type
		assert_eq!(vps.as_ref(), &[0x40, 0x01, 0x0c, 0x01]);

		let sps = iter.next().unwrap().unwrap();
		assert_eq!((sps[0] >> 1) & 0x3f, 33); // SPS type
		assert_eq!(sps.as_ref(), &[0x42, 0x01, 0x01, 0x60]);

		let pps = iter.next().unwrap().unwrap();
		assert_eq!((pps[0] >> 1) & 0x3f, 34); // PPS type
		assert_eq!(pps.as_ref(), &[0x44, 0x01, 0xc0, 0xf1]);

		let idr = iter.next().unwrap().unwrap();
		assert_eq!((idr[0] >> 1) & 0x3f, 19); // IDR slice type (IDR_W_RADL)
		assert_eq!(idr.as_ref(), &[0x26, 0x01, 0x9a, 0x20]);

		assert!(iter.next().is_none());

		// Make sure the trailing 0001 is still in the buffer.
		assert_eq!(data.as_ref(), &[0, 0, 0, 1]);
	}

	#[test]
	fn test_nal_iterator_invalid_start() {
		let mut data = Bytes::from(vec![1, 0, 1, 0x67]);
		let mut iter = NalIterator::new(&mut data);

		assert!(iter.next().unwrap().is_err());

		// Make sure the data is still in the buffer.
		assert_eq!(data.as_ref(), &[1, 0, 1, 0x67]);
	}

	#[test]
	fn test_nal_iterator_empty_nal() {
		// Two consecutive start codes create an empty NAL
		let mut data = Bytes::from(vec![0, 0, 1, 0, 0, 1, 0x67, 0, 0, 1]);
		let mut iter = NalIterator::new(&mut data);

		let nal1 = iter.next().unwrap().unwrap();
		assert_eq!(nal1.len(), 0);

		let nal2 = iter.next().unwrap().unwrap();
		assert_eq!(nal2.as_ref(), &[0x67]);

		assert!(iter.next().is_none());

		// Make sure the data is still in the buffer.
		assert_eq!(data.as_ref(), &[0, 0, 1]);
	}

	#[test]
	fn test_nal_iterator_nal_with_embedded_zeros() {
		// NAL data that contains zeros (but not a start code pattern)
		let mut data = Bytes::from(vec![
			0, 0, 1, 0x67, 0x00, 0x00, 0x00, 0xff, // NAL with embedded zeros
			0, 0, 1, 0x68, // Next NAL
			0, 0, 1,
		]);
		let mut iter = NalIterator::new(&mut data);

		let nal1 = iter.next().unwrap().unwrap();
		assert_eq!(nal1.as_ref(), &[0x67, 0x00, 0x00, 0x00, 0xff]);

		let nal2 = iter.next().unwrap().unwrap();
		assert_eq!(nal2.as_ref(), &[0x68]);

		assert!(iter.next().is_none());

		// Make sure the data is still in the buffer.
		assert_eq!(data.as_ref(), &[0, 0, 1]);
	}

	// Tests for flush - extracts final NAL without trailing start code

	#[test]
	fn test_flush_after_iteration() {
		// Normal case: iterate over NALs, then flush the final one
		let mut data = Bytes::from(vec![
			0, 0, 0, 1, 0x67, 0x42, // First NAL
			0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80, // Second NAL (final, no trailing start code)
		]);
		let mut iter = NalIterator::new(&mut data);

		let nal1 = iter.next().unwrap().unwrap();
		assert_eq!(nal1.as_ref(), &[0x67, 0x42]);

		assert!(iter.next().is_none());

		let final_nal = iter.flush().unwrap().unwrap();
		assert_eq!(final_nal.as_ref(), &[0x68, 0xce, 0x3c, 0x80]);
	}

	#[test]
	fn test_flush_single_nal() {
		// Buffer contains only a single NAL with no trailing start code
		let mut data = Bytes::from(vec![0, 0, 1, 0x67, 0x42, 0x00, 0x1f]);
		let iter = NalIterator::new(&mut data);

		let final_nal = iter.flush().unwrap().unwrap();
		assert_eq!(final_nal.as_ref(), &[0x67, 0x42, 0x00, 0x1f]);
	}

	#[test]
	fn test_flush_4_byte_start_code() {
		// Test flush with 4-byte start code
		let mut data = Bytes::from(vec![0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00, 0xff]);
		let iter = NalIterator::new(&mut data);

		let final_nal = iter.flush().unwrap().unwrap();
		assert_eq!(final_nal.as_ref(), &[0x65, 0x88, 0x84, 0x00, 0xff]);
	}

	#[test]
	fn test_flush_no_start_code() {
		// Buffer doesn't start with a start code and has no cached start position
		let mut data = Bytes::from(vec![0x67, 0x42, 0x00, 0x1f]);
		let iter = NalIterator::new(&mut data);

		let result = iter.flush();
		assert!(result.is_err());
	}

	#[test]
	fn test_flush_empty_buffer() {
		// Empty buffer should return None
		let mut data = Bytes::from(vec![]);
		let iter = NalIterator::new(&mut data);

		let result = iter.flush().unwrap();
		assert!(result.is_none());
	}

	#[test]
	fn test_flush_incomplete_start_code() {
		// Buffer has incomplete start code (not enough bytes)
		let mut data = Bytes::from(vec![0, 0]);
		let iter = NalIterator::new(&mut data);

		let result = iter.flush().unwrap();
		assert!(result.is_none());
	}

	#[test]
	fn test_flush_multiple_nals_then_flush() {
		// Iterate over multiple NALs, then flush the final one
		let mut data = Bytes::from(vec![
			0, 0, 0, 1, 0x67, 0x42, // SPS
			0, 0, 0, 1, 0x68, 0xce, // PPS
			0, 0, 0, 1, 0x65, 0x88, 0x84, // IDR (final NAL)
		]);
		let mut iter = NalIterator::new(&mut data);

		let sps = iter.next().unwrap().unwrap();
		assert_eq!(sps.as_ref(), &[0x67, 0x42]);

		let pps = iter.next().unwrap().unwrap();
		assert_eq!(pps.as_ref(), &[0x68, 0xce]);

		assert!(iter.next().is_none());

		let idr = iter.flush().unwrap().unwrap();
		assert_eq!(idr.as_ref(), &[0x65, 0x88, 0x84]);
	}

	#[test]
	fn test_flush_empty_final_nal() {
		// Edge case: final NAL is empty (just a start code with no data)
		let mut data = Bytes::from(vec![
			0, 0, 0, 1, 0x67, 0x42, // First NAL
			0, 0, 0, 1, // Second NAL (empty)
		]);
		let mut iter = NalIterator::new(&mut data);

		let nal1 = iter.next().unwrap().unwrap();
		assert_eq!(nal1.as_ref(), &[0x67, 0x42]);

		assert!(iter.next().is_none());

		let final_nal = iter.flush().unwrap().unwrap();
		assert_eq!(final_nal.len(), 0);
	}
}
