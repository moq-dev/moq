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

/// Classify a packet libopus accepted, preserving DTX across packet loss.
///
/// Comfort noise is the one thing an Opus packet says about itself by size: a
/// frame that codes silence is the TOC byte plus at most a two-byte SID
/// payload, where real audio is tens of bytes even at the lowest bitrates. So a
/// packet is DTX when every coded frame in it is that small, which covers the
/// one-byte entry packet, the periodic multi-byte SID refresh, and the
/// multi-frame refresh a 40 or 60 ms configuration emits. Reading the framing
/// rather than the payload keeps this working for senders whose libopus build
/// differs from ours.
///
/// An empty payload asks the decoder for packet-loss concealment, which says
/// nothing about the audio: carry `in_dtx` from the last real packet through it.
pub(crate) fn activity(packet: &[u8], in_dtx: bool) -> Activity {
	if packet.is_empty() {
		return if in_dtx { Activity::Dtx } else { Activity::Active };
	}

	if is_comfort_noise(packet) {
		Activity::Dtx
	} else {
		Activity::Active
	}
}

/// The largest coded frame that is still comfort noise rather than audio.
const SID_BYTES: i16 = 2;

/// Whether every coded frame in an accepted packet is comfort noise, ignoring
/// any padding the sender added.
fn is_comfort_noise(packet: &[u8]) -> bool {
	let Ok(len) = i32::try_from(packet.len()) else {
		return false;
	};

	// An Opus packet holds at most 48 frames (RFC 6716 section 3.2.5).
	let mut sizes = [0i16; 48];
	// SAFETY: `sizes` has libopus's maximum frame count. Passing null for the TOC
	// and frame pointers asks it to skip them, which it checks for; only `size` is
	// required. Padding is parsed off rather than counted.
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

	match usize::try_from(count).ok().and_then(|count| sizes.get(..count)) {
		Some(sizes) if !sizes.is_empty() => sizes.iter().all(|&size| size <= SID_BYTES),
		_ => false,
	}
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
		assert_eq!(activity(&[0xf8], false), Activity::Dtx);
		assert_eq!(activity(&[0xf8, 0xff], false), Activity::Dtx);
		assert_eq!(activity(&[], true), Activity::Dtx);
		assert_eq!(activity(&[], false), Activity::Active);
		// A SID refresh is comfort noise whatever the last packet was.
		assert_eq!(activity(&[0xf8, 0xff, 0xfe], false), Activity::Dtx);
		assert_eq!(activity(&[0xf8, 0xff, 0xfe], true), Activity::Dtx);
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
