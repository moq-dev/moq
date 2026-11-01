//! Bitrate caps for WHIP ingest.
//!
//! An embedder hands [`whip::accept`](crate::whip::accept) a [`Limit`] through
//! [`Request::with_rate`](crate::whip::Request::with_rate), and moq-rtc steers the
//! publisher's encoder under its [`Cap`]: a `b=TIAS`
//! line in the SDP answer, and periodic REMB where the publisher negotiated
//! it. WebRTC has no receiver backpressure, so a publisher that ignores both is metered
//! and closed with [`Error::RateLimited`](crate::Error::RateLimited) past the cap plus its
//! burst.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use kio::clock::Instant;
use moq_net::bandwidth::Rate;
use tokio::sync::watch;

/// The share of the cap moq-rtc steers the encoder to.
///
/// The meter counts every media payload, but the steering lines only bound what the
/// browser's video encoder targets: audio rides on top, and encoders overshoot their
/// target around keyframes. Steering to the cap itself would let an honest publisher
/// drift past it and be closed, so the steered rate leaves a tenth of the cap for both.
const STEER: f64 = 0.9;

/// How long the meter remembers the credit it discarded, for frames still assembling.
///
/// str0m completes a frame at most its video reordering timeout after the frame's first
/// packet, 2 seconds by default, which moq-rtc keeps; audio waits half that.
const ASSEMBLY: Duration = Duration::from_secs(2);

/// A bitrate cap: the sustained rate media may arrive at, and the burst above it.
///
/// The cap counts media payload, the depacketized frames a publisher sends. RTP, SRTP,
/// and UDP overhead sits outside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Cap {
	/// The sustained rate.
	pub rate: Rate,
	/// How far ahead of `rate` media may run, as time at `rate`. Defaults to
	/// [`Cap::DEFAULT_BURST`], as on QUIC.
	pub burst: Duration,
}

impl Cap {
	/// About one second of the cap.
	pub const DEFAULT_BURST: Duration = Duration::from_secs(1);

	/// A cap at `rate` with the default burst.
	pub const fn new(rate: Rate) -> Self {
		Self {
			rate,
			burst: Self::DEFAULT_BURST,
		}
	}

	/// This cap with a burst of `burst` at its rate.
	pub const fn with_burst(mut self, burst: Duration) -> Self {
		self.burst = burst;
		self
	}

	/// The rate the encoder is steered to, in the answer and in REMB: never above the cap.
	pub(crate) fn steer(self) -> Rate {
		self.rate.scaled(STEER)
	}

	/// The burst in bytes.
	fn burst_bytes(self) -> f64 {
		self.rate.as_bps() as f64 / 8.0 * self.burst.as_secs_f64()
	}
}

impl From<Rate> for Cap {
	fn from(rate: Rate) -> Self {
		Self::new(rate)
	}
}

/// A live cap for one WHIP session.
///
/// Clones share the cap. [`Limit::set`] applies to a running session at once: the
/// meter switches to the new cap, and the next REMB goes out immediately, lifting its
/// ceiling when the cap is lifted. The SDP answer's TIAS is fixed at the cap set when the
/// offer was accepted, since moq-rtc never renegotiates: a session that starts uncapped
/// steers only through REMB, and raising or lifting a starting cap frees the publisher
/// only as far as that TIAS allows.
#[derive(Clone)]
pub struct Limit {
	cap: Arc<watch::Sender<Option<Cap>>>,
}

impl Limit {
	/// A handle starting at `cap`, or uncapped for `None`.
	pub fn new(cap: impl Into<Option<Cap>>) -> Self {
		Self {
			cap: Arc::new(watch::Sender::new(cap.into())),
		}
	}

	/// An uncapped handle.
	pub fn unlimited() -> Self {
		Self::new(None)
	}

	/// Replace the cap; `None` lifts it.
	pub fn set(&self, cap: impl Into<Option<Cap>>) {
		self.cap.send_replace(cap.into());
	}

	/// The current cap, if any.
	pub fn get(&self) -> Option<Cap> {
		*self.cap.borrow()
	}

	pub(crate) fn subscribe(&self) -> watch::Receiver<Option<Cap>> {
		self.cap.subscribe()
	}
}

impl Default for Limit {
	fn default() -> Self {
		Self::unlimited()
	}
}

impl std::fmt::Debug for Limit {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_tuple("Limit").field(&self.get()).finish()
	}
}

/// A token bucket over received media payload: full at the burst, refilled at the rate.
///
/// It charges whole frames as str0m completes them, stamped with their first packet's
/// arrival, so frames can be charged out of order: a frame that completes after a
/// later-starting one is charged behind it. str0m 0.24 surfaces individual RTP packets
/// only in `rtp_mode`, which gives up depacketization. The meter refills to each frame's
/// completion, remembers the credit it discarded while full, and refunds a frame what was
/// discarded since its arrival, up to its size. So a frame is credited for its own
/// assembly, a keyframe larger than the burst included, and the meter never closes a
/// publisher that charging packets in arrival order would accept. It errs lenient by at
/// most the cap's rate over a frame's assembly time.
pub(crate) struct Meter {
	cap: Option<Cap>,
	/// Bytes still allowed before the cap plus burst is exceeded.
	tokens: f64,
	last: Instant,
	/// When the bucket sat full, oldest first, kept for [`ASSEMBLY`].
	full: VecDeque<Full>,
}

/// An interval the bucket sat full, discarding credit at `rate` bytes per second.
struct Full {
	since: Instant,
	until: Instant,
	rate: f64,
}

impl Meter {
	pub fn new(cap: Option<Cap>, now: Instant) -> Self {
		Self {
			cap,
			tokens: cap.map_or(0.0, Cap::burst_bytes),
			last: now,
			full: VecDeque::new(),
		}
	}

	pub fn cap(&self) -> Option<Cap> {
		self.cap
	}

	/// Switch to `cap`. Credit earned under the old cap stays, up to the new burst; a
	/// session that was uncapped starts with a full burst.
	pub fn set(&mut self, cap: Option<Cap>, now: Instant) {
		self.refill(now);
		self.tokens = match (self.cap, cap) {
			(_, None) => 0.0,
			(None, Some(cap)) => cap.burst_bytes(),
			(Some(_), Some(cap)) => self.tokens.min(cap.burst_bytes()),
		};
		if cap.is_none() {
			self.full.clear();
		}
		self.cap = cap;
	}

	/// Charge a frame of `bytes` whose first packet arrived at `arrival` and that completed
	/// at `now`, returning the cap it exceeded.
	pub fn charge(&mut self, bytes: usize, arrival: Instant, now: Instant) -> Result<(), Cap> {
		let Some(cap) = self.cap else {
			return Ok(());
		};
		self.refill(now);
		let bytes = bytes as f64;
		// Charged in arrival order, this frame would have kept the bucket below full
		// for the credit discarded since it arrived.
		self.tokens += self.refund(arrival, bytes) - bytes;
		if self.tokens < 0.0 {
			return Err(cap);
		}
		Ok(())
	}

	fn refill(&mut self, now: Instant) {
		// Arrival times can trail a `set` stamped with the current time; never run the clock back.
		let from = self.last;
		let elapsed = now.saturating_duration_since(from);
		self.last = self.last.max(now);
		let Some(cap) = self.cap else {
			return;
		};

		let rate = cap.rate.as_bps() as f64 / 8.0;
		let burst = cap.burst_bytes();
		let tokens = self.tokens + rate * elapsed.as_secs_f64();
		if tokens > burst {
			// The bucket filled partway through and discarded credit from then until now.
			let since = (from + Duration::from_secs_f64((burst - self.tokens).max(0.0) / rate)).min(now);
			match self.full.back_mut() {
				Some(full) if full.until == since && full.rate == rate => full.until = now,
				_ => self.full.push_back(Full {
					since,
					until: now,
					rate,
				}),
			}
		}
		self.tokens = tokens.min(burst);

		if let Some(horizon) = self.last.checked_sub(ASSEMBLY) {
			while self.full.front().is_some_and(|full| full.until < horizon) {
				self.full.pop_front();
			}
		}
	}

	/// Take back up to `limit` bytes of credit discarded after `at`, oldest first: a frame
	/// charged later that arrived later can use only newer credit.
	fn refund(&mut self, at: Instant, limit: f64) -> f64 {
		let mut refund = 0.0;
		let mut i = 0;
		while refund < limit && i < self.full.len() {
			let Full { since, until, rate } = self.full[i];
			if until <= at {
				i += 1;
				continue;
			}
			let start = since.max(at);
			let take = (rate * until.saturating_duration_since(start).as_secs_f64()).min(limit - refund);
			refund += take;
			self.full[i].since = (start + Duration::from_secs_f64(take / rate)).min(until);
			if since < at {
				// The credit discarded before `at` stays for frames that arrived earlier.
				self.full.insert(i, Full { since, until: at, rate });
				i += 1;
			}
			i += 1;
		}
		refund
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn cap() -> Cap {
		// 8 kbps is 1000 bytes per second, so a burst of one second is 1000 bytes.
		Cap::new(Rate::from_kbps(8))
	}

	impl Meter {
		/// Charge a frame that completed as it arrived.
		fn charge_at(&mut self, bytes: usize, at: Instant) -> Result<(), Cap> {
			self.charge(bytes, at, at)
		}
	}

	#[test]
	fn steering_never_exceeds_the_cap() {
		for bps in [0, 1, 8_000, 2_500_000, u64::MAX] {
			let cap = Cap::new(Rate::from_bps(bps));
			assert!(cap.steer() <= cap.rate, "{bps}");
		}
	}

	#[test]
	fn media_under_the_cap_passes() {
		let t0 = Instant::now();
		let mut meter = Meter::new(Some(cap()), t0);
		// 500 bytes every half second is exactly the cap, for a minute.
		for i in 0..120 {
			assert!(meter.charge_at(500, t0 + Duration::from_millis(500 * i)).is_ok(), "{i}");
		}
	}

	#[test]
	fn media_past_the_cap_plus_burst_is_refused() {
		let t0 = Instant::now();
		let mut meter = Meter::new(Some(cap()), t0);
		// The whole burst at once is allowed.
		assert!(meter.charge_at(1000, t0).is_ok());
		// A second later, one more second of the cap is allowed, but no more.
		assert!(meter.charge_at(1000, t0 + Duration::from_secs(1)).is_ok());
		assert_eq!(meter.charge_at(1, t0 + Duration::from_secs(1)), Err(cap()));
	}

	#[test]
	fn a_sustained_overshoot_is_refused_once_the_burst_is_spent() {
		let t0 = Instant::now();
		let mut meter = Meter::new(Some(cap()), t0);
		// 10% over the cap: 550 bytes every half second spends 50 bytes of burst per tick.
		let refused = (1..100).find(|i| meter.charge_at(550, t0 + Duration::from_millis(500 * i)).is_err());
		assert_eq!(refused, Some(11));
	}

	#[test]
	fn idle_time_banks_no_more_than_the_burst() {
		let t0 = Instant::now();
		let mut meter = Meter::new(Some(cap()), t0);
		let later = t0 + Duration::from_secs(3600);
		assert!(meter.charge_at(1000, later).is_ok());
		assert!(meter.charge_at(1, later).is_err());
	}

	#[test]
	fn a_frame_charged_late_is_refunded_the_credit_discarded_since_it_arrived() {
		let t0 = Instant::now();
		let ms = |ms| t0 + Duration::from_millis(ms);

		// A 1000-byte video frame arrives over 0..50 ms with 10-byte audio frames at 20 and
		// 40 ms, which str0m completes first. In arrival order the bucket ends at 30 bytes.
		let mut ordered = Meter::new(Some(cap()), t0);
		for (bytes, at) in [(1000, ms(0)), (10, ms(20)), (10, ms(40)), (0, ms(50))] {
			assert!(ordered.charge_at(bytes, at).is_ok());
		}

		let mut meter = Meter::new(Some(cap()), t0);
		for (bytes, at, done) in [(10, ms(20), ms(20)), (10, ms(40), ms(40)), (1000, ms(0), ms(50))] {
			assert!(meter.charge(bytes, at, done).is_ok());
		}
		assert!(
			(meter.tokens - ordered.tokens).abs() < 1e-6,
			"{} != {}",
			meter.tokens,
			ordered.tokens
		);
		assert!((meter.tokens - 30.0).abs() < 1e-6);
	}

	#[test]
	fn a_frame_is_credited_for_its_own_assembly() {
		let t0 = Instant::now();
		// 2 Mbps with a 100 ms burst holds 25 kB. A 100 kB keyframe over 500 ms is 1.6 Mbps,
		// under the cap, though four times the burst.
		let cap = Cap::new(Rate::from_kbps(2_000)).with_burst(Duration::from_millis(100));
		let mut meter = Meter::new(Some(cap), t0);
		assert!(meter.charge(100_000, t0, t0 + Duration::from_millis(500)).is_ok());

		// The same keyframe over 100 ms is 8 Mbps.
		let mut meter = Meter::new(Some(cap), t0);
		assert_eq!(meter.charge(100_000, t0, t0 + Duration::from_millis(100)), Err(cap));
	}

	#[test]
	fn a_refund_outlives_a_short_burst() {
		let t0 = Instant::now();
		// A 100-byte burst, and a 100-byte video frame from 0 ms that completes after nine
		// 20-byte audio frames at 20..180 ms. Arrival order accepts all of them.
		let mut meter = Meter::new(Some(cap().with_burst(Duration::from_millis(100))), t0);
		for i in 1..10 {
			assert!(meter.charge_at(20, t0 + Duration::from_millis(20 * i)).is_ok(), "{i}");
		}
		assert!(meter.charge(100, t0, t0 + Duration::from_millis(180)).is_ok());
	}

	#[test]
	fn a_refund_leaves_newer_credit_to_frames_that_arrived_later() {
		let t0 = Instant::now();
		let ms = |ms| t0 + Duration::from_millis(ms);
		// Frame A takes 499 bytes at 0 ms and 1 at 1000 ms. Frame B takes 1000 bytes at
		// 500 ms and 499 at 1001 ms. In packet order the bucket ends at 1 byte.
		let mut meter = Meter::new(Some(cap()), t0);
		assert!(meter.charge(500, ms(0), ms(1000)).is_ok());
		// A was refunded the credit discarded over 0..500 ms, which B could not use. B, charged
		// at its first packet, gets the byte its last one paid for back too.
		assert!(meter.charge(1499, ms(500), ms(1001)).is_ok());
		assert!((meter.tokens - 2.0).abs() < 1e-6, "{}", meter.tokens);
	}

	#[test]
	fn a_refund_covers_only_credit_discarded_after_the_frame_arrived() {
		let t0 = Instant::now();
		// Idle for a minute, then a frame at 60 s and one charged late from 10 ms before it.
		let mut meter = Meter::new(Some(cap()), t0);
		assert!(meter.charge_at(10, t0 + Duration::from_secs(60)).is_ok());
		assert!(
			meter
				.charge(1000, t0 + Duration::from_millis(59_990), t0 + Duration::from_secs(60))
				.is_ok()
		);
		// Only the 10 bytes discarded over those 10 ms came back.
		assert!((meter.tokens - 0.0).abs() < 1e-6, "{}", meter.tokens);
	}

	#[test]
	fn discarded_credit_is_refunded_once() {
		let t0 = Instant::now();
		let mut meter = Meter::new(Some(cap()), t0);
		// 100 bytes discarded over 0..100 ms, then two frames from 0 ms charged late.
		assert!(meter.charge_at(0, t0 + Duration::from_millis(100)).is_ok());
		let done = t0 + Duration::from_millis(100);
		assert!(meter.charge(600, t0, done).is_ok());
		assert!(meter.charge(500, t0, done).is_ok());
		assert!((meter.tokens - 0.0).abs() < 1e-6, "{}", meter.tokens);
		assert!(meter.charge(1, t0, done).is_err());
	}

	#[test]
	fn an_uncapped_meter_passes_everything() {
		let t0 = Instant::now();
		let mut meter = Meter::new(None, t0);
		assert!(meter.charge_at(usize::MAX, t0).is_ok());
	}

	#[test]
	fn a_lowered_cap_clamps_the_banked_burst() {
		let t0 = Instant::now();
		let mut meter = Meter::new(Some(cap()), t0);
		let lower = Cap::new(Rate::from_kbps(4));
		meter.set(Some(lower), t0);
		assert!(meter.charge_at(500, t0).is_ok());
		assert_eq!(meter.charge_at(1, t0), Err(lower));
	}

	#[test]
	fn capping_an_uncapped_session_grants_a_full_burst() {
		let t0 = Instant::now();
		let mut meter = Meter::new(None, t0);
		assert!(meter.charge_at(1_000_000, t0).is_ok());
		meter.set(Some(cap()), t0 + Duration::from_secs(1));
		assert!(meter.charge_at(1000, t0 + Duration::from_secs(1)).is_ok());
		assert!(meter.charge_at(1, t0 + Duration::from_secs(1)).is_err());
	}

	#[test]
	fn the_handle_shares_live_updates() {
		let limit = Limit::unlimited();
		let mut rx = limit.clone().subscribe();
		assert_eq!(limit.get(), None);
		limit.clone().set(cap());
		assert!(rx.has_changed().unwrap());
		assert_eq!(*rx.borrow_and_update(), Some(cap()));
		assert_eq!(limit.get(), Some(cap()));
	}
}
