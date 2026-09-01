//! libopus constraints, shared by the encoder and decoder.
//!
//! Both sides have to agree on which rates, channel counts, and frame durations
//! libopus accepts, so the checks live here rather than being duplicated (and
//! drifting) across `encode` and `decode`.

use std::time::Duration;

use crate::{Activity, Error};

/// Sample rates libopus runs at, ascending.
const RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];

/// Frame durations libopus accepts, in microseconds.
const FRAME_DURATIONS: [u128; 6] = [2_500, 5_000, 10_000, 20_000, 40_000, 60_000];

/// Snap an arbitrary sample rate up to the nearest libopus-supported rate;
/// falls back to 48 kHz for anything above the highest.
pub(crate) fn pick_rate(input_rate: u32) -> u32 {
	RATES.iter().copied().find(|&r| r >= input_rate).unwrap_or(48_000)
}

pub(crate) fn validate_rate(rate: u32) -> Result<(), Error> {
	if RATES.contains(&rate) {
		return Ok(());
	}
	Err(Error::Unsupported(format!(
		"opus only supports 8/12/16/24/48 kHz (got {rate})"
	)))
}

pub(crate) fn validate_channels(count: u32) -> Result<i32, Error> {
	match count {
		1 | 2 => Ok(count as i32),
		other => Err(Error::Unsupported(format!(
			"opus only supports 1 or 2 channels (got {other})"
		))),
	}
}

/// Samples per channel in one frame of `duration` at `sample_rate`.
pub(crate) fn frame_size(sample_rate: u32, duration: Duration) -> Result<usize, Error> {
	let micros = duration.as_micros();
	if !FRAME_DURATIONS.contains(&micros) {
		return Err(Error::Unsupported(format!(
			"opus frame duration must be 2.5/5/10/20/40/60 ms (got {micros} us)"
		)));
	}
	Ok((sample_rate as u128 * micros / 1_000_000) as usize)
}

pub(crate) fn error(code: i32, context: &str) -> Error {
	Error::Unsupported(format!("libopus {context} failed (code {code})"))
}

/// A failed decode call, split by whose fault it is.
///
/// Only a rejected packet is the stream's problem and worth skipping; the rest
/// mean we handed libopus something wrong, which no later packet fixes.
pub(crate) fn decode_error(code: i32) -> Error {
	if code == unsafe_libopus::OPUS_INVALID_PACKET {
		return Error::Decode(format!("libopus rejected the packet (code {code})"));
	}
	error(code, "opus_decode_float")
}

/// Classify a packet a decoder accepted, preserving DTX across packet loss.
///
/// An empty payload asks the decoder for packet-loss concealment, which says
/// nothing about the audio: carry `in_dtx` from the last real packet through it.
pub(crate) fn activity(packet: &[u8], in_dtx: bool) -> Activity {
	if packet.is_empty() {
		return if in_dtx { Activity::Dtx } else { Activity::Active };
	}

	if is_comfort_noise(packet, in_dtx) {
		Activity::Dtx
	} else {
		Activity::Active
	}
}

/// The largest coded frame that is still comfort noise rather than audio.
const SID_BYTES: i16 = 2;

/// Whether an accepted packet's framing looks like comfort noise.
///
/// libopus codes silence as frames that are empty or a two-byte SID payload,
/// where audio fills every frame with tens of bytes. Once DTX is established it
/// also emits a periodic refresh that can carry a larger payload in one frame
/// and leave the others empty, so an empty frame beside that one still means
/// silence rather than a return to audio.
///
/// This reads the framing rather than the payload bytes, so it does not assume
/// the sender's libopus emits the same SID as ours. It cannot be exact: a
/// sender below libopus's `3 * frame_rate * 8` bps floor emits a bare TOC for
/// audio too, and nothing on the wire distinguishes that from silence. Only the
/// encoder can tell the difference, via `OPUS_GET_IN_DTX`.
pub(crate) fn is_comfort_noise(packet: &[u8], in_dtx: bool) -> bool {
	let Some((sizes, count)) = frame_sizes(packet) else {
		return false;
	};
	let Some(sizes) = sizes.get(..count) else {
		return false;
	};
	if sizes.is_empty() {
		return false;
	}

	sizes.iter().all(|&size| size <= SID_BYTES) || (in_dtx && sizes.contains(&0))
}

/// Whether the packet codes nothing at all: a bare TOC with every frame empty.
///
/// Ambiguous on its own. libopus emits this both while withholding silence and,
/// below its `3 * frame_rate * 8` bps floor, for loud audio it has no room to
/// code. Only `OPUS_GET_IN_DTX` separates the two, so a decoder cannot.
pub(crate) fn carries_nothing(packet: &[u8]) -> bool {
	match frame_sizes(packet).and_then(|(sizes, count)| sizes.get(..count).map(<[i16]>::to_vec)) {
		Some(sizes) => !sizes.is_empty() && sizes.iter().all(|&size| size == 0),
		None => false,
	}
}

/// Sizes of the coded frames in an accepted packet, with padding parsed off.
fn frame_sizes(packet: &[u8]) -> Option<([i16; 48], usize)> {
	let len = i32::try_from(packet.len()).ok()?;

	// An Opus packet holds at most 48 frames (RFC 6716 section 3.2.5).
	let mut sizes = [0i16; 48];
	// SAFETY: `sizes` has libopus's maximum frame count, and it bounds the count
	// it writes by that. Passing null for the TOC and frame pointers asks it to
	// skip them, which it checks for; only `size` is required.
	let count = unsafe {
		unsafe_libopus::opus_packet_parse(
			packet.as_ptr(),
			len,
			std::ptr::null_mut(),
			std::ptr::null_mut(),
			sizes.as_mut_ptr(),
			std::ptr::null_mut(),
		)
	};

	Some((sizes, usize::try_from(count).ok()?))
}

/// The coded frame sizes of an accepted packet, so tests can assert on the
/// framing libopus actually produced rather than on a hand-built packet.
#[cfg(test)]
pub(crate) fn frame_sizes_for_test(packet: &[u8]) -> Vec<i16> {
	frame_sizes(packet)
		.and_then(|(sizes, count)| sizes.get(..count).map(<[i16]>::to_vec))
		.unwrap_or_default()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn rate_picker_snaps_up() {
		assert_eq!(pick_rate(44_100), 48_000);
		assert_eq!(pick_rate(22_050), 24_000);
		for &r in &RATES {
			assert_eq!(pick_rate(r), r);
		}
	}

	#[test]
	fn activity_reads_the_opus_framing() {
		// A bare TOC, and the two-byte SID refresh libopus emits at 20 ms.
		assert_eq!(activity(&[0xf8], false), Activity::Dtx);
		assert_eq!(activity(&[0xf8, 0xff], false), Activity::Dtx);
		assert_eq!(activity(&[0xf8, 0xff, 0xfe], false), Activity::Dtx);
		// Loss carries the last real packet's classification.
		assert_eq!(activity(&[], true), Activity::Dtx);
		assert_eq!(activity(&[], false), Activity::Active);
	}

	/// Another sender's comfort noise is not byte-for-byte ours, so only the
	/// framing may decide. Code 3 CBR, three 20 ms frames, contents arbitrary.
	#[test]
	fn activity_ignores_the_sid_payload_bytes() {
		assert_eq!(
			activity(&[0xfb, 0x03, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc], false),
			Activity::Dtx
		);
		// The same framing carrying three-byte frames is audio, not comfort noise.
		assert_eq!(
			activity(
				&[0xfb, 0x03, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x11],
				false
			),
			Activity::Active
		);
	}
}
