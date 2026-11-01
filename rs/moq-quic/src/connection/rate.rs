//! The token bucket that holds a connection to a [`Rate`](crate::Rate) cap in one direction.

use std::num::NonZeroU64;
use std::time::Duration;

use crate::Instant;

/// How long a cap's rate may be spent at once, after the bucket sat full.
const BURST: Duration = Duration::from_secs(1);

const NANOS_PER_SEC: u128 = 1_000_000_000;

/// Bytes accrue at `rate` up to a burst of [`BURST`] at that rate, and charges spend them.
///
/// A charge may overdraw the bucket, leaving a deficit the rate repays before anything is
/// available again, so a cap enforced by withholding credit can still account for bytes that
/// arrive on credit granted earlier.
#[derive(Debug, Clone)]
pub(crate) struct Bucket {
	/// Bytes per second.
	rate: NonZeroU64,
	burst: u64,
	/// Bytes ever earned, never past `charged + burst`.
	earned: u64,
	/// Bytes ever charged.
	charged: u64,
	/// The instant `earned` is current to, or `None` before the first refill. The bucket starts
	/// full, and a full bucket earns nothing however long it sits, so no instant is needed yet.
	refilled: Option<Instant>,
}

impl Bucket {
	/// A full bucket.
	pub(crate) fn new(rate: NonZeroU64) -> Self {
		let burst = burst(rate);
		Self {
			rate,
			burst,
			earned: burst,
			charged: 0,
			refilled: None,
		}
	}

	/// Change the rate, keeping what was earned up to the new, possibly smaller, burst.
	///
	/// Time since the last refill accrues at the new rate.
	pub(crate) fn set_rate(&mut self, rate: NonZeroU64) {
		self.rate = rate;
		self.burst = burst(rate);
		self.earned = self.earned.min(self.charged.saturating_add(self.burst));
	}

	/// The most the bucket holds.
	pub(crate) fn burst(&self) -> u64 {
		self.burst
	}

	/// Earn what accrued up to `now`.
	///
	/// Call it before every [`Self::charge`], so time the bucket sat full is not credited after
	/// the charge.
	pub(crate) fn refill(&mut self, now: Instant) {
		let full = self.charged.saturating_add(self.burst);
		let Some(prev) = self.refilled.filter(|_| self.earned < full) else {
			self.refilled = Some(now);
			return;
		};
		let elapsed = now.saturating_duration_since(prev).as_nanos();
		let earned = u64::try_from(u128::from(self.rate.get()) * elapsed / NANOS_PER_SEC).unwrap_or(u64::MAX);
		if earned == 0 {
			// Keep the fraction of a byte for the next refill.
			return;
		}
		self.earned = self.earned.saturating_add(earned).min(full);
		self.refilled = match self.earned < full {
			// Advance by exactly the time the whole bytes took, keeping the remainder.
			true => Some(prev + nanos(u128::from(earned) * NANOS_PER_SEC / u128::from(self.rate.get()))),
			false => Some(now),
		};
	}

	/// Spend `bytes`, possibly past what is available.
	pub(crate) fn charge(&mut self, bytes: u64) {
		self.charged = self.charged.saturating_add(bytes);
	}

	/// The bytes available as of the last refill: none while a deficit is repaid.
	pub(crate) fn available(&self) -> u64 {
		self.earned.saturating_sub(self.charged)
	}

	/// When [`Self::available`] reaches `want`, at most the burst, or `None` if it already has.
	pub(crate) fn ready_at(&self, want: u64) -> Option<Instant> {
		debug_assert!(want <= self.burst, "{want} bytes never fit a {} byte burst", self.burst);
		let short = self
			.charged
			.saturating_add(want)
			.checked_sub(self.earned)
			.filter(|&s| s > 0)?;
		// Not refilled yet means full, which holds every `want` up to the burst.
		let refilled = self.refilled?;
		// Round up, so the wait always earns the whole shortfall.
		Some(refilled + nanos((u128::from(short) * NANOS_PER_SEC).div_ceil(u128::from(self.rate.get()))))
	}

	/// When `bytes` may be sent at `now`: `None` for now, or the instant to ask again.
	///
	/// A send larger than the burst waits for a full bucket rather than forever.
	pub(crate) fn delay(&mut self, now: Instant, bytes: u64) -> Option<Instant> {
		self.refill(now);
		self.ready_at(bytes.min(self.burst))
	}
}

fn burst(rate: NonZeroU64) -> u64 {
	u64::try_from(u128::from(rate.get()) * BURST.as_nanos() / NANOS_PER_SEC)
		.unwrap_or(u64::MAX)
		.max(1)
}

fn nanos(nanos: u128) -> Duration {
	Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn rate(bytes: u64) -> NonZeroU64 {
		NonZeroU64::new(bytes).unwrap()
	}

	#[test]
	fn starts_full_and_refills_at_the_rate() {
		let start = Instant::now();
		let mut bucket = Bucket::new(rate(1000));
		assert_eq!(bucket.available(), 1000);
		bucket.refill(start);
		bucket.charge(1000);
		assert_eq!(bucket.available(), 0);
		assert_eq!(bucket.ready_at(250), Some(start + Duration::from_millis(250)));

		bucket.refill(start + Duration::from_millis(250));
		assert_eq!(bucket.available(), 250);
		assert_eq!(bucket.ready_at(250), None);
	}

	#[test]
	fn idle_time_never_banks_more_than_the_burst() {
		let start = Instant::now();
		let mut bucket = Bucket::new(rate(1000));
		bucket.refill(start);
		bucket.refill(start + Duration::from_secs(60));
		assert_eq!(bucket.available(), 1000);
		bucket.charge(1000);
		bucket.refill(start + Duration::from_secs(60));
		assert_eq!(bucket.available(), 0, "time spent full earns nothing after the charge");
	}

	#[test]
	fn a_deficit_is_repaid_before_anything_is_available() {
		let start = Instant::now();
		let mut bucket = Bucket::new(rate(1000));
		bucket.refill(start);
		bucket.charge(1500);
		assert_eq!(bucket.available(), 0);
		assert_eq!(bucket.ready_at(1), Some(start + Duration::from_millis(501)));
		bucket.refill(start + Duration::from_millis(500));
		assert_eq!(bucket.available(), 0);
		bucket.refill(start + Duration::from_millis(600));
		assert_eq!(bucket.available(), 100);
	}

	#[test]
	fn fractions_of_a_byte_carry_over() {
		let start = Instant::now();
		let mut bucket = Bucket::new(rate(3));
		bucket.refill(start);
		bucket.charge(3);
		for ms in (100..=1000).step_by(100) {
			bucket.refill(start + Duration::from_millis(ms));
		}
		assert_eq!(bucket.available(), 3);
	}

	#[test]
	fn lowering_the_rate_drops_what_the_new_burst_cannot_hold() {
		let start = Instant::now();
		let mut bucket = Bucket::new(rate(1000));
		bucket.refill(start);
		bucket.set_rate(rate(100));
		assert_eq!(bucket.burst(), 100);
		assert_eq!(bucket.available(), 100);
	}

	#[test]
	fn a_send_larger_than_the_burst_waits_for_a_full_bucket() {
		let start = Instant::now();
		let mut bucket = Bucket::new(rate(1000));
		assert_eq!(bucket.delay(start, 5000), None);
		bucket.charge(5000);
		let ready = bucket.delay(start, 5000).unwrap();
		assert_eq!(ready, start + Duration::from_secs(5));
		assert_eq!(bucket.delay(ready, 5000), None);
	}
}
