//! libopus constraints, shared by the encoder and decoder.
//!
//! Both sides have to agree on which rates, channel counts, and frame durations
//! libopus accepts, so the checks live here rather than being duplicated (and
//! drifting) across `encode` and `decode`.

use std::time::Duration;

use crate::Error;

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
}
