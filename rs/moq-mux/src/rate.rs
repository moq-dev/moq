//! Shared rate control for media senders.
//!
//! [`Control`] turns each grant from a [`bandwidth::Consumer`] into the target
//! rate a sender should produce, so audio, video, and transcode back off the
//! same way. [`Follower`] drives it from a live grant.

use std::{
	pin::Pin,
	task::{Poll, ready},
	time::Duration,
};

use moq_clock::Instant;

use moq_net::bandwidth;

/// Ignore moves smaller than 1/`HYSTERESIS` (5%) of the current target, so a
/// jittering estimate doesn't reconfigure the sender on every 100ms sample.
const HYSTERESIS: u64 = 20;

/// How long the target takes to climb by its own size: 25% of the current target per
/// second (~3s from the floor back to a 2x higher rate).
///
/// Drops ignore this and apply at once. Overshooting a closing uplink stalls media,
/// while undershooting an opening one costs a few seconds of lower quality,
/// so the response is deliberately asymmetric.
const RAMP: Duration = Duration::from_secs(4);

/// The soonest a failed change is retried: the time the ramp takes to clear the
/// hysteresis band, so a sender that keeps failing is asked at most this often.
const RETRY: Duration = Duration::from_millis(RAMP.as_millis() as u64 / HYSTERESIS);

/// How a bandwidth estimate maps onto the bitrate a sender should produce at.
///
/// Build one with [`Policy::new`]. The behaviour is tuned for a live contribution
/// sender on a cellular uplink: give back bandwidth immediately when the pipe closes,
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

	/// Lower bound. Below some rate the media isn't worth sending, so the target
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
/// straight to a sender without rate-limiting it further:
///
/// ```
/// # use moq_mux::rate::{Control, Policy};
/// # use moq_net::bandwidth;
/// # use moq_clock::Instant;
/// let mut control = Control::new(Policy::new(bandwidth::Rate::from_mbps(4)));
/// // A 2 Mbps estimate takes the 4 Mbps target down to what the link will carry.
/// assert_eq!(
///     control.update(Some(bandwidth::Rate::from_mbps(2)), moq_clock::now()),
///     Some(bandwidth::Rate::from_mbps(2))
/// );
/// ```
///
/// A raise climbs over several updates, and a grant that has settled sends no
/// further estimate to climb on, so a sender following a live grant uses
/// [`Follower`], which also wakes when the ramp allows the next step.
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
	/// When an update with the same estimate will move the target. `None` when
	/// only a new estimate can.
	deadline: Option<Instant>,
	/// What the change [`update`](Self::update) last returned replaced, so
	/// [`failed`](Self::failed) can take it back.
	last: Option<Change>,
}

/// The state a returned change replaced, and the clamped estimate that asked for it.
#[derive(Clone, Copy, Debug)]
struct Change {
	target: bandwidth::Rate,
	applied: Option<Instant>,
	desired: bandwidth::Rate,
}

impl Control {
	/// Start at [`Policy::max`], the optimistic case: until an estimate says
	/// otherwise, send what the caller configured.
	pub fn new(policy: Policy) -> Self {
		Self {
			target: policy.max,
			policy,
			applied: None,
			deadline: None,
			last: None,
		}
	}

	/// The current target bitrate.
	pub fn target(&self) -> bandwidth::Rate {
		self.target
	}

	/// When to [`update`](Self::update) again with the latest estimate, changed or
	/// not, to continue a raise or retry a failed change; `None` once the target has
	/// settled.
	///
	/// An update at this instant with an unchanged estimate always returns a change.
	fn deadline(&self) -> Option<Instant> {
		self.deadline
	}

	/// Feed a new estimate, returning the new target when it moved enough to be
	/// worth applying and `None` when it didn't.
	///
	/// A `None` estimate (no congestion controller, or disconnected) holds the
	/// current target rather than resetting to [`Policy::max`]: losing the
	/// estimate is not evidence the uplink got better.
	pub fn update(&mut self, estimate: Option<bandwidth::Rate>, now: Instant) -> Option<bandwidth::Rate> {
		self.last = None;
		let Some(estimate) = estimate else {
			self.deadline = None;
			return None;
		};

		// Normalize rather than trusting the fields: `min > max` would make the
		// clamp below panic.
		let min = self.policy.min.min(self.policy.max);
		let desired = estimate.clamp(min, self.policy.max);

		let next = if desired <= self.target {
			// Attack: the pipe is closing, give the bandwidth back now.
			desired
		} else {
			// Decay: climb back at no more than `RAMP` allows since the last
			// change. Before the first change there's nothing to ramp from.
			match self.applied {
				Some(applied) => grown(self.target, now.saturating_duration_since(applied))
					.min(desired)
					.clamp(min, self.policy.max),
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
		// that could ever clear the threshold, and the sender would sit a few
		// percent under its configured rate for good after one congestion event.
		//
		// Scoped to the *configured* ceiling, not to any `desired`. Every raise
		// eventually lands on `desired`, so exempting all of them would let a
		// slowly-rising estimate reconfigure the sender on every tick, which is
		// precisely what the deadband exists to prevent. Stalling a few percent
		// below a merely estimate-limited ceiling is the deadband working; stalling
		// below the rate the caller asked for is not.
		//
		// Drops keep the deadband either way: sitting a little above a falling
		// estimate is what it's for.
		let settling = next > self.target && next == self.policy.max;
		if next == self.target || (!settling && next.abs_diff(self.target) < band(self.target)) {
			self.deadline = self.raise(desired);
			return None;
		}

		self.last = Some(Change {
			target: self.target,
			applied: self.applied,
			desired,
		});
		self.target = next;
		self.applied = Some(now);
		self.deadline = self.raise(desired);
		Some(next)
	}

	/// Take back the target the last [`update`](Self::update) returned, because the
	/// sender failed to apply it at `now`.
	///
	/// The previous target is restored, and [`deadline`](Self::deadline) asks for the
	/// change again no sooner than one ramp step after `now`, so a quiet grant still
	/// retries and a sender that keeps failing is not hammered. Does nothing when the
	/// last update returned no change.
	fn failed(&mut self, now: Instant) {
		let Some(change) = self.last.take() else { return };
		// The old anchor too, so the retry keeps the ramp allowance it had earned,
		// the same as a raise suppressed by the deadband.
		self.target = change.target;
		self.applied = change.applied;
		let retry = now.checked_add(RETRY);
		self.deadline = if change.desired < self.target {
			// A returned drop already cleared the deadband, and drops don't ramp.
			retry
		} else {
			self.raise(change.desired).zip(retry).map(|(at, retry)| at.max(retry))
		};
	}

	/// The earliest instant the ramp lets the target climb toward `desired` by enough
	/// to apply, or `None` when it never will without a new estimate.
	///
	/// Exact, in the same integer math as [`grown`]: an update at this instant applies
	/// the raise, so a caller sleeping until then never wakes for nothing and spins.
	fn raise(&self, desired: bandwidth::Rate) -> Option<Instant> {
		if desired <= self.target {
			return None;
		}

		// The smallest raise that clears the deadband, or the configured ceiling,
		// which is exempt from it.
		let target = self.target.as_bps();
		let goal = target.saturating_add(band(self.target).as_bps().max(1));
		let goal = if desired == self.policy.max {
			goal.min(desired.as_bps())
		} else if goal <= desired.as_bps() {
			goal
		} else {
			// The raise is capped at `desired`, inside the deadband: only a new
			// estimate can move it.
			return None;
		};

		// A zero target has nothing to grow from.
		if target == 0 {
			return None;
		}
		let nanos = (u128::from(goal - target) * RAMP.as_nanos()).div_ceil(u128::from(target));
		self.applied?
			.checked_add(Duration::from_nanos(u64::try_from(nanos).ok()?))
	}
}

/// Follows a bandwidth grant through a [`Control`] on the ambient clock.
///
/// Wakes on every change to the grant and whenever the ramp allows the next step of
/// a raise or the retry of a refused change, so a sender climbs back to its ceiling
/// even after the grant goes quiet.
pub struct Follower {
	grant: bandwidth::Consumer,
	control: Control,
	/// Armed at the control's deadline, created on first use.
	timer: Option<moq_clock::Sleep>,
	/// Feed the current grant on the next poll, after a [`restart`](Self::restart).
	refeed: bool,
}

impl Follower {
	/// Follow `grant` under `policy`, starting at [`Policy::max`].
	pub fn new(grant: bandwidth::Consumer, policy: Policy) -> Self {
		Self {
			grant,
			control: Control::new(policy),
			timer: None,
			refeed: false,
		}
	}

	/// The policy the target follows.
	pub fn policy(&self) -> &Policy {
		&self.control.policy
	}

	/// Start over under `policy`, as a fresh [`Follower`] at [`Policy::max`], and feed
	/// the current grant on the next poll.
	pub fn restart(&mut self, policy: Policy) {
		self.control = Control::new(policy);
		self.refeed = true;
	}

	/// Poll for the next target the sender should produce at, or the error that
	/// closed the grant.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<bandwidth::Rate, moq_net::Error>> {
		loop {
			let estimate = if std::mem::take(&mut self.refeed) {
				self.grant.peek()
			} else if let Poll::Ready(estimate) = self.grant.poll_changed(waiter) {
				estimate?
			} else {
				// Due only when an update with the unchanged grant moves the target, so
				// this never spins.
				ready!(self.poll_deadline(waiter));
				self.grant.peek()
			};
			if let Some(target) = self.control.update(estimate, moq_clock::now()) {
				return Poll::Ready(Ok(target));
			}
		}
	}

	/// The next target the sender should produce at, or the error that closed the grant.
	pub async fn next(&mut self) -> Result<bandwidth::Rate, moq_net::Error> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Take back the target [`next`](Self::next) last returned, because the sender
	/// failed to apply it.
	///
	/// The previous target is restored and asked for again no sooner than one ramp
	/// step from now, so a quiet grant still retries and a sender that keeps failing
	/// is not hammered. Does nothing once another poll has fed the grant.
	pub fn failed(&mut self) {
		self.control.failed(moq_clock::now());
	}

	fn poll_deadline(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let Some(at) = self.control.deadline() else {
			self.timer = None;
			return Poll::Pending;
		};
		let timer = self.timer.get_or_insert_with(|| moq_clock::sleep_until(at));
		if timer.deadline() != at {
			timer.reset(at);
		}
		waiter.poll_future(Pin::new(timer))
	}
}

/// The hysteresis band around `target`: moves smaller than this are suppressed.
fn band(target: bandwidth::Rate) -> bandwidth::Rate {
	bandwidth::Rate::from_bps(target.as_bps() / HYSTERESIS)
}

/// `target` plus the [`RAMP`] allowance for `elapsed`, saturating.
fn grown(target: bandwidth::Rate, elapsed: Duration) -> bandwidth::Rate {
	let target = target.as_bps();
	let allowance = u128::from(target) * elapsed.as_nanos() / RAMP.as_nanos();
	bandwidth::Rate::from_bps(target.saturating_add(u64::try_from(allowance).unwrap_or(u64::MAX)))
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
	fn drop_applies_immediately() {
		let mut control = control();
		// A 2 Mbps pipe: take the target down to it at once, no ramp, no waiting.
		assert_eq!(
			control.update(Some(bps(2_000_000)), moq_clock::now()),
			Some(bps(2_000_000))
		);
		assert_eq!(control.target(), bps(2_000_000));
	}

	#[test]
	fn missing_estimate_holds_the_target() {
		let mut control = control();
		let now = moq_clock::now();
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
		assert_eq!(control.update(Some(bps(100_000_000)), moq_clock::now()), None);
		assert_eq!(control.target(), bps(4_000_000));
	}

	#[test]
	fn target_never_falls_below_min() {
		let mut control = control();
		// A near-dead uplink floors at min (max/10) rather than chasing to zero.
		assert_eq!(control.update(Some(bps(1)), moq_clock::now()), Some(bps(400_000)));
		assert_eq!(control.target(), bps(400_000));
	}

	#[test]
	fn raise_is_ramp_limited() {
		let mut control = control();
		let start = moq_clock::now();
		control.update(Some(bps(1_000_000)), start).unwrap(); // target 1M

		// The pipe reopens to 4 Mbps. One second later the default 25%/s ramp
		// allows only 1M -> 1.25M, not the full 4 Mbps the estimate wants.
		let raised = control
			.update(Some(bps(4_000_000)), start + Duration::from_secs(1))
			.unwrap();
		assert_eq!(raised, bps(1_250_000));
	}

	/// Regression: the ramp allowance per tick (25%/s * 100ms = 2.5%) is smaller
	/// than the hysteresis threshold (5%), so a raise is suppressed on any single
	/// tick. Suppression must not reset the ramp anchor, or the allowance would be
	/// recomputed from `now` every tick, never clear the threshold, and the target
	/// would be starved at the floor forever while the uplink sat idle.
	#[test]
	fn suppressed_raises_do_not_starve_the_ramp() {
		let mut control = control();
		let start = moq_clock::now();
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
	/// the walk below stalls at 3_866_256 and the sender never returns to the
	/// bitrate it was configured with.
	#[test]
	fn a_raise_reaching_the_ceiling_beats_hysteresis() {
		let mut control = control();
		let start = moq_clock::now();
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
	/// configured ceiling let a slowly-rising estimate retune the sender on every
	/// single tick, which is the exact behavior `hysteresis` exists to prevent.
	#[test]
	fn upward_jitter_stays_inside_the_deadband() {
		let mut control = control();
		let start = moq_clock::now();
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
		let now = moq_clock::now();
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

	/// `min > max` is normalized to the ceiling at construction and update.
	#[test]
	fn inverted_bounds_use_the_ceiling() {
		let mut policy = Policy::new(bps(1_000_000));
		policy.min = bps(5_000_000);
		let mut control = Control::new(policy);

		assert_eq!(control.target(), bps(1_000_000));
		assert_eq!(control.update(Some(bps(2_000_000)), moq_clock::now()), None);
		assert_eq!(control.target(), bps(1_000_000));
	}

	/// Feed `estimate` at every deadline until the target settles, asserting each one
	/// applies a change. Returns the targets applied and when the last one landed.
	fn follow(control: &mut Control, estimate: bandwidth::Rate) -> (Vec<bandwidth::Rate>, Option<Instant>) {
		let mut applied = Vec::new();
		let mut last = None;
		while let Some(at) = control.deadline() {
			assert!(applied.len() < 1_000, "the deadline never settled: {applied:?}");
			let next = control.update(Some(estimate), at);
			let next = next.unwrap_or_else(|| panic!("a deadline at {at:?} applied nothing after {applied:?}"));
			applied.push(next);
			last = Some(at);
		}
		(applied, last)
	}

	/// After an attack, the target climbs back to the ceiling on deadlines alone: the
	/// grant restored a second later never changes again.
	#[test]
	fn a_quiet_grant_recovers_on_deadlines() {
		let mut control = control();
		let start = moq_clock::now();
		control.update(Some(bps(1_000_000)), start).unwrap();
		assert_eq!(control.deadline(), None, "a drop lands at once");

		let recovered = start + Duration::from_secs(1);
		assert_eq!(control.update(Some(bps(4_000_000)), recovered), Some(bps(1_250_000)));
		let (applied, last) = follow(&mut control, bps(4_000_000));

		assert_eq!(applied.last(), Some(&bps(4_000_000)));
		assert!(applied.windows(2).all(|pair| pair[0] < pair[1]), "{applied:?}");
		// 1.25M to 4M at 25%/s compounding with each 5% step is about 4.7s.
		let took = last.unwrap().duration_since(recovered);
		assert!(
			took > Duration::from_secs(4) && took < Duration::from_secs(6),
			"{took:?}"
		);
	}

	/// The deadline holds for awkward rates too, where integer rounding or a zero-width
	/// band could leave a deadline that applies nothing and spins its caller.
	#[test]
	fn every_deadline_applies_a_change() {
		let start = moq_clock::now();
		for max in [15, 20, 21, 999, 1_000_003, 4_000_000, 1 << 50] {
			for floor in [1, max / 10, max / 3, max - max / 25] {
				let mut control = Control::new(Policy::new(bps(max)));
				control.update(Some(bps(floor)), start);
				let at = start + Duration::from_millis(7);
				control.update(Some(bps(max)), at);
				let (applied, _) = follow(&mut control, bps(max));
				assert_eq!(control.target(), bps(max), "{max} from {floor}: {applied:?}");
			}
		}
	}

	/// A raise that reaches the estimate stops asking.
	#[test]
	fn reaching_the_estimate_clears_the_deadline() {
		let mut control = control();
		let start = moq_clock::now();
		control.update(Some(bps(1_000_000)), start).unwrap();
		assert_eq!(
			control.update(Some(bps(1_100_000)), start + Duration::from_secs(1)),
			Some(bps(1_100_000))
		);
		assert_eq!(control.deadline(), None);
	}

	/// A climb toward an estimate below the ceiling settles within the deadband of it,
	/// which only a new estimate can clear, and stops asking there.
	#[test]
	fn a_climb_below_the_ceiling_settles_in_the_deadband() {
		let mut control = control();
		let start = moq_clock::now();
		control.update(Some(bps(1_000_000)), start).unwrap();
		control
			.update(Some(bps(2_000_000)), start + Duration::from_secs(1))
			.unwrap();
		assert!(control.deadline().is_some());

		let (applied, _) = follow(&mut control, bps(2_000_000));
		let settled = *applied.last().unwrap();
		assert!(settled <= bps(2_000_000) && bps(2_000_000).abs_diff(settled) < band(settled));
	}

	/// A remainder inside the deadband below the ceiling can never clear it, so it is
	/// not scheduled, whether it is all of the raise or what one step leaves over.
	#[test]
	fn a_raise_inside_the_deadband_is_not_scheduled() {
		let start = moq_clock::now();
		let mut whole = control();
		whole.update(Some(bps(2_000_000)), start).unwrap();
		assert_eq!(whole.update(Some(bps(2_050_000)), start + Duration::from_secs(1)), None);
		assert_eq!(whole.deadline(), None);

		// 1M to 1.3M: one second allows 1.25M, leaving 50k under the 62.5k band.
		let mut remainder = control();
		remainder.update(Some(bps(1_000_000)), start).unwrap();
		assert_eq!(
			remainder.update(Some(bps(1_300_000)), start + Duration::from_secs(1)),
			Some(bps(1_250_000))
		);
		assert_eq!(remainder.deadline(), None);
	}

	/// Losing the estimate or a drop cancels a scheduled raise.
	#[test]
	fn a_missing_estimate_or_a_drop_clears_the_deadline() {
		let start = moq_clock::now();
		let scheduled = || {
			let mut control = control();
			control.update(Some(bps(1_000_000)), start).unwrap();
			control
				.update(Some(bps(4_000_000)), start + Duration::from_secs(1))
				.unwrap();
			assert!(control.deadline().is_some());
			control
		};

		let mut control = scheduled();
		assert_eq!(control.update(None, start + Duration::from_secs(2)), None);
		assert_eq!(control.deadline(), None);

		let mut control = scheduled();
		assert_eq!(
			control.update(Some(bps(800_000)), start + Duration::from_secs(2)),
			Some(bps(800_000))
		);
		assert_eq!(control.deadline(), None);
	}

	/// A raise the sender refused is taken back and retried no sooner than one ramp
	/// step later, even on the final step to the ceiling, which is otherwise due early.
	#[test]
	fn a_failed_raise_is_retried() {
		let mut control = control();
		let start = moq_clock::now();
		control.update(Some(bps(1_000_000)), start).unwrap();
		control
			.update(Some(bps(4_000_000)), start + Duration::from_secs(1))
			.unwrap();
		let (applied, last) = follow(&mut control, bps(4_000_000));
		assert_eq!(applied.last(), Some(&bps(4_000_000)));
		let before = applied[applied.len() - 2];

		let failed = last.unwrap() + Duration::from_millis(1);
		control.failed(failed);
		assert_eq!(control.target(), before, "the previous target is restored");
		let retry = control.deadline().expect("the failed raise is retried");
		assert!(
			retry >= failed + RETRY,
			"{retry:?} is sooner than one step after {failed:?}"
		);

		// A second report has nothing left to take back.
		control.failed(failed + Duration::from_secs(1));
		assert_eq!(control.target(), before);
		assert_eq!(control.deadline(), Some(retry));

		assert_eq!(control.update(Some(bps(4_000_000)), retry), Some(bps(4_000_000)));
		assert_eq!(control.deadline(), None);
	}

	/// A drop the sender refused is retried too: the sender is still above the grant.
	#[test]
	fn a_failed_drop_is_retried() {
		let mut control = control();
		let start = moq_clock::now();
		control.update(Some(bps(1_000_000)), start).unwrap();
		control.failed(start);
		assert_eq!(control.target(), bps(4_000_000));
		assert_eq!(control.deadline(), Some(start + RETRY));
		assert_eq!(
			control.update(Some(bps(1_000_000)), start + RETRY),
			Some(bps(1_000_000))
		);
		assert_eq!(control.deadline(), None);
	}

	/// Only the change the last update returned can be taken back.
	#[test]
	fn a_failure_after_no_change_does_nothing() {
		let mut control = control();
		let start = moq_clock::now();
		control.update(Some(bps(2_000_000)), start).unwrap();
		assert_eq!(
			control.update(Some(bps(1_960_000)), start + Duration::from_secs(1)),
			None
		);
		control.failed(start + Duration::from_secs(1));
		assert_eq!(control.target(), bps(2_000_000));
		assert_eq!(control.deadline(), None);
	}

	/// The grant recovers once and never changes again, and the sender refuses the final
	/// raise once: the follower still wakes on its own and carries the target to the ceiling.
	#[tokio::test(start_paused = true)]
	async fn a_follower_recovers_a_quiet_grant() {
		let grant = bandwidth::Producer::new();
		let mut follower = Follower::new(grant.consume(), Policy::new(bps(4_000_000)));

		grant.set(Some(bps(1_000_000))).unwrap();
		assert_eq!(follower.next().await.unwrap(), bps(1_000_000));

		tokio::time::advance(Duration::from_secs(1)).await;
		grant.set(Some(bps(4_000_000))).unwrap();

		let mut applied = Vec::new();
		let mut refused = false;
		// A minute without a wake is far past anything the ramp still needs.
		while let Ok(target) = moq_clock::timeout(Duration::from_secs(60), follower.next()).await {
			let target = target.unwrap();
			if target == bps(4_000_000) && !refused {
				refused = true;
				follower.failed();
				continue;
			}
			applied.push(target);
		}
		assert!(refused, "the final raise was never asked for: {applied:?}");
		assert_eq!(applied.last(), Some(&bps(4_000_000)), "{applied:?}");
		assert!(applied.windows(2).all(|pair| pair[0] < pair[1]), "{applied:?}");
	}

	/// A restart feeds the unchanged grant through the new policy at once.
	#[tokio::test(start_paused = true)]
	async fn a_restart_feeds_the_current_grant() {
		let grant = bandwidth::Producer::new();
		let mut follower = Follower::new(grant.consume(), Policy::new(bps(4_000_000)));
		grant.set(Some(bps(3_000_000))).unwrap();
		assert_eq!(follower.next().await.unwrap(), bps(3_000_000));

		follower.restart(Policy::new(bps(2_000_000)));
		assert_eq!(follower.policy().max, bps(2_000_000));
		// The 3 Mbps grant no longer fits under the new 2 Mbps ceiling, which is where a
		// restart starts, so nothing moves until the grant does.
		assert!(
			moq_clock::timeout(Duration::from_secs(60), follower.next())
				.await
				.is_err()
		);

		follower.restart(Policy::new(bps(4_000_000)));
		assert_eq!(follower.next().await.unwrap(), bps(3_000_000));
	}

	/// A closed grant ends the follower with the reason.
	#[tokio::test(start_paused = true)]
	async fn a_closed_grant_ends_the_follower() {
		let grant = bandwidth::Producer::new();
		let mut follower = Follower::new(grant.consume(), Policy::new(bps(4_000_000)));
		drop(grant);
		assert!(follower.next().await.is_err());
	}
}
