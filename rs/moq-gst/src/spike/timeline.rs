//! Pure SEGMENT/running-time policy, split out so it unit-tests with plain numbers, no pipeline.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentInfo {
	/// Only TIME segments map to a media timeline (not BYTES/DEFAULT).
	pub time_format: bool,
	pub rate: f64,
	/// Running-time anchor of the segment; continuity is judged on this, not on `start`.
	pub base_nanos: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SegmentDecision {
	Accept,
	Reject(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
pub enum FrameDecision {
	/// Emit at this MoQ timestamp (micros).
	Emit(u64),
	Drop(&'static str),
}

/// The first TIME segment fixes the timeline. Continuity is judged in running time, not media
/// origin: `base` is the running-time anchor, so a moved `start` (a seek that keeps moving forward)
/// stays continuous as long as `base` does not rewind. A rewind is rejected so the pad stops rather
/// than splicing two timelines.
pub fn classify_segment(prev: Option<&SegmentInfo>, next: &SegmentInfo) -> SegmentDecision {
	if !next.time_format {
		return SegmentDecision::Reject("segment is not in TIME format");
	}
	if next.rate != 1.0 {
		return SegmentDecision::Reject("segment rate is not 1.0");
	}
	match prev {
		None => SegmentDecision::Accept,
		Some(prev) if next.base_nanos >= prev.base_nanos => SegmentDecision::Accept,
		Some(_) => SegmentDecision::Reject("discontinuous segment (running time rewound)"),
	}
}

/// Maps a signed running time (nanos) to a MoQ timestamp. Stateless and shared across pads on
/// purpose: re-anchoring per pad is what breaks A/V alignment. A buffer before the segment (a
/// negative running time) is dropped, never clamped to zero, since clamping would collapse distinct
/// frames onto one timestamp.
pub fn frame_micros(running_time_nanos: Option<i64>) -> FrameDecision {
	match running_time_nanos {
		Some(nanos) if nanos >= 0 => FrameDecision::Emit(nanos as u64 / 1000),
		Some(_) => FrameDecision::Drop("buffer before the segment (negative running time)"),
		None => FrameDecision::Drop("buffer outside the segment"),
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
		assert_eq!(classify_segment(None, &time(1.0, 0)), SegmentDecision::Accept);
	}

	#[test]
	fn non_time_segment_is_rejected() {
		let bytes = SegmentInfo {
			time_format: false,
			rate: 1.0,
			base_nanos: 0,
		};
		assert!(matches!(classify_segment(None, &bytes), SegmentDecision::Reject(_)));
	}

	#[test]
	fn non_unit_rate_is_rejected() {
		assert!(matches!(
			classify_segment(None, &time(2.0, 0)),
			SegmentDecision::Reject(_)
		));
		assert!(matches!(
			classify_segment(None, &time(-1.0, 0)),
			SegmentDecision::Reject(_)
		));
	}

	#[test]
	fn advancing_base_is_continuous() {
		let first = time(1.0, 0);
		assert_eq!(classify_segment(Some(&first), &time(1.0, 500)), SegmentDecision::Accept);
	}

	// Equal base is still continuous: continuity is base-monotonic, not strictly increasing.
	#[test]
	fn equal_base_is_continuous() {
		let first = time(1.0, 500);
		assert_eq!(classify_segment(Some(&first), &time(1.0, 500)), SegmentDecision::Accept);
	}

	#[test]
	fn rewinding_base_is_rejected() {
		let first = time(1.0, 500);
		assert!(matches!(
			classify_segment(Some(&first), &time(1.0, 400)),
			SegmentDecision::Reject(_)
		));
	}

	#[test]
	fn positive_running_time_emits_micros() {
		assert_eq!(frame_micros(Some(20_000_000)), FrameDecision::Emit(20_000));
	}

	#[test]
	fn negative_running_time_is_dropped_not_clamped() {
		assert!(matches!(frame_micros(Some(-5_000_000)), FrameDecision::Drop(_)));
	}

	#[test]
	fn out_of_segment_frame_is_dropped() {
		assert!(matches!(frame_micros(None), FrameDecision::Drop(_)));
	}

	// frame_micros is a stateless conversion (no last-emitted state, no per-pad anchor). The real
	// A/V-offset guarantee is exercised by two_pads_keep_av_aligned_through_real_segments.
	#[test]
	fn frame_micros_is_stateless() {
		assert_eq!(frame_micros(Some(7_000_000)), FrameDecision::Emit(7_000));
		assert_eq!(frame_micros(Some(5_000_000)), FrameDecision::Emit(5_000));
	}
}
