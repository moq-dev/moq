//! Bitrate caps for WHIP ingest.
//!
//! An embedder hands [`whip::accept`](crate::whip::accept) a [`Limit`], and moq-rtc steers the publisher's encoder under its [`Cap`]: a `b=TIAS`
//! line in the SDP answer, and periodic REMB where the publisher negotiated
//! it. WebRTC has no receiver backpressure, so a publisher that ignores both is metered
//! and closed with [`Error::RateLimited`](crate::Error::RateLimited) past the cap plus its
//! burst.

use std::sync::Arc;
use std::time::Duration;

use moq_clock::Instant;
use moq_net::bandwidth::Rate;
use tokio::sync::watch;

/// The share of the cap moq-rtc steers the encoder to.
///
/// The meter counts every media payload, but the steering lines only bound what the
/// browser's video encoder targets: audio rides on top, and encoders overshoot their
/// target around keyframes. Steering to the cap itself would let an honest publisher
/// drift past it and be closed, so the steered rate leaves a tenth of the cap for both.
const STEER: f64 = 0.9;

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
pub(crate) struct Meter {
	cap: Option<Cap>,
	/// Bytes still allowed before the cap plus burst is exceeded.
	tokens: f64,
	last: Instant,
}

impl Meter {
	pub fn new(cap: Option<Cap>, now: Instant) -> Self {
		Self {
			cap,
			tokens: cap.map_or(0.0, Cap::burst_bytes),
			last: now,
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
		self.cap = cap;
	}

	/// Charge `bytes` of media arriving at `now`, returning the cap it exceeded.
	pub fn charge(&mut self, bytes: usize, now: Instant) -> Result<(), Cap> {
		let Some(cap) = self.cap else {
			return Ok(());
		};
		self.refill(now);
		self.tokens -= bytes as f64;
		if self.tokens < 0.0 {
			return Err(cap);
		}
		Ok(())
	}

	fn refill(&mut self, now: Instant) {
		// Arrival times can trail a `set` stamped with the current time; never run the clock back.
		let elapsed = now.saturating_duration_since(self.last);
		self.last = self.last.max(now);
		if let Some(cap) = self.cap {
			let earned = cap.rate.as_bps() as f64 / 8.0 * elapsed.as_secs_f64();
			self.tokens = (self.tokens + earned).min(cap.burst_bytes());
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn cap() -> Cap {
		// 8 kbps is 1000 bytes per second, so a burst of one second is 1000 bytes.
		Cap::new(Rate::from_kbps(8))
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
			assert!(meter.charge(500, t0 + Duration::from_millis(500 * i)).is_ok(), "{i}");
		}
	}

	#[test]
	fn media_past_the_cap_plus_burst_is_refused() {
		let t0 = Instant::now();
		let mut meter = Meter::new(Some(cap()), t0);
		// The whole burst at once is allowed.
		assert!(meter.charge(1000, t0).is_ok());
		// A second later, one more second of the cap is allowed, but no more.
		assert!(meter.charge(1000, t0 + Duration::from_secs(1)).is_ok());
		assert_eq!(meter.charge(1, t0 + Duration::from_secs(1)), Err(cap()));
	}

	#[test]
	fn a_sustained_overshoot_is_refused_once_the_burst_is_spent() {
		let t0 = Instant::now();
		let mut meter = Meter::new(Some(cap()), t0);
		// 10% over the cap: 550 bytes every half second spends 50 bytes of burst per tick.
		let refused = (1..100).find(|i| meter.charge(550, t0 + Duration::from_millis(500 * i)).is_err());
		assert_eq!(refused, Some(11));
	}

	#[test]
	fn idle_time_banks_no_more_than_the_burst() {
		let t0 = Instant::now();
		let mut meter = Meter::new(Some(cap()), t0);
		let later = t0 + Duration::from_secs(3600);
		assert!(meter.charge(1000, later).is_ok());
		assert!(meter.charge(1, later).is_err());
	}

	#[test]
	fn an_uncapped_meter_passes_everything() {
		let t0 = Instant::now();
		let mut meter = Meter::new(None, t0);
		assert!(meter.charge(usize::MAX, t0).is_ok());
	}

	#[test]
	fn a_lowered_cap_clamps_the_banked_burst() {
		let t0 = Instant::now();
		let mut meter = Meter::new(Some(cap()), t0);
		let lower = Cap::new(Rate::from_kbps(4));
		meter.set(Some(lower), t0);
		assert!(meter.charge(500, t0).is_ok());
		assert_eq!(meter.charge(1, t0), Err(lower));
	}

	#[test]
	fn capping_an_uncapped_session_grants_a_full_burst() {
		let t0 = Instant::now();
		let mut meter = Meter::new(None, t0);
		assert!(meter.charge(1_000_000, t0).is_ok());
		meter.set(Some(cap()), t0 + Duration::from_secs(1));
		assert!(meter.charge(1000, t0 + Duration::from_secs(1)).is_ok());
		assert!(meter.charge(1, t0 + Duration::from_secs(1)).is_err());
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
