//! Pure SEGMENT/running-time policy, split out so it unit-tests with plain numbers, no pipeline.

/// The facts about a SEGMENT that decide timeline continuity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentInfo {
	/// Only TIME segments map to a media timeline (not BYTES/DEFAULT).
	pub time_format: bool,
	/// Playback rate; only a unit rate (1.0) maps to a continuous media timeline.
	pub rate: f64,
	/// Running-time anchor of the segment; continuity is judged on this, not on `start`.
	pub base_nanos: u64,
}

/// Decide whether a SEGMENT can anchor or continue the timeline. `Err` carries the reason for logs.
///
/// The first TIME segment fixes the timeline. Continuity is judged in running time, not media origin:
/// `base` is the running-time anchor, so a moved `start` (a seek that keeps moving forward) stays
/// continuous as long as `base` does not rewind. A rewind is rejected so the pad stops rather than
/// splicing two timelines.
pub fn classify_segment(prev: Option<&SegmentInfo>, next: &SegmentInfo) -> Result<(), &'static str> {
	if !next.time_format {
		return Err("segment is not in TIME format");
	}
	if next.rate != 1.0 {
		return Err("segment rate is not 1.0");
	}
	match prev {
		None => Ok(()),
		Some(prev) if next.base_nanos >= prev.base_nanos => Ok(()),
		Some(_) => Err("discontinuous segment (running time rewound)"),
	}
}

/// Map a signed running time (nanos) to a MoQ timestamp in micros, or `Err` with why it was dropped.
///
/// Stateless and shared across pads on purpose: re-anchoring per pad is what breaks A/V alignment. A
/// buffer before the segment (a negative running time) is dropped, never clamped to zero, since
/// clamping would collapse distinct frames onto one timestamp.
pub fn frame_micros(running_time_nanos: Option<i64>) -> Result<u64, &'static str> {
	match running_time_nanos {
		Some(nanos) if nanos >= 0 => Ok(nanos as u64 / 1000),
		Some(_) => Err("buffer before the segment (negative running time)"),
		None => Err("buffer outside the segment"),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn time(rate: f64, base: u64) -> SegmentInfo {
		SegmentInfo {
			time_format: true,
			rate,
			base_nanos: base,
		}
	}

	#[test]
	fn first_time_segment_is_accepted() {
		assert_eq!(classify_segment(None, &time(1.0, 0)), Ok(()));
	}

	#[test]
	fn non_time_segment_is_rejected() {
		let bytes = SegmentInfo {
			time_format: false,
			rate: 1.0,
			base_nanos: 0,
		};
		assert!(classify_segment(None, &bytes).is_err());
	}

	#[test]
	fn non_unit_rate_is_rejected() {
		assert!(classify_segment(None, &time(2.0, 0)).is_err());
		assert!(classify_segment(None, &time(-1.0, 0)).is_err());
	}

	#[test]
	fn advancing_base_is_continuous() {
		let first = time(1.0, 0);
		assert_eq!(classify_segment(Some(&first), &time(1.0, 500)), Ok(()));
	}

	// Equal base is still continuous: continuity is base-monotonic, not strictly increasing.
	#[test]
	fn equal_base_is_continuous() {
		let first = time(1.0, 500);
		assert_eq!(classify_segment(Some(&first), &time(1.0, 500)), Ok(()));
	}

	#[test]
	fn rewinding_base_is_rejected() {
		let first = time(1.0, 500);
		assert!(classify_segment(Some(&first), &time(1.0, 400)).is_err());
	}

	#[test]
	fn positive_running_time_emits_micros() {
		assert_eq!(frame_micros(Some(20_000_000)), Ok(20_000));
	}

	#[test]
	fn negative_running_time_is_dropped_not_clamped() {
		assert!(frame_micros(Some(-5_000_000)).is_err());
	}

	#[test]
	fn out_of_segment_frame_is_dropped() {
		assert!(frame_micros(None).is_err());
	}

	// frame_micros is a stateless conversion (no last-emitted state, no per-pad anchor). The real
	// A/V-offset guarantee is exercised by two_pads_keep_av_aligned_through_real_segments.
	#[test]
	fn frame_micros_is_stateless() {
		assert_eq!(frame_micros(Some(7_000_000)), Ok(7_000));
		assert_eq!(frame_micros(Some(5_000_000)), Ok(5_000));
	}
}
