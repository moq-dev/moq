//! Reading Opus packets (RFC 6716 §3) without decoding them.

use crate::src::opus::{opus_packet_get_samples_per_frame, opus_packet_parse};
use crate::Error;

/// Samples per channel in each of `packet`'s frames at `sample_rate`, read from its
/// TOC byte, or `None` for an empty packet or a rate Opus does not code (8, 12, 16,
/// 24, or 48 kHz).
pub fn samples_per_frame(packet: &[u8], sample_rate: u32) -> Option<usize> {
	if ![8_000, 12_000, 16_000, 24_000, 48_000].contains(&sample_rate) {
		return None;
	}
	let toc = packet.first()?;
	// SAFETY: libopus reads only the TOC byte.
	let samples = unsafe { opus_packet_get_samples_per_frame(toc, sample_rate as i32) };
	usize::try_from(samples).ok()
}

/// The coded size in bytes of each of `packet`'s frames, its padding parsed off.
pub fn frame_sizes(packet: &[u8]) -> Result<Vec<usize>, Error> {
	let len = i32::try_from(packet.len()).map_err(|_| Error::InvalidPacket)?;
	// A packet holds at most 48 frames (RFC 6716 §3.2.5), which libopus checks.
	let mut sizes = [0i16; 48];
	// SAFETY: libopus reads at most `len` bytes of `packet` and writes at most 48
	// sizes. Null TOC, frame, and offset pointers ask it to skip those, which it
	// checks for.
	let count = unsafe {
		opus_packet_parse(
			packet.as_ptr(),
			len,
			std::ptr::null_mut(),
			std::ptr::null_mut(),
			sizes.as_mut_ptr(),
			std::ptr::null_mut(),
		)
	};
	let count = Error::check(count)? as usize;
	Ok(sizes[..count].iter().map(|&size| size as usize).collect())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reads_the_toc() {
		// Config 31 (CELT FB 20 ms), one frame of three bytes.
		let packet = [31 << 3, 1, 2, 3];
		assert_eq!(samples_per_frame(&packet, 48_000), Some(960));
		assert_eq!(samples_per_frame(&[], 48_000), None);
		assert_eq!(samples_per_frame(&packet, 44_100), None);
		assert_eq!(frame_sizes(&packet), Ok(vec![3]));
	}

	#[test]
	fn rejects_a_malformed_packet() {
		assert_eq!(frame_sizes(&[]), Err(Error::InvalidPacket));
		// Code 3 with a frame count of 63, above the 48 a packet holds.
		assert_eq!(frame_sizes(&[0x03, 63]), Err(Error::InvalidPacket));
	}
}
