//! Rate control: turning a congestion-control bandwidth estimate into the
//! bitrate the encoder should actually produce at.
//!
//! [`Control`] is the one place this policy lives, so every sender backs off the
//! same way. It's a pure function of the estimate: feed it every value from a
//! [`bandwidth::Consumer`] and hand what it
//! returns to [`Encoder::set_bitrate`](super::Encoder::set_bitrate).

use std::time::Instant;

use moq_net::bandwidth;

/// Ignore moves smaller than this fraction of the current target, so a jittering
/// estimate doesn't reconfigure the encoder on every 100ms sample.
const HYSTERESIS: f64 = 0.05;

/// How fast the target may climb back, as a fraction of the current target per second
/// (~3s from the floor back to a 2x higher rate).
///
/// Drops ignore this and apply at once. Overshooting a closing uplink costs a stalled
/// picture, while undershooting an opening one costs a few seconds of lower quality,
/// so the response is deliberately asymmetric.
const RAMP: f64 = 0.25;

/// How a bandwidth estimate maps onto the bitrate a sender should produce at.
///
/// Build one with [`Policy::new`]. The behaviour is tuned for a live contribution
/// encoder on a cellular uplink: give back bandwidth immediately when the pipe closes,
/// take it back slowly when it opens, and don't twitch at every jitter in the estimate.
/// The deadband and ramp that implement that are deliberately not knobs; they are
/// properties of how congestion control behaves, not of any one sender.
///
/// The estimate handed in is expected to be this sender's alone. Splitting one
/// connection's estimate among the senders sharing it is
/// [`bandwidth::Allocator`]'s job, one layer down,
/// so nothing here holds a fraction back for anyone else.
///
/// `#[non_exhaustive]`: construct via [`Policy::new`] and set fields, so new
/// knobs stay additive.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Policy {
	/// Upper bound, normally the bitrate the caller asked for. The estimate can only
	/// ever take the target *down* from here: an optimistic estimate is not a reason
	/// to send more than was configured.
	pub max: bandwidth::Rate,

	/// Lower bound. Below some rate the picture isn't worth sending, so the target
	/// holds here and the transport's priority queue sheds the excess instead.
	/// Defaults to a tenth of `max`.
	pub min: bandwidth::Rate,
}

impl Policy {
	/// A policy targeting at most `max`, with a floor a tenth of it.
	pub fn new(max: bandwidth::Rate) -> Self {
		Self {
			max,
			// A tenth of the ceiling: low enough to ride out a bad uplink, high
			// enough that what we do send is still worth decoding.
			min: bandwidth::Rate::from_bps(max.as_bps() / 10),
		}
	}
}

/// Maps bandwidth estimates onto a target bitrate, per a [`Policy`].
///
/// Feed it every estimate from a
/// [`bandwidth::Consumer`]; it returns a new
/// target only when one is worth applying, so a caller can hand the result
/// straight to an encoder without rate-limiting it further:
///
/// ```
/// # use moq_video::encode::rate::{Control, Policy};
/// # use moq_net::bandwidth;
/// # use std::time::Instant;
/// let mut control = Control::new(Policy::new(bandwidth::Rate::from_mbps(4)));
/// // A 2 Mbps estimate takes the 4 Mbps target down to what the link will carry.
/// assert_eq!(
///     control.update(Some(bandwidth::Rate::from_mbps(2)), Instant::now()),
///     Some(bandwidth::Rate::from_mbps(2))
/// );
/// ```
///
/// The time source is a parameter rather than an [`Instant::now`] call so the
/// policy stays pure and testable. Pass the time the estimate was observed.
#[derive(Clone, Debug)]
pub struct Control {
	policy: Policy,
	target: bandwidth::Rate,
	/// When the target last moved, anchoring the [`RAMP`] limit. `None`
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

	/// The current target bitrate.
	pub fn target(&self) -> bandwidth::Rate {
		self.target
	}

	/// Feed a new estimate, returning the new target when it moved enough to be
	/// worth applying and `None` when it didn't.
	///
	/// A `None` estimate (no congestion controller, or disconnected) holds the
	/// current target rather than resetting to [`Policy::max`]: losing the
	/// estimate is not evidence the uplink got better.
	pub fn update(&mut self, estimate: Option<bandwidth::Rate>, now: Instant) -> Option<bandwidth::Rate> {
		let estimate = estimate?;

		// Normalize rather than trusting the fields: `min > max` would make the
		// clamp below panic.
		let min = self.policy.min.min(self.policy.max);
		let desired = estimate.clamp(min, self.policy.max);

		let next = if desired <= self.target {
			// Attack: the pipe is closing, give the bandwidth back now.
			desired
		} else {
			// Decay: climb back at no more than `ramp` per second since the last
			// change. Before the first change there's nothing to ramp from.
			match self.applied {
				Some(applied) => {
					let elapsed = now.saturating_duration_since(applied).as_secs_f64();
					let grown = self.target.scaled(1.0 + RAMP * elapsed);
					grown.min(desired).clamp(min, self.policy.max)
				}
				None => desired,
			}
		};

		// Hysteresis is checked against the *applied* target and deliberately
		// does not touch `applied` when it suppresses a move. The ramp allowance
		// therefore keeps growing while small raises are suppressed, so a raise
		// lands once it clears the threshold instead of being starved forever by
		// a per-tick allowance smaller than the threshold.
		//
		// A raise landing exactly on [`Policy::max`] is exempt: that's the last step
		// of a recovery, not a twitch. `next` stops growing once it reaches the
		// ceiling, so a target arriving within `hysteresis` of it has no move left
		// that could ever clear the threshold, and the encoder would sit a few
		// percent under its configured rate for good after one congestion event.
		//
		// Scoped to the *configured* ceiling, not to any `desired`. Every raise
		// eventually lands on `desired`, so exempting all of them would let a
		// slowly-rising estimate reconfigure the encoder on every tick, which is
		// precisely what the deadband exists to prevent. Stalling a few percent
		// below a merely estimate-limited ceiling is the deadband working; stalling
		// below the rate the caller asked for is not.
		//
		// Drops keep the deadband either way: sitting a little above a falling
		// estimate is what it's for.
		let settling = next > self.target && next == self.policy.max;
		if !settling && next.abs_diff(self.target) < self.target.scaled(HYSTERESIS) {
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

	/// Bits per second, so the tables below stay readable.
	fn bps(bps: u64) -> bandwidth::Rate {
		bandwidth::Rate::from_bps(bps)
	}

	/// 4 Mbps ceiling, so the max/10 floor lands on a round 400 kbps.
	fn control() -> Control {
		Control::new(Policy::new(bps(4_000_000)))
	}

	#[test]
	fn starts_optimistic() {
		assert_eq!(control().target(), bps(4_000_000));
	}

	#[test]
	fn drop_applies_immediately() {
		let mut control = control();
		// A 2 Mbps pipe: take the target down to it at once, no ramp, no waiting.
		assert_eq!(
			control.update(Some(bps(2_000_000)), Instant::now()),
			Some(bps(2_000_000))
		);
		assert_eq!(control.target(), bps(2_000_000));
	}

	#[test]
	fn missing_estimate_holds_the_target() {
		let mut control = control();
		let now = Instant::now();
		control.update(Some(bps(2_000_000)), now).unwrap();

		// Losing the estimate (disconnected) is not evidence the uplink is
		// healthy again, so the target must not jump back to max.
		assert_eq!(control.update(None, now + Duration::from_secs(10)), None);
		assert_eq!(control.target(), bps(2_000_000));
	}

	#[test]
	fn estimate_never_raises_above_max() {
		let mut control = control();
		// A wildly optimistic estimate is not licence to exceed what was configured.
		assert_eq!(control.update(Some(bps(100_000_000)), Instant::now()), None);
		assert_eq!(control.target(), bps(4_000_000));
	}

	#[test]
	fn target_never_falls_below_min() {
		let mut control = control();
		// A near-dead uplink floors at min (max/10) rather than chasing to zero.
		assert_eq!(control.update(Some(bps(1)), Instant::now()), Some(bps(400_000)));
		assert_eq!(control.target(), bps(400_000));
	}

	#[test]
	fn raise_is_ramp_limited() {
		let mut control = control();
		let start = Instant::now();
		control.update(Some(bps(1_000_000)), start).unwrap(); // target 1M

		// The pipe reopens to 4 Mbps. One second later the default 25%/s ramp
		// allows only 1M -> 1.25M, not the full 4 Mbps the estimate wants.
		let raised = control
			.update(Some(bps(4_000_000)), start + Duration::from_secs(1))
			.unwrap();
		assert_eq!(raised, bps(1_250_000));
	}

	#[test]
	fn raise_eventually_reaches_the_estimate() {
		let mut control = control();
		let start = Instant::now();
		control.update(Some(bps(1_000_000)), start).unwrap(); // target 1M

		// Feed a steady healthy estimate every 100ms; the ramp should walk the
		// target back up to the ceiling and then stop.
		for tick in 1..=200 {
			control.update(Some(bps(4_000_000)), start + Duration::from_millis(100 * tick));
		}
		assert_eq!(control.target(), bps(4_000_000));
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
		control.update(Some(bps(1_000_000)), start).unwrap(); // target 1M

		// Tick at 100ms: each tick alone is under the 5% threshold.
		let mut raised = None;
		for tick in 1..=10 {
			if let Some(next) = control.update(Some(bps(4_000_000)), start + Duration::from_millis(100 * tick)) {
				raised = Some((tick, next));
				break;
			}
		}

		let (tick, next) = raised.expect("a raise must eventually clear hysteresis");
		// 5% of 1M needs 0.05/0.25 = 0.2s of ramp, i.e. the tick at 200ms.
		assert_eq!(tick, 2);
		assert_eq!(next, bps(1_050_000));
	}

	/// Regression: the ramp stops growing `next` once it reaches the ceiling, so a
	/// target that lands within the 5% deadband of it has no move left that can
	/// clear hysteresis. Without the exemption for a raise that reaches `desired`,
	/// the walk above stalls at 3_866_256 and the encoder never returns to the
	/// bitrate it was configured with.
	#[test]
	fn a_raise_reaching_the_ceiling_beats_hysteresis() {
		let mut control = control();
		let start = Instant::now();
		control.update(Some(bps(1_000_000)), start).unwrap();

		// Walk up until it settles, then confirm where it settled.
		let mut last = None;
		for tick in 1..=200 {
			if let Some(next) = control.update(Some(bps(4_000_000)), start + Duration::from_millis(100 * tick)) {
				last = Some(next);
			}
		}

		assert_eq!(last, Some(bps(4_000_000)));
		// And it stops there rather than reapplying the same value forever.
		assert_eq!(
			control.update(Some(bps(4_000_000)), start + Duration::from_secs(60)),
			None
		);
	}

	/// Regression: the ceiling exemption above must not swallow the deadband. Every
	/// raise eventually lands on `desired`, so keying it on that rather than on the
	/// configured ceiling let a slowly-rising estimate retune the encoder on every
	/// single tick, which is the exact behavior `hysteresis` exists to prevent.
	#[test]
	fn upward_jitter_stays_inside_the_deadband() {
		let mut control = control();
		let start = Instant::now();
		control.update(Some(bps(2_000_000)), start).unwrap(); // target 2M, well under the 4M ceiling

		// A 0.5% rise: inside the 5% deadband, and nowhere near the ceiling.
		assert_eq!(
			control.update(Some(bps(2_010_000)), start + Duration::from_millis(100)),
			None
		);

		// A whole walk of them still doesn't, rather than one reconfigure per tick.
		// Stops short of 2.1M, which is exactly 5% up and so clears the threshold.
		for tick in 2..=9 {
			let estimate = 2_000_000 + tick * 10_000;
			assert_eq!(
				control.update(Some(bps(estimate)), start + Duration::from_millis(100 * tick)),
				None,
				"estimate {estimate} is inside the deadband"
			);
		}
		assert_eq!(control.target(), bps(2_000_000));

		// And the deadband is still only a deadband: once the estimate does clear it,
		// the raise lands normally, without needing the ceiling exemption.
		assert_eq!(
			control.update(Some(bps(2_200_000)), start + Duration::from_secs(2)),
			Some(bps(2_200_000))
		);
	}

	#[test]
	fn small_moves_are_suppressed() {
		let mut control = control();
		let now = Instant::now();
		control.update(Some(bps(2_000_000)), now).unwrap(); // target 2M

		// 2% under the current target: inside the 5% deadband, so no reconfigure.
		assert_eq!(control.update(Some(bps(1_960_000)), now + Duration::from_secs(1)), None);
		assert_eq!(control.target(), bps(2_000_000));

		// 20% under: outside the deadband, so it applies.
		assert_eq!(
			control.update(Some(bps(1_600_000)), now + Duration::from_secs(2)),
			Some(bps(1_600_000))
		);
	}

	/// `min > max` is a caller error, but it must clamp rather than panic: the
	/// bound is fed straight to `clamp`, which panics on an inverted range.
	#[test]
	fn inverted_bounds_do_not_panic() {
		let mut policy = Policy::new(bps(1_000_000));
		policy.min = bps(5_000_000);
		let mut control = Control::new(policy);
		control.update(Some(bps(2_000_000)), Instant::now());
		assert!(control.target() <= bps(5_000_000));
	}
}
