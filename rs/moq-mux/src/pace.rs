//! Pacing of media frames onto the wall clock.

use std::time::{Duration, Instant};

use moq_net::Timestamp;

/// Maps each export frame's media timestamp to the wall-clock instant it should
/// be delivered at, re-anchored to the live edge.
///
/// The exporters stamp every [`Frame`](crate::container::Frame) with its media
/// timestamp on the contract that the caller delivers the bytes at the time the
/// stamp asserts. That matters most for MPEG-TS, where the byte stream itself
/// carries no per-frame timing: [`ts::Export`](crate::container::ts::Export)
/// slices its output on the PCR grid and stamps each slice at its slot boundary,
/// and a caller that drains them on arrival collapses the clock into position
/// clusters no downstream stage can repair. "Deliver" is up to the transport:
/// a paced sink sleeps until the returned instant before writing, while a
/// transport with receiver-side buffering (e.g. SRT's TSBPD) stamps the payload
/// with it and sends immediately.
///
/// `send_at = anchor + (ts - base)` maps the frame's media time onto the wall
/// clock, where `base`'s media time and `anchor`'s wall instant were pinned to
/// each other at the last re-anchor. Offsets are computed in nanoseconds, so the
/// frames may change [`Timescale`](moq_net::Timescale) mid-stream: each exporter
/// picks its own, and the TS one stamps grid boundaries in microseconds whatever
/// scale the source's frames carry.
///
/// When `send_at` would lead `now` by more than the configured
/// [`lead`](Self::with_lead), the media clock has outrun wall-clock by more than
/// the caller is willing to buffer: a tune-in burst, a group skip, or producer
/// drift. The pacer re-anchors instead, making this newest frame the live edge
/// (delivered at `now`), and later frames pace relative to it. Re-anchoring only
/// ever moves the anchor *forward*: a frame that merely arrives late (network
/// or CPU jitter, or a reordered B-frame whose timestamp trails the edge) keeps
/// its earlier media instant instead of collapsing to its arrival instant.
///
/// The other direction is the standing lag. Every producer delivers some fixed
/// distance behind the media clock (a muxer's buffer, a hop of network), and the
/// first frame pins an offset that has no room for it, so every later frame is
/// due the instant it arrives, the sleep is a no-op, and the sink ends up writing
/// at whatever cadence frames turned up in. So the anchor slides back by however
/// much a frame fell behind, up to `lead` in total: it discovers that distance
/// rather than assuming one, and the bound is the buffer the caller already said
/// it would hold.
///
/// A player adds a [`delay`](Self::with_delay) on top: it shifts every result
/// without touching the anchor, so playback trails the live edge by a fixed
/// offset that survives every re-anchor.
#[derive(Default)]
pub struct Pacer {
	/// How far ahead of `now` a frame may be scheduled before re-anchoring.
	lead: Duration,
	/// How long every result is held past the instant the anchor maps it to.
	delay: Duration,
	/// The wall instant and media time (in nanoseconds) pinned to each other at
	/// the last re-anchor; every frame paces relative to this pair.
	anchor: Option<(Instant, u128)>,
	/// How far the anchor has slid back to absorb late arrivals, capped at `lead`.
	slack: Duration,
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

	/// Hold every result this long past the instant the anchor maps it to.
	///
	/// The playout delay a player presents against: how far behind the live edge
	/// it runs, and so how late a frame may arrive and still make its slot. It
	/// rides on top of the anchor rather than inside it, so a re-anchor keeps it:
	/// a frame arriving earlier than the anchor predicted pulls playback toward
	/// live and still goes out `delay` after the edge it just set. Zero (the
	/// default) delivers at the edge itself, which is what an export wants.
	pub fn with_delay(mut self, delay: Duration) -> Self {
		self.delay = delay;
		self
	}

	/// The wall-clock instant `ts` should be delivered at, given that it is
	/// being scheduled at `now`.
	///
	/// The first call pins `ts` to `now` and returns `now + delay`; later calls
	/// pace relative to that anchor, re-anchoring whenever the result would lead
	/// `now` by more than the configured lead (see the type docs). The result is
	/// never later than `now + lead + delay`, but may be arbitrarily far in the
	/// past for a frame that arrived late.
	pub fn pace(&mut self, ts: Timestamp, now: Instant) -> Instant {
		let nanos = ts.as_nanos();
		let (anchor, base) = *self.anchor.get_or_insert((now, nanos));

		// The lead is compared against the undelayed instant. Folding the delay in
		// first would have every frame overshoot by that delay and re-anchor,
		// discarding the very offset it was given.
		match send_at(anchor, base, nanos) {
			// `saturating_duration_since` is zero for an `at` in the past, which any
			// lead admits; the subtraction form can't overflow on a huge lead.
			Some(at) if at.saturating_duration_since(now) <= self.lead => {
				let at = self.absorb(at, now);
				self.delayed(at)
			}
			// Media outran wall-clock (or overflowed the platform clock).
			_ => self.hurry(ts, now),
		}
	}

	/// The instant `ts` is due at under the current anchor, or `None` before any
	/// frame has pinned one.
	///
	/// [`pace`](Self::pace) answers the same question for a frame arriving now,
	/// and folds that arrival into the anchor. This is the read-only form, for a
	/// player that queues frames on arrival and asks again at presentation time:
	/// the anchor may have moved since, and a stale instant would present the
	/// whole queue late by however far it moved.
	pub fn due(&self, ts: Timestamp) -> Option<Instant> {
		let (anchor, base) = self.anchor?;
		Some(self.delayed(send_at(anchor, base, ts.as_nanos())?))
	}

	/// Hold an undelayed instant for the configured delay, saturating at the far
	/// end of the platform clock.
	fn delayed(&self, at: Instant) -> Instant {
		at.checked_add(self.delay).unwrap_or(at)
	}

	/// Slide the anchor back by however much `at` fell behind `now`, and return the
	/// instant that leaves.
	///
	/// This is how the pacer finds the producer's standing delivery lag (see the
	/// type docs). The shift is capped so the total never exceeds the lead, and it
	/// can only ever raise a past instant toward `now`, never past it, so absorbing
	/// never schedules a frame later than it would have gone out anyway.
	fn absorb(&mut self, at: Instant, now: Instant) -> Instant {
		let behind = now.saturating_duration_since(at);
		let shift = behind.min(self.lead.saturating_sub(self.slack));
		if shift.is_zero() {
			return at;
		}
		self.slack += shift;
		if let Some((anchor, _)) = self.anchor.as_mut() {
			*anchor += shift;
		}
		at + shift
	}

	/// How much of the producer's standing delivery lag the anchor has absorbed.
	///
	/// A caller running its own lag budget has to know this: the pacer is holding
	/// it deliberately (see the type docs), so counting it as lag would have the
	/// caller shedding the very margin that makes its sleeps do anything.
	pub fn slack(&self) -> Duration {
		self.slack
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
		// A fresh pin has no lag absorbed into it yet, so the budget starts over.
		self.slack = Duration::ZERO;
		// `now` is the live edge, and the delay is measured from it: a re-anchor
		// moves the edge, never the offset playback holds behind it.
		self.delayed(now)
	}
}

/// The undelayed instant `nanos` maps to under the anchor pinned at (`anchor`,
/// `base`), or `None` when the platform clock can't express it.
fn send_at(anchor: Instant, base: u128, nanos: u128) -> Option<Instant> {
	if nanos >= base {
		anchor.checked_add(duration(nanos - base))
	} else {
		// A reordered (B-frame) timestamp can trail the anchor: pace it at that
		// earlier instant instead of collapsing it onto the anchor, falling back
		// to the anchor if the platform clock can't express it.
		Some(anchor.checked_sub(duration(base - nanos)).unwrap_or(anchor))
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
	/// panics on a large but valid `Duration` (`--max-age` is unbounded).
	#[test]
	fn huge_lead_does_not_overflow() {
		let start = Instant::now();
		let mut pacer = Pacer::default().with_lead(Duration::MAX);
		assert_eq!(pacer.pace(ms(0), start), start);
		assert_eq!(pacer.pace(ms(40), start), start + Duration::from_millis(40));
	}

	/// A producer that delivers a constant distance behind the media clock (the TS
	/// exporter's mux buffer, or a hop of network) would otherwise pin that distance
	/// into the first frame's anchor and never get it back: every later frame is due
	/// exactly when it arrives, so the sleep is a no-op and the sink writes at the
	/// arrival cadence rather than the media one.
	#[test]
	fn absorbs_a_standing_delivery_lag() {
		let start = Instant::now();
		let mut pacer = Pacer::default().with_lead(Duration::from_millis(500));
		assert_eq!(pacer.pace(ms(0), start), start, "the first frame anchors at now");

		// Steady state: the producer is 40ms behind, so this frame is already due.
		let now = start + Duration::from_millis(80);
		assert_eq!(pacer.pace(ms(40), now), now, "a late frame still goes out at once");

		// The anchor absorbed those 40ms, so the next frame has room to be paced.
		assert_eq!(
			pacer.pace(ms(80), now),
			now + Duration::from_millis(40),
			"the discovered lag becomes the sink's margin"
		);
	}

	/// The absorbed lag is a buffer the caller holds, so it is capped by the lead
	/// rather than growing with every outlier.
	#[test]
	fn absorbed_lag_is_capped_by_the_lead() {
		let start = Instant::now();
		let mut pacer = Pacer::default().with_lead(Duration::from_millis(50));
		assert_eq!(pacer.pace(ms(0), start), start);

		// 200ms behind, but only 50ms of that may be taken.
		let now = start + Duration::from_millis(240);
		assert_eq!(pacer.pace(ms(40), now), start + Duration::from_millis(90));
		assert_eq!(
			pacer.pace(ms(80), now),
			start + Duration::from_millis(130),
			"the anchor moved by the cap, not by the shortfall"
		);
	}

	/// Zero lead (SRT, whose receiver owns the jitter buffer) absorbs nothing.
	#[test]
	fn zero_lead_absorbs_nothing() {
		let start = Instant::now();
		let mut pacer = Pacer::default();
		assert_eq!(pacer.pace(ms(0), start), start);

		let now = start + Duration::from_millis(80);
		assert_eq!(pacer.pace(ms(40), now), start + Duration::from_millis(40));
		assert_eq!(pacer.pace(ms(80), now), start + Duration::from_millis(80));
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

	/// The delay shifts the schedule without entering the anchor, so the frame
	/// after the first still paces off its own media time rather than collapsing
	/// onto a re-anchor the delay itself provoked.
	#[test]
	fn the_delay_is_outside_the_lead_comparison() {
		let start = Instant::now();
		let mut pacer = Pacer::default().with_delay(Duration::from_millis(100));

		assert_eq!(pacer.pace(ms(0), start), start + Duration::from_millis(100));
		// 40ms of media arriving 40ms later: on the anchor, so it must not re-anchor.
		let now = start + Duration::from_millis(40);
		assert_eq!(pacer.pace(ms(40), now), start + Duration::from_millis(140));
		// And another 40ms on, still on the original anchor rather than a re-anchor
		// the delay provoked.
		let now = start + Duration::from_millis(80);
		assert_eq!(pacer.pace(ms(80), now), start + Duration::from_millis(180));
	}

	/// The headline player defect: a first frame that arrives late pins the delay
	/// plus that lateness, and every earlier arrival afterwards has to pull
	/// playback back toward live rather than leaving it there for the session.
	#[test]
	fn an_earlier_arrival_re_anchors_and_keeps_the_delay() {
		let start = Instant::now();
		let mut pacer = Pacer::default().with_delay(Duration::from_millis(100));

		// The first frame was produced 500ms ago; nothing here knows that yet.
		assert_eq!(pacer.pace(ms(0), start), start + Duration::from_millis(100));

		// 40ms of media later, but the next frame took only 20ms to arrive, so the
		// anchor is 480ms behind live. Re-anchor onto it, delay intact.
		let now = start + Duration::from_millis(20);
		assert_eq!(pacer.pace(ms(40), now), now + Duration::from_millis(100));

		// A frame that merely arrives late keeps its media instant instead: the
		// delay is exactly the room it has to be late in.
		let late = now + Duration::from_millis(70);
		assert_eq!(
			pacer.pace(ms(80), late),
			now + Duration::from_millis(140),
			"a late arrival must not re-anchor"
		);
	}

	/// A caller's own re-anchor is a move of the live edge, not a decision to
	/// present at it: the offset playback holds behind the edge has to survive.
	#[test]
	fn hurry_keeps_the_delay() {
		let start = Instant::now();
		let mut pacer = Pacer::default().with_delay(Duration::from_millis(100));
		assert_eq!(pacer.hurry(ms(800), start), start + Duration::from_millis(100));

		let now = start + Duration::from_millis(40);
		assert_eq!(pacer.pace(ms(840), now), start + Duration::from_millis(140));
	}

	/// A player queues frames on arrival and asks again when it presents them, by
	/// which time another track may have moved the anchor. Asking must not fold a
	/// second arrival in.
	#[test]
	fn due_reads_the_anchor_without_moving_it() {
		let start = Instant::now();
		let mut pacer = Pacer::default().with_delay(Duration::from_millis(100));
		assert_eq!(pacer.due(ms(0)), None, "no anchor until the first frame");

		pacer.pace(ms(0), start);
		assert_eq!(pacer.due(ms(40)), Some(start + Duration::from_millis(140)));
		assert_eq!(pacer.due(ms(40)), Some(start + Duration::from_millis(140)));

		// Another track re-anchors, and the queued frame is due earlier for it.
		let now = start + Duration::from_millis(10);
		pacer.hurry(ms(20), now);
		assert_eq!(pacer.due(ms(40)), Some(now + Duration::from_millis(120)));
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
