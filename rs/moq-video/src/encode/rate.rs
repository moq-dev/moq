//! Rate control: turning a congestion-control bandwidth estimate into the
//! bitrate the encoder should actually produce at.
//!
//! [`Control`] is the one place this policy lives, so every sender backs off the
//! same way. It's a pure function of the estimate: feed it every value from a
//! [`bandwidth::Consumer`](moq_net::bandwidth::Consumer) and hand what it
//! returns to [`Encoder::set_bitrate`](super::Encoder::set_bitrate).

use std::time::Instant;

/// How a bandwidth estimate maps onto the bitrate a sender should produce at.
///
/// Build one with [`Policy::new`] and override what you need. The defaults are
/// tuned for a live contribution encoder on a cellular uplink: give back
/// bandwidth immediately when the pipe closes, take it back slowly when it
/// opens, and don't twitch at every jitter in the estimate.
///
/// `#[non_exhaustive]`: construct via [`Policy::new`] and set fields, so new
/// knobs stay additive.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Policy {
	/// Fraction of the estimate to target, reserving room for the other tracks
	/// sharing this connection (audio) and for transport overhead. Defaults to
	/// 0.9. Must be greater than 0; values above 1.0 target more than the link
	/// is estimated to carry and are clamped away.
	pub headroom: f64,

	/// Upper bound in bits per second, normally the bitrate the caller asked
	/// for. The estimate can only ever take the target *down* from here: an
	/// optimistic estimate is not a reason to send more than was configured.
	pub max: u64,

	/// Lower bound in bits per second. Below some rate the picture isn't worth
	/// sending, so the target holds here and the transport's priority queue
	/// sheds the excess instead. Defaults to a tenth of `max`.
	pub min: u64,

	/// Ignore moves smaller than this fraction of the current target, so a
	/// jittering estimate doesn't reconfigure the encoder every 100ms.
	/// Defaults to 0.05 (5%).
	pub hysteresis: f64,

	/// How fast the target may climb back, as a fraction of the current target
	/// per second. Defaults to 0.25 (25%/s, so ~3s from the floor back to a 2x
	/// higher rate). Drops ignore this and apply at once: overshooting a closing
	/// uplink costs a stalled picture, while undershooting an opening one costs
	/// only a few seconds of lower quality.
	pub ramp: f64,
}

impl Policy {
	/// A policy targeting at most `max` bits per second, with the documented
	/// defaults for every other knob.
	pub fn new(max: u64) -> Self {
		Self {
			headroom: 0.9,
			max,
			// A tenth of the ceiling: low enough to ride out a bad uplink, high
			// enough that what we do send is still worth decoding.
			min: max / 10,
			hysteresis: 0.05,
			ramp: 0.25,
		}
	}
}

/// Maps bandwidth estimates onto a target bitrate, per a [`Policy`].
///
/// Feed it every estimate from a
/// [`bandwidth::Consumer`](moq_net::bandwidth::Consumer); it returns a new
/// target only when one is worth applying, so a caller can hand the result
/// straight to an encoder without rate-limiting it further:
///
/// ```
/// # use moq_video::encode::rate::{Control, Policy};
/// # use std::time::Instant;
/// let mut control = Control::new(Policy::new(4_000_000));
/// // A 2 Mbps estimate takes the 4 Mbps target down to 2 Mbps * 0.9 headroom.
/// assert_eq!(control.update(Some(2_000_000), Instant::now()), Some(1_800_000));
/// ```
///
/// The time source is a parameter rather than an [`Instant::now`] call so the
/// policy stays pure and testable. Pass the time the estimate was observed.
#[derive(Clone, Debug)]
pub struct Control {
	policy: Policy,
	target: u64,
	/// When the target last moved, anchoring the [`Policy::ramp`] limit. `None`
	/// until the first change, when there's nothing to ramp from.
	applied: Option<Instant>,
}

impl Control {
	/// Start at [`Policy::max`], the optimistic case: until an estimate says
	/// otherwise, send what the caller configured.
	pub fn new(policy: Policy) -> Self {
		Self {
			target: policy.max.max(policy.min),
			policy,
			applied: None,
		}
	}

	/// The current target in bits per second.
	pub fn target(&self) -> u64 {
		self.target
	}

	/// Feed a new estimate, returning the new target when it moved enough to be
	/// worth applying and `None` when it didn't.
	///
	/// A `None` estimate (no congestion controller, or disconnected) holds the
	/// current target rather than resetting to [`Policy::max`]: losing the
	/// estimate is not evidence the uplink got better.
	pub fn update(&mut self, estimate: Option<u64>, now: Instant) -> Option<u64> {
		let estimate = estimate?;

		// Normalize here rather than trusting the fields: `min > max` would make
		// the clamp below panic, and a non-finite headroom would poison the cast.
		let min = self.policy.min.min(self.policy.max);
		let headroom = if self.policy.headroom.is_finite() {
			self.policy.headroom.clamp(0.0, 1.0)
		} else {
			0.0
		};

		let desired = ((estimate as f64 * headroom) as u64).clamp(min, self.policy.max);

		let next = if desired <= self.target {
			// Attack: the pipe is closing, give the bandwidth back now.
			desired
		} else {
			// Decay: climb back at no more than `ramp` per second since the last
			// change. Before the first change there's nothing to ramp from.
			match self.applied {
				Some(applied) => {
					let elapsed = now.saturating_duration_since(applied).as_secs_f64();
					let ramp = self.policy.ramp.max(0.0);
					let grown = self.target as f64 * (1.0 + ramp * elapsed);
					(grown as u64).min(desired).clamp(min, self.policy.max)
				}
				None => desired,
			}
		};

		// Hysteresis is checked against the *applied* target and deliberately
		// does not touch `applied` when it suppresses a move. The ramp allowance
		// therefore keeps growing while small raises are suppressed, so a raise
		// lands once it clears the threshold instead of being starved forever by
		// a per-tick allowance smaller than the threshold.
		let hysteresis = self.policy.hysteresis.max(0.0);
		if (next.abs_diff(self.target) as f64) < self.target as f64 * hysteresis {
			return None;
		}

		self.target = next;
		self.applied = Some(now);
		Some(next)
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;

	/// 4 Mbps ceiling, so the 0.9 headroom and the max/10 floor land on round
	/// numbers: 400 kbps floor, and an estimate of E targets 0.9 * E.
	fn control() -> Control {
		Control::new(Policy::new(4_000_000))
	}

	#[test]
	fn starts_optimistic() {
		assert_eq!(control().target(), 4_000_000);
	}

	#[test]
	fn drop_applies_immediately_with_headroom() {
		let mut control = control();
		// A 2 Mbps pipe: target 90% of it at once, no ramp, no waiting.
		assert_eq!(control.update(Some(2_000_000), Instant::now()), Some(1_800_000));
		assert_eq!(control.target(), 1_800_000);
	}

	#[test]
	fn missing_estimate_holds_the_target() {
		let mut control = control();
		let now = Instant::now();
		control.update(Some(2_000_000), now).unwrap();

		// Losing the estimate (disconnected) is not evidence the uplink is
		// healthy again, so the target must not jump back to max.
		assert_eq!(control.update(None, now + Duration::from_secs(10)), None);
		assert_eq!(control.target(), 1_800_000);
	}

	#[test]
	fn estimate_never_raises_above_max() {
		let mut control = control();
		// A wildly optimistic estimate is not licence to exceed what was configured.
		assert_eq!(control.update(Some(100_000_000), Instant::now()), None);
		assert_eq!(control.target(), 4_000_000);
	}

	#[test]
	fn target_never_falls_below_min() {
		let mut control = control();
		// A near-dead uplink floors at min (max/10) rather than chasing to zero.
		assert_eq!(control.update(Some(1), Instant::now()), Some(400_000));
		assert_eq!(control.target(), 400_000);
	}

	#[test]
	fn raise_is_ramp_limited() {
		let mut control = control();
		let start = Instant::now();
		control.update(Some(1_000_000), start).unwrap(); // target 900k

		// The pipe reopens to 4 Mbps. One second later the default 25%/s ramp
		// allows only 900k -> 1125k, not the full 3.6 Mbps the estimate wants.
		let raised = control.update(Some(4_000_000), start + Duration::from_secs(1)).unwrap();
		assert_eq!(raised, 1_125_000);
	}

	#[test]
	fn raise_eventually_reaches_the_estimate() {
		let mut control = control();
		let start = Instant::now();
		control.update(Some(1_000_000), start).unwrap(); // target 900k

		// Feed a steady healthy estimate every 100ms; the ramp should walk the
		// target up to the full 90% of it and then stop.
		for tick in 1..=200 {
			control.update(Some(4_000_000), start + Duration::from_millis(100 * tick));
		}
		assert_eq!(control.target(), 3_600_000);
	}

	/// Regression: the ramp allowance per tick (25%/s * 100ms = 2.5%) is smaller
	/// than the hysteresis threshold (5%), so a raise is suppressed on any single
	/// tick. Suppression must not reset the ramp anchor, or the allowance would be
	/// recomputed from `now` every tick, never clear the threshold, and the target
	/// would be starved at the floor forever while the uplink sat idle.
	#[test]
	fn suppressed_raises_do_not_starve_the_ramp() {
		let mut control = control();
		let start = Instant::now();
		control.update(Some(1_000_000), start).unwrap(); // target 900k

		// Tick at 100ms: each tick alone is under the 5% threshold.
		let mut raised = None;
		for tick in 1..=10 {
			if let Some(next) = control.update(Some(4_000_000), start + Duration::from_millis(100 * tick)) {
				raised = Some((tick, next));
				break;
			}
		}

		let (tick, next) = raised.expect("a raise must eventually clear hysteresis");
		// 5% of 900k needs 0.05/0.25 = 0.2s of ramp, i.e. the tick at 200ms.
		assert_eq!(tick, 2);
		assert_eq!(next, 945_000);
	}

	#[test]
	fn small_moves_are_suppressed() {
		let mut control = control();
		let now = Instant::now();
		control.update(Some(2_000_000), now).unwrap(); // target 1.8M

		// 2% under the current target: inside the 5% deadband, so no reconfigure.
		assert_eq!(control.update(Some(1_960_000), now + Duration::from_secs(1)), None);
		assert_eq!(control.target(), 1_800_000);

		// 20% under: outside the deadband, so it applies.
		assert_eq!(
			control.update(Some(1_600_000), now + Duration::from_secs(2)),
			Some(1_440_000)
		);
	}

	/// `min > max` is a caller error, but it must clamp rather than panic: the
	/// bound is fed straight to `clamp`, which panics on an inverted range.
	#[test]
	fn inverted_bounds_do_not_panic() {
		let mut policy = Policy::new(1_000_000);
		policy.min = 5_000_000;
		let mut control = Control::new(policy);
		control.update(Some(2_000_000), Instant::now());
		assert!(control.target() <= 5_000_000);
	}

	/// A non-finite headroom would make the `as u64` cast produce garbage rather
	/// than a rate, so it's normalized away.
	#[test]
	fn non_finite_headroom_does_not_poison_the_target() {
		let mut policy = Policy::new(4_000_000);
		policy.headroom = f64::NAN;
		let mut control = Control::new(policy);
		control.update(Some(2_000_000), Instant::now());
		assert_eq!(control.target(), 400_000); // floored, not NaN-cast to 0
	}
}
