//! Pace media output on a media clock, following the live edge.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::time::Instant;

use super::Timestamp;

/// A one-shot wall-clock timer wired into kio's poll model.
///
/// [`Pacer`] answers "at what wall-clock instant is this media due"; `Timer` is the
/// other half, letting a `poll_*` function actually wait for that instant. It drives a
/// stored tokio sleep against the poll's [`kio::Waiter`], so the poll re-fires when the
/// deadline passes. moq-mux already depends on tokio, so this keeps the wait local
/// instead of pushing a runtime dependency into kio.
// Wired into the windowed TS exporter's `poll_next`; until that lands only the unit test
// constructs it, so allow the transitional dead code.
#[allow(dead_code)]
#[derive(Default)]
pub(crate) struct Timer {
	/// The armed sleep, kept alive across polls so its timer registration persists.
	sleep: Option<Pin<Box<tokio::time::Sleep>>>,
	/// The instant `sleep` targets, so we only re-arm when it moves.
	until: Option<Instant>,
}

#[allow(dead_code)]
impl Timer {
	/// Poll until `after`. Returns `Ready(now)` once the clock reaches `after`; otherwise
	/// arms (or re-arms) against `waiter` and returns `Pending`. Re-arms only when `after`
	/// changes, so repeated polls for one deadline reuse a single timer registration.
	///
	/// Reads the clock through `tokio::time`, so a `tokio::time::pause()` test advances the
	/// deadline check and the sleep together.
	pub(crate) fn poll(&mut self, after: Instant, waiter: &kio::Waiter) -> Poll<Instant> {
		let now = Instant::now();
		if now >= after {
			self.disarm();
			return Poll::Ready(now);
		}
		if self.until != Some(after) {
			self.until = Some(after);
			self.sleep = Some(Box::pin(tokio::time::sleep_until(after)));
		}
		let mut cx = Context::from_waker(waiter.waker());
		match self.sleep.as_mut().unwrap().as_mut().poll(&mut cx) {
			Poll::Ready(()) => {
				self.disarm();
				Poll::Ready(Instant::now())
			}
			Poll::Pending => Poll::Pending,
		}
	}

	/// Drop the armed sleep. Called once the deadline fires so a later poll for a new
	/// deadline starts clean.
	fn disarm(&mut self) {
		self.sleep = None;
		self.until = None;
	}
}

/// Maps media (decode) timestamps onto the wall clock so a caller can emit frames at
/// the source's real-time rate while bounding how far behind the live edge it falls.
///
/// The first frame anchors the media clock to "now"; every later frame is due at
/// `anchor + (timestamp - base)`. Sleeping until [`Pacer::due`] before emitting a frame
/// drains a retained broadcast at its media rate, like ffmpeg's `-re`, instead of as
/// fast as it can be read.
///
/// To keep a bursty or faster-than-real source from accruing unbounded latency, the
/// timeline holds at most `lead` of buffer ahead of now: when a frame would be due
/// further out than that (a tune-in burst delivers a whole GOP at once, or the source
/// drifts ahead of wall-clock), the anchor jumps forward to the live edge so the buffer
/// never exceeds `lead`. A frame that merely trails the edge (network jitter, a
/// reordered B-frame) keeps its earlier instant.
///
/// `lead` is the target buffer, typically the subscription's max latency. With `lead`
/// = 0 the timeline never leads now, which is what an SRT egress wants (it stamps each
/// payload's TSBPD origin time and the receiver owns the jitter buffer); with `lead` >
/// 0 the caller sleeps to pace output and holds that much buffer itself.
#[derive(Default)]
pub(crate) struct Pacer {
	anchor: Option<Anchor>,
}

/// The media-clock anchor: `base`'s media time maps to `at` on the wall clock.
struct Anchor {
	at: Instant,
	base: Timestamp,
}

impl Pacer {
	/// A pacer that anchors on the first frame it sees.
	pub(crate) fn new() -> Self {
		Self::default()
	}

	/// The wall-clock instant `timestamp` is due, holding at most `lead` of buffer
	/// ahead of now. Reads the clock itself; see [`Self::at`] for the testable core.
	pub(crate) fn due(&mut self, timestamp: Timestamp, lead: Duration) -> Instant {
		self.at(timestamp, lead, Instant::now())
	}

	/// [`Self::due`] with an explicit `now`, so the mapping is deterministic in tests.
	fn at(&mut self, timestamp: Timestamp, lead: Duration, now: Instant) -> Instant {
		let anchor = self.anchor.get_or_insert(Anchor {
			at: now,
			base: timestamp,
		});

		let due = match timestamp.checked_sub(anchor.base) {
			Ok(ahead) => anchor.at + Duration::from(ahead),
			Err(_) => anchor
				.at
				.checked_sub(Duration::from(anchor.base - timestamp))
				.unwrap_or(anchor.at),
		};

		// Never schedule more than `lead` ahead: when the source outruns that, re-anchor
		// the live edge to `now + lead` so the buffer stays bounded. Re-anchoring only ever
		// moves forward, so a frame that trails the edge keeps its earlier instant.
		let cap = now + lead;
		if due > cap {
			anchor.at = cap;
			anchor.base = timestamp;
			cap
		} else {
			due
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn ms(m: u64) -> Timestamp {
		Timestamp::from_micros(m * 1_000).unwrap()
	}

	#[tokio::test(start_paused = true)]
	async fn timer_fires_at_its_deadline() {
		let start = Instant::now();
		let deadline = start + Duration::from_millis(50);
		let mut timer = Timer::default();
		// `kio::wait` drives the poll; under paused time tokio auto-advances to the armed
		// sleep, so this resolves at the deadline rather than blocking for real.
		let fired = kio::wait(|waiter| timer.poll(deadline, waiter)).await;
		assert!(fired >= deadline, "fired {fired:?} before deadline {deadline:?}");
	}

	#[test]
	fn paces_on_the_media_clock() {
		// Within the lead budget, frames pace on the media clock: a frame 40ms later in
		// media is due 40ms after the anchor, however quickly it was read. This is the
		// `-re` case (the caller sleeps in lockstep, so the cap never trips).
		let mut pacer = Pacer::new();
		let start = Instant::now();
		let lead = Duration::from_millis(500);

		assert_eq!(
			pacer.at(ms(1_000), lead, start),
			start,
			"the first frame anchors to now"
		);
		assert_eq!(
			pacer.at(ms(1_040), lead, start + Duration::from_millis(1)),
			start + Duration::from_millis(40),
			"output is paced on the media clock, not arrival time"
		);
	}

	#[test]
	fn re_anchors_past_the_lead_budget() {
		// A tune-in burst: a whole GOP is read at once, so the newest frame's media time
		// runs far past `now`. It re-anchors to `now + lead` rather than scheduling
		// seconds out, so the buffer never exceeds the target latency.
		let mut pacer = Pacer::new();
		let start = Instant::now();
		let lead = Duration::from_millis(500);
		pacer.at(ms(1_000), lead, start);

		let now = start + Duration::from_millis(2);
		let edge = pacer.at(ms(4_132), lead, now);
		assert_eq!(edge, now + lead, "the live edge is held `lead` ahead of now");

		// A later frame paces off the re-anchored edge. The caller slept until the edge
		// was due, so `now` has advanced to it; the 40ms-newer frame is due 40ms past it.
		let now = now + lead;
		assert_eq!(
			pacer.at(ms(4_172), lead, now),
			now + Duration::from_millis(40),
			"subsequent frames pace off the re-anchored edge"
		);
	}

	#[test]
	fn trailing_frame_keeps_its_instant() {
		// A reordered B-frame whose timestamp dips below the edge maps into the past, so
		// the caller's sleep is a no-op and it's emitted immediately. No re-anchor.
		let mut pacer = Pacer::new();
		let start = Instant::now();
		let lead = Duration::from_millis(500);
		pacer.at(ms(1_000), lead, start);

		assert_eq!(
			pacer.at(ms(967), lead, start + Duration::from_millis(5)),
			start - Duration::from_millis(33),
		);
	}

	#[test]
	fn zero_lead_never_leads_now() {
		// `lead` = 0 is the SRT egress policy: the timeline is capped at now, so a burst
		// re-anchors to now (the newest frame is the live edge) and nothing is stamped
		// into the future.
		let mut pacer = Pacer::new();
		let start = Instant::now();
		pacer.at(ms(1_000), Duration::ZERO, start);

		let now = start + Duration::from_millis(2);
		assert_eq!(
			pacer.at(ms(4_132), Duration::ZERO, now),
			now,
			"the live edge paces to now"
		);
	}
}
