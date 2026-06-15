//! Pure SEGMENT/running-time policy, split out so it unit-tests with plain numbers, no pipeline.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentInfo {
	/// Only TIME segments map to a media timeline (not BYTES/DEFAULT).
	pub time_format: bool,
	pub rate: f64,
	pub start_nanos: u64,
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

/// First TIME segment fixes the timeline; a discontinuity is rejected so the pad stops rather than
/// splicing two timelines.
pub fn classify_segment(prev: Option<&SegmentInfo>, next: &SegmentInfo) -> SegmentDecision {
	if !next.time_format {
		return SegmentDecision::Reject("segment is not in TIME format");
	}
	if next.rate != 1.0 {
		return SegmentDecision::Reject("segment rate is not 1.0");
	}
	match prev {
		None => SegmentDecision::Accept,
		Some(prev) if next.start_nanos == prev.start_nanos && next.base_nanos >= prev.base_nanos => {
			SegmentDecision::Accept
		}
		Some(_) => SegmentDecision::Reject("discontinuous segment (flush/seek)"),
	}
}

/// Stateless and shared across pads on purpose: re-anchoring per pad is what breaks A/V alignment.
/// `None` (outside the segment) drops, never clamps.
pub fn frame_micros(has_segment: bool, running_time_nanos: Option<u64>) -> FrameDecision {
	if !has_segment {
		return FrameDecision::Drop("buffer arrived before a SEGMENT");
	}
	match running_time_nanos {
		Some(running_time) => FrameDecision::Emit(running_time / 1000),
		None => FrameDecision::Drop("buffer outside the segment"),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn time(rate: f64, start: u64, base: u64) -> SegmentInfo {
		SegmentInfo {
			time_format: true,
			rate,
			start_nanos: start,
			base_nanos: base,
		}
	}

	#[test]
	fn first_time_segment_is_accepted() {
		assert_eq!(classify_segment(None, &time(1.0, 0, 0)), SegmentDecision::Accept);
	}

	#[test]
	fn non_time_segment_is_rejected() {
		let bytes = SegmentInfo {
			time_format: false,
			rate: 1.0,
			start_nanos: 0,
			base_nanos: 0,
		};
		assert!(matches!(classify_segment(None, &bytes), SegmentDecision::Reject(_)));
	}

	#[test]
	fn non_unit_rate_is_rejected() {
		assert!(matches!(classify_segment(None, &time(2.0, 0, 0)), SegmentDecision::Reject(_)));
		assert!(matches!(classify_segment(None, &time(-1.0, 0, 0)), SegmentDecision::Reject(_)));
	}

	#[test]
	fn continuous_second_segment_is_accepted() {
		let first = time(1.0, 100, 0);
		assert_eq!(classify_segment(Some(&first), &time(1.0, 100, 500)), SegmentDecision::Accept);
	}

	#[test]
	fn discontinuous_second_segment_is_rejected() {
		let first = time(1.0, 100, 500);
		// start moved (a seek) -> reject.
		assert!(matches!(
			classify_segment(Some(&first), &time(1.0, 200, 600)),
			SegmentDecision::Reject(_)
		));
		// base went backwards (a flush rewind) -> reject.
		assert!(matches!(
			classify_segment(Some(&first), &time(1.0, 100, 400)),
			SegmentDecision::Reject(_)
		));
	}

	#[test]
	fn buffer_before_segment_is_dropped() {
		assert!(matches!(frame_micros(false, Some(0)), FrameDecision::Drop(_)));
	}

	#[test]
	fn out_of_segment_frame_is_dropped_not_clamped() {
		assert!(matches!(frame_micros(true, None), FrameDecision::Drop(_)));
	}

	// Stateless conversion: two pads sharing one running-time clock keep their relative offset.
	#[test]
	fn shared_timeline_keeps_av_aligned() {
		// Simultaneous frames on two pads (same running time) get the same timestamp.
		assert_eq!(frame_micros(true, Some(20_000_000)), FrameDecision::Emit(20_000));
		assert_eq!(frame_micros(true, Some(20_000_000)), FrameDecision::Emit(20_000));

		// A real 2ms offset between a video and an audio frame survives, regardless of call order.
		assert_eq!(frame_micros(true, Some(7_000_000)), FrameDecision::Emit(7_000));
		assert_eq!(frame_micros(true, Some(5_000_000)), FrameDecision::Emit(5_000));
	}
}
