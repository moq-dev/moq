//! Pacing of export frames onto the wall clock.

use std::time::{Duration, Instant};

use moq_net::Timestamp;

/// Maps each export frame's media timestamp to the wall-clock instant it should
/// be delivered at, re-anchored to the live edge.
///
/// The exporters stamp every [`Frame`](crate::container::Frame) with its media
/// timestamp on the contract that the caller delivers the bytes at the time the
/// stamp asserts. That matters most for MPEG-TS, where the byte stream itself
/// carries no per-frame timing: [`ts::Export`](crate::container::ts::Export)
/// emits its PCR grid as standalone frames stamped at their slot boundaries, and
/// a caller that drains them on arrival collapses the clock into position
/// clusters no downstream stage can repair. "Deliver" is up to the transport:
/// a paced sink sleeps until the returned instant before writing, while a
/// transport with receiver-side buffering (e.g. SRT's TSBPD) stamps the payload
/// with it and sends immediately.
///
/// `send_at = anchor + (ts - base)` maps the frame's media time onto the wall
/// clock, where `base`'s media time and `anchor`'s wall instant were pinned to
/// each other at the last re-anchor. Offsets are computed in nanoseconds, so the
/// frames may change [`Timescale`](moq_net::Timescale) mid-stream (the TS
/// exporter stamps PCR frames in microseconds and media frames at the source's
/// own scale).
///
/// When `send_at` would lead `now` by more than the configured
/// [`lead`](Self::with_lead), the media clock has outrun wall-clock by more than
/// the caller is willing to buffer: a tune-in burst, a group skip, or producer
/// drift. The pacer re-anchors instead, making this newest frame the live edge
/// (delivered at `now`), and later frames pace relative to it. Re-anchoring only
/// ever moves the anchor *forward*: a frame that merely arrives late (network
/// or CPU jitter, or a reordered B-frame whose timestamp trails the edge) keeps
/// its earlier media instant instead of collapsing to its arrival instant.
#[derive(Default)]
pub struct Pacer {
	/// How far ahead of `now` a frame may be scheduled before re-anchoring.
	lead: Duration,
	/// The wall instant and media time (in nanoseconds) pinned to each other at
	/// the last re-anchor; every frame paces relative to this pair.
	anchor: Option<(Instant, u128)>,
}

impl Pacer {
	/// Set how far ahead of the wall clock a frame may be scheduled.
	///
	/// This is the smoothing buffer the caller holds: a paced sink sleeps up to
	/// this long per frame, so an arrival burst spanning at most `lead` of media
	/// time drains evenly instead of re-anchoring. Zero (the default) never
	/// schedules into the future, for transports whose receiver owns the jitter
	/// buffer and reconstructs spacing from the stamped instants.
	pub fn with_lead(mut self, lead: Duration) -> Self {
		self.lead = lead;
		self
	}

	/// The wall-clock instant `ts` should be delivered at, given that it is
	/// being scheduled at `now`.
	///
	/// The first call pins `ts` to `now` and returns `now`; later calls pace
	/// relative to that anchor, re-anchoring whenever the result would lead
	/// `now` by more than the configured lead (see the type docs). The result is
	/// never later than `now + lead`, but may be arbitrarily far in the past for
	/// a frame that arrived late.
	pub fn pace(&mut self, ts: Timestamp, now: Instant) -> Instant {
		let nanos = ts.as_nanos();
		let (anchor, base) = *self.anchor.get_or_insert((now, nanos));

		let send_at = if nanos >= base {
			anchor.checked_add(duration(nanos - base))
		} else {
			// A reordered (B-frame) timestamp can trail the anchor: pace it at that
			// earlier instant instead of collapsing it onto the anchor, falling back
			// to the anchor if the platform clock can't express it.
			Some(anchor.checked_sub(duration(base - nanos)).unwrap_or(anchor))
		};

		match send_at {
			// `saturating_duration_since` is zero for an `at` in the past, which any
			// lead admits; the subtraction form can't overflow on a huge lead.
			Some(at) if at.saturating_duration_since(now) <= self.lead => at,
			// Media outran wall-clock (or overflowed the platform clock).
			_ => self.hurry(ts, now),
		}
	}

	/// Deliver `ts` at `now` and make it the live edge: later frames pace
	/// relative to this pair.
	///
	/// This is the re-anchor [`pace`](Self::pace) applies when a frame overshoots
	/// the lead, exposed for callers whose own lag detection is stricter than
	/// `pace`'s. A sleeping sink's sleeps push the `now` it paces with forward, so
	/// a backlog can stay within the lead of every individual call while total
	/// delivery lag grows; such a caller measures lag against when the frame could
	/// have arrived and hurries when that overshoots.
	pub fn hurry(&mut self, ts: Timestamp, now: Instant) -> Instant {
		self.anchor = Some((now, ts.as_nanos()));
		now
	}
}

/// A nanosecond span as a [`Duration`], saturating at ~584 years.
fn duration(nanos: u128) -> Duration {
	Duration::from_nanos(nanos.try_into().unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn ms(m: u64) -> Timestamp {
		Timestamp::from_micros(m * 1_000).unwrap()
	}

	#[test]
	fn re_anchors_to_live_edge() {
		// Tune-in burst: the live edge (4132ms of media) is produced ~8ms after the
		// first frame (1400ms). It must re-anchor to `now` rather than schedule
		// ~2.7s into the future.
		let start = Instant::now();
		let mut pacer = Pacer::default();
		assert_eq!(pacer.pace(ms(1_400), start), start, "the first frame anchors at now");

		let now = start + Duration::from_millis(8);
		assert_eq!(pacer.pace(ms(4_132), now), now, "the live edge paces to now");

		// The re-anchor moved the base up to the live edge (4132ms <-> now). A frame
		// 33ms newer in MEDIA that arrives 80ms later in WALL-clock (jitter) paces
		// from that carried-forward anchor: its media instant (+33ms off the edge),
		// not its 80ms arrival instant.
		let jittered = pacer.pace(ms(4_165), now + Duration::from_millis(80));
		assert_eq!(
			jittered,
			now + Duration::from_millis(33),
			"a late frame keeps its media instant, not its arrival instant"
		);

		// A reordered B-frame can carry a timestamp before the re-anchored live
		// edge. Keep that earlier media instant instead of flattening it onto the
		// anchor.
		let reordered = pacer.pace(ms(4_099), now + Duration::from_millis(100));
		assert_eq!(
			reordered,
			now - Duration::from_millis(33),
			"a reordered frame can pace before the anchor"
		);
	}

	/// Regression for #2984: the TS exporter stamps PCR frames in microseconds and
	/// media frames at the source's own timescale (90 kHz for a TS import), so the
	/// pacer must compare timestamps across scales. A scale-strict subtraction
	/// (`Timestamp::checked_sub`) errors on the mix and collapsed every media
	/// frame onto the anchor.
	#[test]
	fn paces_across_timescales() {
		let start = Instant::now();
		let mut pacer = Pacer::default().with_lead(Duration::from_millis(500));

		// A PCR slot at microsecond scale anchors the stream.
		assert_eq!(pacer.pace(ms(0), start), start);

		// A media frame 40ms later at 90 kHz (3600 ticks) paces on the same clock.
		let media = Timestamp::from_scale(3_600, 90_000).unwrap();
		assert_eq!(pacer.pace(media, start), start + Duration::from_millis(40));

		// The next PCR slot, back at microsecond scale, lands on its own boundary.
		assert_eq!(pacer.pace(ms(50), start), start + Duration::from_millis(50));
	}

	/// Regression: the lead comparison must not construct `now + lead`, which
	/// panics on a large but valid `Duration` (`--latency-max` is unbounded).
	#[test]
	fn huge_lead_does_not_overflow() {
		let start = Instant::now();
		let mut pacer = Pacer::default().with_lead(Duration::MAX);
		assert_eq!(pacer.pace(ms(0), start), start);
		assert_eq!(pacer.pace(ms(40), start), start + Duration::from_millis(40));
	}

	#[test]
	fn hurry_makes_the_frame_the_live_edge() {
		let start = Instant::now();
		let mut pacer = Pacer::default().with_lead(Duration::from_millis(500));
		assert_eq!(pacer.pace(ms(0), start), start);

		// A caller's stricter lag detection can force the re-anchor `pace` alone
		// would not apply: the frame goes out at `now` and becomes the new base.
		let now = start + Duration::from_millis(100);
		assert_eq!(pacer.hurry(ms(800), now), now);
		assert_eq!(pacer.pace(ms(840), now), now + Duration::from_millis(40));
	}

	#[test]
	fn lead_schedules_bursts_into_the_future() {
		let start = Instant::now();
		let mut pacer = Pacer::default().with_lead(Duration::from_millis(500));
		assert_eq!(pacer.pace(ms(0), start), start);

		// An arrival burst within the lead window is spaced, not re-anchored.
		assert_eq!(pacer.pace(ms(40), start), start + Duration::from_millis(40));
		assert_eq!(pacer.pace(ms(500), start), start + Duration::from_millis(500));

		// One step beyond the lead is a discontinuity: re-anchor to now.
		assert_eq!(pacer.pace(ms(1_200), start), start);
		// And later frames pace off the new anchor.
		assert_eq!(pacer.pace(ms(1_240), start), start + Duration::from_millis(40));
	}
}
