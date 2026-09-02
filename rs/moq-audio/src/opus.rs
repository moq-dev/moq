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

/// Classify a packet, preserving DTX across packet loss.
///
/// An empty payload asks the decoder for packet-loss concealment, which says
/// nothing about the audio: carry `in_dtx` from the last real packet through it.
pub(crate) fn activity(packet: &[u8], in_dtx: bool) -> Activity {
	if packet.is_empty() {
		return if in_dtx { Activity::Dtx } else { Activity::Active };
	}

	if carries_nothing(packet) {
		Activity::Dtx
	} else {
		Activity::Active
	}
}

/// Whether the sender coded no audio at all for this packet: a bare TOC whose
/// every frame is empty.
///
/// Exactly the packet libopus emits when it withholds audio, in any mode, from
/// either of the two paths that do so (SILK coding nothing, and the Opus-level
/// silence detector). Nothing else in a conforming encoder produces it except a
/// bitrate below libopus's floor, which codes nothing whatever the input;
/// [`Config::bitrate`](crate::encode::Config::bitrate) refuses those, so our own
/// encoder cannot emit one and this stays exact on both sides.
///
/// It deliberately says nothing about the periodic refresh that interrupts a
/// silence run. That refresh is an ordinarily coded frame of the silence, not a
/// marked packet, so it reads as active. Erring that way keeps the rule from
/// ever calling real audio silence, which is the direction that matters.
pub(crate) fn carries_nothing(packet: &[u8]) -> bool {
	let Some((sizes, count)) = frame_sizes(packet) else {
		return false;
	};

	match sizes.get(..count) {
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

/// The lowest bitrate that still codes audio, in bits per second.
///
/// Below `3 * frame_rate * 8` (and below 2400 for frame rates under 50 Hz)
/// libopus stops coding entirely and emits a bare TOC for every frame, however
/// loud the input, which is byte-for-byte what it sends while withholding
/// silence. Refusing those rates keeps the two apart.
pub(crate) fn bitrate_floor(sample_rate: u32, frame_size: usize) -> u64 {
	// Integer division, matching how libopus derives the same value.
	let frame_rate = u64::from(sample_rate) / frame_size.max(1) as u64;
	let floor = 3 * frame_rate * 8;
	if frame_rate < 50 { floor.max(2_400) } else { floor }
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
		// A bare TOC codes nothing, whatever the mode or frame count.
		assert_eq!(activity(&[0xf8], false), Activity::Dtx);
		assert_eq!(activity(&[0x08], false), Activity::Dtx);
		assert_eq!(activity(&[0xfb, 0x03], false), Activity::Dtx);
		// Anything with a coded frame is audio, however little of it there is.
		assert_eq!(activity(&[0xf8, 0xff, 0xfe], false), Activity::Active);
		assert_eq!(activity(&[0xf8, 0xff], false), Activity::Active);
		// Loss carries the last real packet's classification.
		assert_eq!(activity(&[], true), Activity::Dtx);
		assert_eq!(activity(&[], false), Activity::Active);
	}

	/// A silence run's periodic refresh is an ordinarily coded frame, and a
	/// speech onset can leave an earlier frame of the same packet empty. Neither
	/// is distinguishable by framing, so both read active rather than risking
	/// real audio being called silence.
	#[test]
	fn activity_never_calls_a_coded_frame_silence() {
		// Code 3 VBR, two 20 ms frames: TOC, then the count byte, then the first
		// frame's length. A refresh puts the coded frame first and leaves the
		// second empty; a speech onset is the same shape the other way round.
		let mut refresh = vec![0xfb, 0x82, 57];
		refresh.extend(std::iter::repeat_n(0xaa, 57));
		let mut onset = vec![0xfb, 0x82, 0];
		onset.extend(std::iter::repeat_n(0xaa, 57));

		assert_eq!(activity(&refresh, true), Activity::Active);
		assert_eq!(activity(&onset, true), Activity::Active);

		// The same framing with both frames empty is the packet that does say
		// silence, which also proves the two above really parse as two frames.
		assert_eq!(activity(&[0xfb, 0x82, 0], true), Activity::Dtx);
	}

	#[test]
	fn bitrate_floor_matches_libopus() {
		// 3 * frame_rate * 8, with the 2400 floor libopus adds under 50 Hz.
		assert_eq!(bitrate_floor(48_000, 960), 1_200); // 20 ms
		assert_eq!(bitrate_floor(48_000, 120), 9_600); // 2.5 ms
		assert_eq!(bitrate_floor(48_000, 480), 2_400); // 10 ms
		assert_eq!(bitrate_floor(48_000, 1_920), 2_400); // 40 ms, 25 Hz
		assert_eq!(bitrate_floor(48_000, 2_880), 2_400); // 60 ms, 16 Hz
	}
}
