//! Shared rate control for media senders.
//!
//! [`Control`] turns each grant from a [`bandwidth::Consumer`] into the target
//! rate a sender should produce, so audio, video, and transcode back off the
//! same way. [`Follower`] drives it from a live grant. A rendition encoded beside
//! lower ones of the same source sets [`Policy::lower`], and stops once its grant
//! can no longer fund it, so the lower renditions keep their share.

use std::{
	pin::Pin,
	task::{Poll, ready},
	time::Duration,
};

use kio::clock::Instant;

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
#[derive(Clone, Debug, PartialEq, Eq)]
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

	/// The next lower rendition's [`max`](Self::max), when this one is encoded beside
	/// lower renditions of the same source.
	///
	/// A grant below the band boundary `(max + 2 * lower) / 3` disables this rendition,
	/// since the lower one then serves viewers better than a starved copy of this one.
	/// It resumes at the boundary once the grant clears it by the same 5% hysteresis
	/// every move needs. It only disables while a session probes for it (see
	/// [`Control::set_probed`]), since nothing else could raise the estimate to resume it.
	///
	/// `None`, the default, for the lowest or only rendition, which never disables. A
	/// `lower` at or above `max` leaves no band, so it never disables either.
	pub lower: Option<bandwidth::Rate>,
}

impl Policy {
	/// A policy targeting at most `max`, with a floor a tenth of it.
	pub fn new(max: bandwidth::Rate) -> Self {
		Self {
			max,
			// A tenth of the ceiling: low enough to ride out a bad uplink, high
			// enough that what we do send is still worth decoding.
			min: bandwidth::Rate::from_bps(max.as_bps() / 10),
			lower: None,
		}
	}

	/// The band boundary [`lower`](Self::lower) sets, below which this rendition
	/// disables, or `None` when it never does.
	fn boundary(&self) -> Option<bandwidth::Rate> {
		let lower = self.lower.filter(|lower| *lower < self.max)?;
		let boundary = (u128::from(self.max.as_bps()) + 2 * u128::from(lower.as_bps())) / 3;
		// Below `max`, which fits a `u64`.
		Some(bandwidth::Rate::from_bps(boundary as u64))
	}
}

/// What a sender should be doing, per its [`Control`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
	/// Encode at this rate, and advertise the rendition as enabled.
	Enabled(bandwidth::Rate),
	/// Stop encoding, and advertise the rendition as disabled. Only a [`Policy`] with a
	/// [`lower`](Policy::lower) rendition ever asks for this.
	Disabled,
}

/// Maps bandwidth estimates onto a target bitrate, per a [`Policy`].
///
/// Feed it every estimate from a
/// [`bandwidth::Consumer`]; it returns a new
/// target only when one is worth applying, so a caller can hand the result
/// straight to a sender without rate-limiting it further:
///
/// ```
/// # use moq_mux::rate::{Control, Policy, Target};
/// # use moq_net::bandwidth;
/// # use kio::clock::Instant;
/// let mut control = Control::new(Policy::new(bandwidth::Rate::from_mbps(4)));
/// // A 2 Mbps estimate takes the 4 Mbps target down to what the link will carry.
/// assert_eq!(
///     control.update(Some(bandwidth::Rate::from_mbps(2)), kio::clock::now()),
///     Some(Target::Enabled(bandwidth::Rate::from_mbps(2)))
/// );
///
/// // A 5 Mbps rendition above a 2.5 Mbps one disables below 3.33 Mbps, once a
/// // session probes for it.
/// let mut policy = Policy::new(bandwidth::Rate::from_mbps(5));
/// policy.lower = Some(bandwidth::Rate::from_kbps(2_500));
/// let mut control = Control::new(policy);
/// control.set_probed(true);
/// assert_eq!(
///     control.update(Some(bandwidth::Rate::from_mbps(3)), kio::clock::now()),
///     Some(Target::Disabled)
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
	/// Whether the sender should encode at all; see [`Policy::lower`].
	enabled: bool,
	/// Whether a session probes for this sender, which disabling needs.
	probed: bool,
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
	enabled: bool,
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
			enabled: true,
			probed: false,
			applied: None,
			deadline: None,
			last: None,
		}
	}

	/// What the sender should currently be doing.
	pub fn target(&self) -> Target {
		match self.enabled {
			true => Target::Enabled(self.target),
			false => Target::Disabled,
		}
	}

	/// Whether a session probes the connection for this sender, as
	/// [`Consumer::probed`](bandwidth::Consumer::probed) reports it, from the next
	/// [`update`](Self::update) on.
	///
	/// A rendition only disables while one does: without a sender probing for it,
	/// the estimate stops rising once the connection is app-limited, so a stopped
	/// rendition could never resume. One disabled when probing stops resumes at the
	/// next estimate instead, following the grant down like any sender. `false`
	/// until set.
	pub fn set_probed(&mut self, probed: bool) {
		self.probed = probed;
	}

	/// Follow `policy` from the next [`update`](Self::update), returning whether it changed;
	/// see [`Follower::set_policy`].
	fn set_policy(&mut self, policy: Policy) -> bool {
		if policy == self.policy {
			return false;
		}
		if policy.max == self.policy.max && policy.min == self.policy.min {
			self.policy = policy;
		} else {
			let (enabled, probed) = (self.enabled, self.probed);
			*self = Control::new(policy);
			self.enabled = enabled;
			self.probed = probed;
		}
		true
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
	pub fn update(&mut self, estimate: Option<bandwidth::Rate>, now: Instant) -> Option<Target> {
		self.last = None;
		let Some(estimate) = estimate else {
			self.deadline = None;
			return None;
		};

		// Normalize rather than trusting the fields: `min > max` would make the
		// clamp below panic.
		let min = self.policy.min.min(self.policy.max);
		let desired = estimate.clamp(min, self.policy.max);
		let boundary = self.policy.boundary().filter(|_| self.probed);

		if !self.enabled {
			let resume = match boundary {
				Some(boundary) => {
					// Capped at the ceiling, or a boundary within the deadband of it could
					// never be cleared by an estimate clamped to that ceiling.
					let clear = boundary
						.as_bps()
						.saturating_add(band(boundary).as_bps())
						.min(self.policy.max.as_bps());
					if desired.as_bps() < clear {
						// A retry of a failed resume is no longer due either.
						self.deadline = None;
						return None;
					}
					// Restart at the boundary and ramp: the estimate was measured while
					// this rendition sent nothing, so its whole share is the least
					// trustworthy number here.
					boundary.clamp(min, self.policy.max)
				}
				// Nothing probes for it any more, so it can't wait for the estimate to
				// rise: follow the grant like any sender instead.
				None => desired,
			};
			self.last = Some(Change {
				target: self.target,
				enabled: false,
				applied: self.applied,
				desired,
			});
			self.enabled = true;
			self.target = resume;
			self.applied = Some(now);
			self.deadline = self.raise(desired);
			return Some(Target::Enabled(resume));
		}

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
		// estimate is what it's for. That is also what keeps a grant jittering
		// around the band boundary from flapping the rendition off and on.
		//
		// The deadband only holds a rung on above a boundary that was already holding
		// it. One whose applied target is under the boundary, because probing just
		// started or the lower rendition rose, disables at once on a grant below it.
		let disable = boundary.is_some_and(|boundary| desired < boundary);
		let under = boundary.is_some_and(|boundary| self.target < boundary);
		let settling = next > self.target && next == self.policy.max;
		let suppressed = next == self.target || (!settling && next.abs_diff(self.target) < band(self.target));
		if suppressed && !(disable && under) {
			self.deadline = self.raise(desired);
			return None;
		}

		self.last = Some(Change {
			target: self.target,
			enabled: true,
			applied: self.applied,
			desired,
		});
		// On `desired` rather than `next`: a ramp still climbing from under the boundary
		// toward a grant above it is funded, so it stays on.
		if disable {
			// Only a new estimate clearing the boundary resumes it, so nothing is due.
			self.enabled = false;
			self.deadline = None;
			return Some(Target::Disabled);
		}
		self.target = next;
		self.applied = Some(now);
		self.deadline = self.raise(desired);
		Some(Target::Enabled(next))
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
		let toggled = self.enabled != change.enabled;
		self.target = change.target;
		self.enabled = change.enabled;
		self.applied = change.applied;
		let retry = now.checked_add(RETRY);
		self.deadline = if toggled || change.desired < self.target {
			// A returned drop already cleared the deadband, and drops don't ramp. A
			// returned disable or resume already cleared its boundary.
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
///
/// Follow a [`Reservation`](bandwidth::Reservation)'s consumer: it reports whether a
/// session probes for it, which a rendition needs before it disables, and the slice it
/// would get back while unwatched, which it needs to resume after its viewers left.
pub struct Follower {
	grant: bandwidth::Consumer,
	control: Control,
	/// Armed at the control's deadline, created on first use.
	timer: Option<kio::clock::Sleep>,
	/// Feed the current grant on the next poll, after [`set_policy`](Self::set_policy)
	/// changed it.
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

	/// Follow under `policy` from now on, feeding it the current grant on the next poll.
	///
	/// A new ceiling or floor starts over at its [`Policy::max`], as a fresh [`Follower`]
	/// would, since the sender applied that ceiling itself. A new
	/// [`lower`](Policy::lower) alone keeps the target and ramp, since nothing retuned
	/// the sender. Either way a disabled sender stays disabled until that grant resumes
	/// it under the new policy, since nothing told it to start.
	pub fn set_policy(&mut self, policy: Policy) {
		if self.control.set_policy(policy) {
			self.refeed = true;
		}
	}

	/// Poll for the next target the sender should produce at, or the error that
	/// closed the grant.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Target, moq_net::Error>> {
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
			self.control.set_probed(self.grant.probed());
			if let Some(target) = self.control.update(estimate, kio::clock::now()) {
				return Poll::Ready(Ok(target));
			}
		}
	}

	/// The next target the sender should produce at, or the error that closed the grant.
	pub async fn next(&mut self) -> Result<Target, moq_net::Error> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Take back the target [`next`](Self::next) last returned, because the sender
	/// failed to apply it.
	///
	/// The previous target is restored and asked for again no sooner than one ramp
	/// step from now, so a quiet grant still retries and a sender that keeps failing
	/// is not hammered. Does nothing once another poll has fed the grant.
	pub fn failed(&mut self) {
		self.control.failed(kio::clock::now());
	}

	fn poll_deadline(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let Some(at) = self.control.deadline() else {
			self.timer = None;
			return Poll::Pending;
		};
		let timer = self.timer.get_or_insert_with(|| kio::clock::sleep_until(at));
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

	/// An enabled target at `rate`.
	fn on(rate: bandwidth::Rate) -> Target {
		Target::Enabled(rate)
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
			control.update(Some(bps(2_000_000)), kio::clock::now()),
			Some(on(bps(2_000_000)))
		);
		assert_eq!(control.target(), on(bps(2_000_000)));
	}

	#[test]
	fn missing_estimate_holds_the_target() {
		let mut control = control();
		let now = kio::clock::now();
		control.update(Some(bps(2_000_000)), now).unwrap();

		// Losing the estimate (disconnected) is not evidence the uplink is
		// healthy again, so the target must not jump back to max.
		assert_eq!(control.update(None, now + Duration::from_secs(10)), None);
		assert_eq!(control.target(), on(bps(2_000_000)));
	}

	#[test]
	fn estimate_never_raises_above_max() {
		let mut control = control();
		// A wildly optimistic estimate is not licence to exceed what was configured.
		assert_eq!(control.update(Some(bps(100_000_000)), kio::clock::now()), None);
		assert_eq!(control.target(), on(bps(4_000_000)));
	}

	#[test]
	fn target_never_falls_below_min() {
		let mut control = control();
		// A near-dead uplink floors at min (max/10) rather than chasing to zero.
		assert_eq!(control.update(Some(bps(1)), kio::clock::now()), Some(on(bps(400_000))));
		assert_eq!(control.target(), on(bps(400_000)));
	}

	#[test]
	fn raise_is_ramp_limited() {
		let mut control = control();
		let start = kio::clock::now();
		control.update(Some(bps(1_000_000)), start).unwrap(); // target 1M

		// The pipe reopens to 4 Mbps. One second later the default 25%/s ramp
		// allows only 1M -> 1.25M, not the full 4 Mbps the estimate wants.
		let raised = control
			.update(Some(bps(4_000_000)), start + Duration::from_secs(1))
			.unwrap();
		assert_eq!(raised, on(bps(1_250_000)));
	}

	/// Regression: the ramp allowance per tick (25%/s * 100ms = 2.5%) is smaller
	/// than the hysteresis threshold (5%), so a raise is suppressed on any single
	/// tick. Suppression must not reset the ramp anchor, or the allowance would be
	/// recomputed from `now` every tick, never clear the threshold, and the target
	/// would be starved at the floor forever while the uplink sat idle.
	#[test]
	fn suppressed_raises_do_not_starve_the_ramp() {
		let mut control = control();
		let start = kio::clock::now();
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
		assert_eq!(next, on(bps(1_050_000)));
	}

	/// Regression: the ramp stops growing `next` once it reaches the ceiling, so a
	/// target that lands within the 5% deadband of it has no move left that can
	/// clear hysteresis. Without the exemption for a raise that reaches `desired`,
	/// the walk below stalls at 3_866_256 and the sender never returns to the
	/// bitrate it was configured with.
	#[test]
	fn a_raise_reaching_the_ceiling_beats_hysteresis() {
		let mut control = control();
		let start = kio::clock::now();
		control.update(Some(bps(1_000_000)), start).unwrap();

		// Walk up until it settles, then confirm where it settled.
		let mut last = None;
		for tick in 1..=200 {
			if let Some(next) = control.update(Some(bps(4_000_000)), start + Duration::from_millis(100 * tick)) {
				last = Some(next);
			}
		}

		assert_eq!(last, Some(on(bps(4_000_000))));
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
		let start = kio::clock::now();
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
		assert_eq!(control.target(), on(bps(2_000_000)));

		// And the deadband is still only a deadband: once the estimate does clear it,
		// the raise lands normally, without needing the ceiling exemption.
		assert_eq!(
			control.update(Some(bps(2_200_000)), start + Duration::from_secs(2)),
			Some(on(bps(2_200_000)))
		);
	}

	#[test]
	fn small_moves_are_suppressed() {
		let mut control = control();
		let now = kio::clock::now();
		control.update(Some(bps(2_000_000)), now).unwrap(); // target 2M

		// 2% under the current target: inside the 5% deadband, so no reconfigure.
		assert_eq!(control.update(Some(bps(1_960_000)), now + Duration::from_secs(1)), None);
		assert_eq!(control.target(), on(bps(2_000_000)));

		// 20% under: outside the deadband, so it applies.
		assert_eq!(
			control.update(Some(bps(1_600_000)), now + Duration::from_secs(2)),
			Some(on(bps(1_600_000)))
		);
	}

	/// `min > max` is normalized to the ceiling at construction and update.
	#[test]
	fn inverted_bounds_use_the_ceiling() {
		let mut policy = Policy::new(bps(1_000_000));
		policy.min = bps(5_000_000);
		let mut control = Control::new(policy);

		assert_eq!(control.target(), on(bps(1_000_000)));
		assert_eq!(control.update(Some(bps(2_000_000)), kio::clock::now()), None);
		assert_eq!(control.target(), on(bps(1_000_000)));
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
			let Target::Enabled(next) = next else {
				panic!("a deadline at {at:?} disabled the sender after {applied:?}")
			};
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
		let start = kio::clock::now();
		control.update(Some(bps(1_000_000)), start).unwrap();
		assert_eq!(control.deadline(), None, "a drop lands at once");

		let recovered = start + Duration::from_secs(1);
		assert_eq!(
			control.update(Some(bps(4_000_000)), recovered),
			Some(on(bps(1_250_000)))
		);
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
		let start = kio::clock::now();
		for max in [15, 20, 21, 999, 1_000_003, 4_000_000, 1 << 50] {
			for floor in [1, max / 10, max / 3, max - max / 25] {
				let mut control = Control::new(Policy::new(bps(max)));
				control.update(Some(bps(floor)), start);
				let at = start + Duration::from_millis(7);
				control.update(Some(bps(max)), at);
				let (applied, _) = follow(&mut control, bps(max));
				assert_eq!(control.target(), on(bps(max)), "{max} from {floor}: {applied:?}");
			}
		}
	}

	/// A raise that reaches the estimate stops asking.
	#[test]
	fn reaching_the_estimate_clears_the_deadline() {
		let mut control = control();
		let start = kio::clock::now();
		control.update(Some(bps(1_000_000)), start).unwrap();
		assert_eq!(
			control.update(Some(bps(1_100_000)), start + Duration::from_secs(1)),
			Some(on(bps(1_100_000)))
		);
		assert_eq!(control.deadline(), None);
	}

	/// A climb toward an estimate below the ceiling settles within the deadband of it,
	/// which only a new estimate can clear, and stops asking there.
	#[test]
	fn a_climb_below_the_ceiling_settles_in_the_deadband() {
		let mut control = control();
		let start = kio::clock::now();
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
		let start = kio::clock::now();
		let mut whole = control();
		whole.update(Some(bps(2_000_000)), start).unwrap();
		assert_eq!(whole.update(Some(bps(2_050_000)), start + Duration::from_secs(1)), None);
		assert_eq!(whole.deadline(), None);

		// 1M to 1.3M: one second allows 1.25M, leaving 50k under the 62.5k band.
		let mut remainder = control();
		remainder.update(Some(bps(1_000_000)), start).unwrap();
		assert_eq!(
			remainder.update(Some(bps(1_300_000)), start + Duration::from_secs(1)),
			Some(on(bps(1_250_000)))
		);
		assert_eq!(remainder.deadline(), None);
	}

	/// Losing the estimate or a drop cancels a scheduled raise.
	#[test]
	fn a_missing_estimate_or_a_drop_clears_the_deadline() {
		let start = kio::clock::now();
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
			Some(on(bps(800_000)))
		);
		assert_eq!(control.deadline(), None);
	}

	/// A raise the sender refused is taken back and retried no sooner than one ramp
	/// step later, even on the final step to the ceiling, which is otherwise due early.
	#[test]
	fn a_failed_raise_is_retried() {
		let mut control = control();
		let start = kio::clock::now();
		control.update(Some(bps(1_000_000)), start).unwrap();
		control
			.update(Some(bps(4_000_000)), start + Duration::from_secs(1))
			.unwrap();
		let (applied, last) = follow(&mut control, bps(4_000_000));
		assert_eq!(applied.last(), Some(&bps(4_000_000)));
		let before = applied[applied.len() - 2];

		let failed = last.unwrap() + Duration::from_millis(1);
		control.failed(failed);
		assert_eq!(control.target(), on(before), "the previous target is restored");
		let retry = control.deadline().expect("the failed raise is retried");
		assert!(
			retry >= failed + RETRY,
			"{retry:?} is sooner than one step after {failed:?}"
		);

		// A second report has nothing left to take back.
		control.failed(failed + Duration::from_secs(1));
		assert_eq!(control.target(), on(before));
		assert_eq!(control.deadline(), Some(retry));

		assert_eq!(control.update(Some(bps(4_000_000)), retry), Some(on(bps(4_000_000))));
		assert_eq!(control.deadline(), None);
	}

	/// A drop the sender refused is retried too: the sender is still above the grant.
	#[test]
	fn a_failed_drop_is_retried() {
		let mut control = control();
		let start = kio::clock::now();
		control.update(Some(bps(1_000_000)), start).unwrap();
		control.failed(start);
		assert_eq!(control.target(), on(bps(4_000_000)));
		assert_eq!(control.deadline(), Some(start + RETRY));
		assert_eq!(
			control.update(Some(bps(1_000_000)), start + RETRY),
			Some(on(bps(1_000_000)))
		);
		assert_eq!(control.deadline(), None);
	}

	/// Only the change the last update returned can be taken back.
	#[test]
	fn a_failure_after_no_change_does_nothing() {
		let mut control = control();
		let start = kio::clock::now();
		control.update(Some(bps(2_000_000)), start).unwrap();
		assert_eq!(
			control.update(Some(bps(1_960_000)), start + Duration::from_secs(1)),
			None
		);
		control.failed(start + Duration::from_secs(1));
		assert_eq!(control.target(), on(bps(2_000_000)));
		assert_eq!(control.deadline(), None);
	}

	/// The grant recovers once and never changes again, and the sender refuses the final
	/// raise once: the follower still wakes on its own and carries the target to the ceiling.
	#[tokio::test(start_paused = true)]
	async fn a_follower_recovers_a_quiet_grant() {
		let grant = bandwidth::Producer::new();
		let mut follower = Follower::new(grant.consume(), Policy::new(bps(4_000_000)));

		grant.set(Some(bps(1_000_000))).unwrap();
		assert_eq!(follower.next().await.unwrap(), on(bps(1_000_000)));

		tokio::time::advance(Duration::from_secs(1)).await;
		grant.set(Some(bps(4_000_000))).unwrap();

		let mut applied = Vec::new();
		let mut refused = false;
		// A minute without a wake is far past anything the ramp still needs.
		while let Ok(target) = kio::clock::timeout(Duration::from_secs(60), follower.next()).await {
			let Target::Enabled(target) = target.unwrap() else {
				panic!("a policy without a lower rendition disabled the sender")
			};
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

	/// A new policy feeds the unchanged grant through it at once, and setting the same
	/// policy again changes nothing.
	#[tokio::test(start_paused = true)]
	async fn a_new_policy_feeds_the_current_grant() {
		let grant = bandwidth::Producer::new();
		let mut follower = Follower::new(grant.consume(), Policy::new(bps(4_000_000)));
		grant.set(Some(bps(3_000_000))).unwrap();
		assert_eq!(follower.next().await.unwrap(), on(bps(3_000_000)));

		follower.set_policy(Policy::new(bps(2_000_000)));
		assert_eq!(follower.policy().max, bps(2_000_000));
		// The 3 Mbps grant no longer fits under the new 2 Mbps ceiling, which is where a
		// new policy starts, so nothing moves until the grant does.
		assert!(
			kio::clock::timeout(Duration::from_secs(60), follower.next())
				.await
				.is_err()
		);

		follower.set_policy(Policy::new(bps(4_000_000)));
		assert_eq!(follower.next().await.unwrap(), on(bps(3_000_000)));
		follower.set_policy(Policy::new(bps(4_000_000)));
		assert!(
			kio::clock::timeout(Duration::from_secs(60), follower.next())
				.await
				.is_err()
		);
	}

	/// A raised ceiling set before the follower polls a grant between the old and new
	/// ceilings follows that grant, rather than clamping it to the old ceiling and
	/// reporting nothing while the sender runs at the new one.
	#[tokio::test(start_paused = true)]
	async fn a_raised_policy_follows_the_next_grant() {
		let grant = bandwidth::Producer::new();
		let mut follower = Follower::new(grant.consume(), Policy::new(bps(2_000_000)));
		grant.set(Some(bps(2_000_000))).unwrap();
		assert!(
			kio::clock::timeout(Duration::from_secs(60), follower.next())
				.await
				.is_err(),
			"settled at the 2 Mbps ceiling"
		);

		grant.set(Some(bps(3_000_000))).unwrap();
		follower.set_policy(Policy::new(bps(4_000_000)));
		assert_eq!(follower.next().await.unwrap(), on(bps(3_000_000)));
	}

	/// A new lower rendition alone keeps the target the sender is actually at, so a
	/// grant at the ceiling still ramps it there instead of reading as already applied.
	#[tokio::test(start_paused = true)]
	async fn a_new_lower_keeps_the_applied_target() {
		let grant = bandwidth::Producer::new();
		let mut follower = Follower::new(grant.consume(), Policy::new(bps(4_000_000)));
		grant.set(Some(bps(2_000_000))).unwrap();
		assert_eq!(follower.next().await.unwrap(), on(bps(2_000_000)));

		grant.set(Some(bps(4_000_000))).unwrap();
		let mut policy = Policy::new(bps(4_000_000));
		policy.lower = Some(bps(1_000_000));
		follower.set_policy(policy);

		let mut applied = Vec::new();
		while let Ok(target) = kio::clock::timeout(Duration::from_secs(60), follower.next()).await {
			applied.push(target.unwrap());
		}
		assert_eq!(applied.last(), Some(&on(bps(4_000_000))), "{applied:?}");
		assert!(applied.len() > 1, "a raise from 2 Mbps ramps: {applied:?}");
	}

	/// A grant no session probes for, like a whole estimate, never disables a rendition,
	/// however far it falls under the band boundary.
	#[tokio::test(start_paused = true)]
	async fn an_unprobed_follower_never_disables() {
		let grant = bandwidth::Producer::new();
		let mut policy = Policy::new(bps(4_000_000));
		policy.lower = Some(bps(2_000_000));
		let mut follower = Follower::new(grant.consume(), policy);
		grant.set(Some(bps(1_000_000))).unwrap();
		assert_eq!(follower.next().await.unwrap(), on(bps(1_000_000)));
	}

	/// A closed grant ends the follower with the reason.
	#[tokio::test(start_paused = true)]
	async fn a_closed_grant_ends_the_follower() {
		let grant = bandwidth::Producer::new();
		let mut follower = Follower::new(grant.consume(), Policy::new(bps(4_000_000)));
		drop(grant);
		assert!(follower.next().await.is_err());
	}

	/// A 5 Mbps rendition above a 2.5 Mbps one, probed, so the boundary lands on 3_333_333.
	fn rung() -> Control {
		let mut policy = Policy::new(bps(5_000_000));
		policy.lower = Some(bps(2_500_000));
		let mut control = Control::new(policy);
		control.set_probed(true);
		control
	}

	#[test]
	fn a_rung_disables_below_its_boundary() {
		let mut rung = rung();
		let start = kio::clock::now();

		// Squeezed, but still inside its band: it adapts like any sender.
		assert_eq!(rung.update(Some(bps(3_500_000)), start), Some(on(bps(3_500_000))));
		// Below it, the lower rendition is the better picture, so stop.
		let off = start + Duration::from_millis(100);
		assert_eq!(rung.update(Some(bps(3_000_000)), off), Some(Target::Disabled));
		assert_eq!(rung.target(), Target::Disabled);
		assert_eq!(rung.deadline(), None, "only a new estimate resumes it");
	}

	/// The lowest rendition has no lower one, so no grant disables it: it floors at
	/// `min` and leaves the rest to the transport, which can only probe with what it sends.
	#[test]
	fn the_lowest_rung_never_disables() {
		let mut lowest = Control::new(Policy::new(bps(350_000)));
		lowest.set_probed(true);
		assert_eq!(lowest.update(Some(bps(1)), kio::clock::now()), Some(on(bps(35_000))));
	}

	/// Without a session probing for it, a stopped rendition could never see the estimate
	/// rise, so it keeps encoding and follows its grant down instead.
	#[test]
	fn an_unprobed_rung_never_disables() {
		let mut rung = rung();
		rung.set_probed(false);
		assert_eq!(
			rung.update(Some(bps(1_000_000)), kio::clock::now()),
			Some(on(bps(1_000_000)))
		);
	}

	/// A rendition disabled while probing resumes at the next estimate once probing stops,
	/// following that grant rather than waiting on an estimate that can no longer rise.
	#[test]
	fn a_disabled_rung_resumes_when_probing_stops() {
		let mut rung = rung();
		let start = kio::clock::now();
		assert_eq!(rung.update(Some(bps(1_000_000)), start), Some(Target::Disabled));

		rung.set_probed(false);
		assert_eq!(
			rung.update(Some(bps(1_000_000)), start + Duration::from_secs(1)),
			Some(on(bps(1_000_000)))
		);
	}

	#[test]
	fn a_rung_resumes_past_the_hysteresis_and_ramps() {
		let mut rung = rung();
		let start = kio::clock::now();
		assert_eq!(rung.update(Some(bps(3_000_000)), start), Some(Target::Disabled));

		// Back over the boundary, but not by the 5% hysteresis: stay off.
		assert_eq!(rung.update(Some(bps(3_400_000)), start + Duration::from_secs(1)), None);
		assert_eq!(rung.update(Some(bps(3_499_000)), start + Duration::from_secs(2)), None);
		assert_eq!(rung.target(), Target::Disabled);

		// Clear of it: resume at the boundary rather than the whole estimate.
		let resumed = start + Duration::from_secs(3);
		assert_eq!(rung.update(Some(bps(5_000_000)), resumed), Some(on(bps(3_333_333))));

		// Then ramp at 25%/s like any raise, on deadlines alone.
		let (applied, _) = follow(&mut rung, bps(5_000_000));
		assert_eq!(applied.last(), Some(&bps(5_000_000)), "{applied:?}");
	}

	/// A grant jittering around the boundary must not flap the catalog: the deadband
	/// keeps a rung on until the grant falls well under its applied target, and the
	/// resume margin keeps it off until the grant is clearly back.
	#[test]
	fn jitter_at_the_boundary_does_not_flap() {
		let mut rung = rung();
		let start = kio::clock::now();
		rung.update(Some(bps(3_400_000)), start).unwrap();

		// About 3% either side of the boundary, every 100ms for 10s.
		let jitter = |tick: u64| if tick.is_multiple_of(2) { 3_250_000 } else { 3_430_000 };
		for tick in 1..=100u64 {
			let at = start + Duration::from_millis(100 * tick);
			assert_eq!(rung.update(Some(bps(jitter(tick))), at), None, "tick {tick}");
		}
		assert_eq!(rung.target(), on(bps(3_400_000)));

		// A real drop turns it off, and the same jitter can't bring it back.
		let off = start + Duration::from_secs(20);
		assert_eq!(rung.update(Some(bps(3_000_000)), off), Some(Target::Disabled));
		for tick in 1..=100u64 {
			let at = off + Duration::from_millis(100 * tick);
			assert_eq!(rung.update(Some(bps(jitter(tick))), at), None, "tick {tick}");
		}
		assert_eq!(rung.target(), Target::Disabled);
	}

	#[test]
	fn a_missing_estimate_holds_a_disabled_rung() {
		let mut rung = rung();
		let now = kio::clock::now();
		rung.update(Some(bps(1_000_000)), now).unwrap();
		assert_eq!(rung.update(None, now + Duration::from_secs(10)), None);
		assert_eq!(rung.target(), Target::Disabled);
	}

	/// A resume margin above the ceiling could never be met by an estimate clamped to
	/// that ceiling, so it is capped there.
	#[test]
	fn a_rung_just_above_its_lower_still_resumes() {
		let mut policy = Policy::new(bps(1_000_000));
		policy.lower = Some(bps(950_000));
		let mut rung = Control::new(policy);
		rung.set_probed(true);
		let now = kio::clock::now();
		assert_eq!(rung.update(Some(bps(500_000)), now), Some(Target::Disabled));
		assert_eq!(
			rung.update(Some(bps(10_000_000)), now + Duration::from_secs(1)),
			Some(on(bps(966_666)))
		);
	}

	/// A lower rendition at or above this one's ceiling leaves no band to encode in.
	#[test]
	fn a_rung_without_a_band_never_disables() {
		let mut policy = Policy::new(bps(1_000_000));
		policy.lower = Some(bps(1_000_000));
		let mut rung = Control::new(policy);
		rung.set_probed(true);
		assert_eq!(rung.update(Some(bps(1)), kio::clock::now()), Some(on(bps(100_000))));
	}

	/// A disable or resume the sender failed to apply is taken back and retried, so the
	/// catalog follows what the encoder did rather than what was asked.
	#[test]
	fn a_failed_toggle_is_retried() {
		let mut rung = rung();
		let start = kio::clock::now();
		assert_eq!(rung.update(Some(bps(3_000_000)), start), Some(Target::Disabled));
		rung.failed(start);
		assert_eq!(rung.target(), on(bps(5_000_000)));
		assert_eq!(rung.deadline(), Some(start + RETRY));
		assert_eq!(rung.update(Some(bps(3_000_000)), start + RETRY), Some(Target::Disabled));

		let resumed = start + Duration::from_secs(5);
		assert_eq!(rung.update(Some(bps(5_000_000)), resumed), Some(on(bps(3_333_333))));
		rung.failed(resumed);
		assert_eq!(rung.target(), Target::Disabled);
		assert_eq!(rung.deadline(), Some(resumed + RETRY));
		assert_eq!(
			rung.update(Some(bps(5_000_000)), resumed + RETRY),
			Some(on(bps(3_333_333)))
		);
	}

	/// A resume the sender failed, whose grant then falls back under the margin, drops
	/// its retry: an update at that deadline would change nothing, so a follower woken
	/// by it would spin.
	#[test]
	fn a_failed_resume_under_the_margin_stops_retrying() {
		let mut rung = rung();
		let start = kio::clock::now();
		assert_eq!(rung.update(Some(bps(3_000_000)), start), Some(Target::Disabled));
		let resumed = start + Duration::from_secs(1);
		assert_eq!(rung.update(Some(bps(5_000_000)), resumed), Some(on(bps(3_333_333))));
		rung.failed(resumed);
		assert_eq!(rung.deadline(), Some(resumed + RETRY));

		assert_eq!(rung.update(Some(bps(3_000_000)), resumed + RETRY / 2), None);
		assert_eq!(rung.target(), Target::Disabled);
		assert_eq!(rung.deadline(), None);
	}

	/// A rung following an unprobed grant under its boundary disables once probing starts,
	/// though the grant never moves, unless the grant can fund it.
	#[test]
	fn a_rung_under_its_boundary_disables_when_probing_starts() {
		let mut rung = rung();
		rung.set_probed(false);
		let start = kio::clock::now();
		assert_eq!(rung.update(Some(bps(3_000_000)), start), Some(on(bps(3_000_000))));

		let mut funded = rung.clone();
		rung.set_probed(true);
		assert_eq!(
			rung.update(Some(bps(3_000_000)), start + Duration::from_millis(100)),
			Some(Target::Disabled)
		);

		// A grant over the boundary funds it, so it ramps up from under it instead.
		funded.set_probed(true);
		assert_eq!(
			funded.update(Some(bps(5_000_000)), start + Duration::from_secs(1)),
			Some(on(bps(3_750_000)))
		);
	}

	/// The same when the lower rendition rises over the applied target.
	#[test]
	fn a_rung_under_a_raised_boundary_disables() {
		let mut rung = rung();
		let start = kio::clock::now();
		assert_eq!(rung.update(Some(bps(3_500_000)), start), Some(on(bps(3_500_000))));

		// The boundary moves from 3.33 Mbps to 3.67 Mbps, over the applied target.
		let mut policy = rung.policy.clone();
		policy.lower = Some(bps(3_000_000));
		assert!(rung.set_policy(policy));
		assert_eq!(
			rung.update(Some(bps(3_500_000)), start + Duration::from_millis(100)),
			Some(Target::Disabled)
		);
	}

	/// End to end through the allocator: descending priority down the renditions fills
	/// the lower ones first, so a shrinking estimate disables the top rendition and leaves
	/// the lower ones at their full share. The top ones come back once the estimate
	/// does, even after their viewers have left, since an idle reservation still reads
	/// the slice it would get back.
	#[test]
	fn the_top_rung_yields_to_the_lower_ones() {
		use moq_net::{broadcast, track};

		let estimate = bandwidth::Producer::new();
		let allocator = bandwidth::Allocator::new(estimate.consume());
		let broadcast = broadcast::Info::default().produce();
		// Lowest first, each a step below the last in priority and a doubling in ceiling.
		let ladder = [
			(1_000_000, None),
			(2_000_000, Some(1_000_000)),
			(4_000_000, Some(2_000_000)),
		];
		let mut tracks = Vec::new();
		let mut viewers = Vec::new();
		let mut reservations = Vec::new();
		let mut controls = Vec::new();
		for (index, (max, lower)) in ladder.into_iter().enumerate() {
			let priority = 60 - index as i8;
			let track = broadcast
				.create_track(format!("{max}"), track::Info::default().with_priority(priority))
				.unwrap();
			viewers.push(Some(track.subscribe(None)));
			reservations.push(allocator.reserve(&track.demand(), bps(max)));
			let mut policy = Policy::new(bps(max));
			policy.lower = lower.map(bps);
			let mut control = Control::new(policy);
			control.set_probed(true);
			controls.push(control);
			tracks.push(track);
		}
		let start = kio::clock::now();
		let mut update = |at: Instant| -> Vec<Target> {
			controls
				.iter_mut()
				.zip(&reservations)
				.map(|(control, reservation)| {
					control.update(reservation.rate(), at);
					control.target()
				})
				.collect()
		};

		// 4.5 Mbps funds the lower two in full and leaves 1.5 Mbps, under the top
		// rendition's 2.67 Mbps boundary.
		estimate.set(Some(bps(4_500_000))).unwrap();
		assert_eq!(
			update(start),
			[on(bps(1_000_000)), on(bps(2_000_000)), Target::Disabled]
		);

		// Shrinking further takes the middle one next, still keeping the lowest whole.
		estimate.set(Some(bps(2_000_000))).unwrap();
		assert_eq!(
			update(start + Duration::from_secs(1)),
			[on(bps(1_000_000)), Target::Disabled, Target::Disabled]
		);

		// Below every boundary, the lowest still encodes at what it gets.
		estimate.set(Some(bps(200_000))).unwrap();
		assert_eq!(
			update(start + Duration::from_secs(2)),
			[on(bps(200_000)), Target::Disabled, Target::Disabled]
		);

		// The viewers of the stopped renditions fall back to the lowest and leave, and
		// the uplink recovers: both resume at their boundaries without a viewer.
		viewers[1] = None;
		viewers[2] = None;
		estimate.set(Some(bps(8_000_000))).unwrap();
		let recovered = update(start + Duration::from_secs(3));
		assert_eq!(recovered[1], on(bps(1_333_333)));
		assert_eq!(recovered[2], on(bps(2_666_666)));
	}

	/// Random walks of grants, probing, policy changes, refused changes, and time,
	/// replayed through [`Control`] the way [`Follower`] drives it, checking the rung's
	/// invariants after every wake.
	mod model {
		use proptest::prelude::*;
		use proptest::test_runner::{Config, RngSeed, TestRunner};

		use super::*;

		#[derive(Clone, Debug)]
		enum Op {
			Grant(Option<u64>),
			/// Move the grant by this many per mille, around the deadband and the margins.
			Jitter(i16),
			Probed(bool),
			Lower(Option<u64>),
			Max(u64),
			Min(u64),
			Advance(Duration),
		}

		fn op() -> impl Strategy<Value = Op> {
			let rate = || prop_oneof![0..12_000_000u64, 0..2_000u64];
			prop_oneof![
				3 => proptest::option::weighted(0.9, rate()).prop_map(Op::Grant),
				3 => (-80i16..=80).prop_map(Op::Jitter),
				2 => any::<bool>().prop_map(Op::Probed),
				2 => proptest::option::weighted(0.8, rate()).prop_map(Op::Lower),
				1 => (1..10_000_000u64).prop_map(Op::Max),
				1 => rate().prop_map(Op::Min),
				4 => prop_oneof![0..400u64, 0..20_000u64].prop_map(|ms| Op::Advance(Duration::from_millis(ms))),
			]
		}

		/// What the follower feeds the control, and when.
		struct Model {
			control: Control,
			now: Instant,
			grant: Option<bandwidth::Rate>,
			probed: bool,
		}

		impl Model {
			fn apply(&mut self, op: Op, fail: bool) {
				let mut policy = self.control.policy.clone();
				match op {
					Op::Grant(grant) => self.set_grant(grant.map(bps), fail),
					Op::Jitter(per_mille) => {
						let Some(grant) = self.grant else { return };
						let moved = i128::from(grant.as_bps()) * (1_000 + i128::from(per_mille)) / 1_000;
						self.set_grant(Some(bps(moved as u64)), fail);
					}
					Op::Probed(probed) => {
						// The grant reports a flip, and only a flip.
						if probed != self.probed {
							self.probed = probed;
							self.wake(fail);
						}
					}
					Op::Lower(lower) => {
						policy.lower = lower.map(bps);
						self.set_policy(policy, fail);
					}
					Op::Max(max) => {
						policy.max = bps(max);
						self.set_policy(policy, fail);
					}
					Op::Min(min) => {
						policy.min = bps(min);
						self.set_policy(policy, fail);
					}
					Op::Advance(elapsed) => {
						let end = self.now + elapsed;
						while let Some(at) = self.control.deadline().filter(|at| *at <= end) {
							self.now = at;
							self.wake(fail);
						}
						self.now = end;
					}
				}
			}

			fn set_grant(&mut self, grant: Option<bandwidth::Rate>, fail: bool) {
				// The grant wakes the follower only when it moves.
				if grant != self.grant {
					self.grant = grant;
					self.wake(fail);
				}
			}

			fn set_policy(&mut self, policy: Policy, fail: bool) {
				if self.control.set_policy(policy) {
					self.wake(fail);
				}
			}

			/// One update, as the follower makes on any wake, then the sender refusing what
			/// it returned when `fail`.
			fn wake(&mut self, fail: bool) {
				let before = self.control.clone();
				self.control.set_probed(self.probed);
				let change = self.control.update(self.grant, self.now);
				self.check(&before, change);
				if fail && change.is_some() {
					self.control.failed(self.now);
				}
				// Anything already due would wake the follower again at once, for nothing.
				if let Some(at) = self.control.deadline() {
					assert!(at > self.now, "a deadline at {at:?} is already due at {:?}", self.now);
				}
			}

			fn check(&self, before: &Control, change: Option<Target>) {
				let policy = &self.control.policy;
				let min = policy.min.min(policy.max);
				let boundary = policy.boundary();
				if let Target::Enabled(rate) = self.control.target() {
					assert!(rate <= policy.max, "{rate:?} over {policy:?}");
					assert!(rate >= min, "{rate:?} under {policy:?}");
				}

				if change == Some(Target::Disabled) {
					let boundary = boundary.expect("a policy without a band disabled the rung");
					assert!(self.probed, "an unprobed rung disabled");
					let desired = self
						.grant
						.expect("a missing estimate disabled the rung")
						.clamp(min, policy.max);
					assert!(desired < boundary, "{desired:?} is not under {boundary:?}");
					let Target::Enabled(was) = before.target() else {
						panic!("a disabled rung disabled again")
					};
					// Outside the deadband, unless the applied target was already under the boundary.
					assert!(
						was < boundary || was.as_bps() - desired.as_bps() >= band(was).as_bps(),
						"{was:?} to {desired:?} is inside the deadband"
					);
				}

				// A missing estimate holds whatever the rung was doing.
				let Some(estimate) = self.grant else { return };
				let desired = estimate.clamp(min, policy.max);
				match self.control.target() {
					Target::Disabled => {
						assert!(self.probed, "an unprobed rung stayed disabled");
						let boundary = boundary.expect("a policy without a band stayed disabled");
						let clear = (boundary.as_bps() + band(boundary).as_bps()).min(policy.max.as_bps());
						assert!(
							desired.as_bps() < clear,
							"{desired:?} clears {boundary:?} by the margin, yet the rung stayed disabled"
						);
					}
					Target::Enabled(rate) => {
						if let Some(boundary) = boundary.filter(|boundary| self.probed && desired < *boundary) {
							assert!(rate >= boundary, "{rate:?} stayed on under {boundary:?} at {desired:?}");
							assert!(
								rate.as_bps() - desired.as_bps() < band(rate).as_bps(),
								"{rate:?} stayed on past the deadband at {desired:?}"
							);
						}
					}
				}
			}
		}

		#[test]
		fn invariants_hold_on_random_walks() {
			let mut runner = TestRunner::new(Config {
				cases: 512,
				rng_seed: RngSeed::Fixed(6781),
				failure_persistence: None,
				..Default::default()
			});
			let walk = (
				1..10_000_000u64,
				proptest::option::of(0..12_000_000u64),
				proptest::collection::vec((op(), proptest::bool::weighted(0.1)), 1..200),
			);
			runner
				.run(&walk, |(max, lower, ops)| {
					let mut policy = Policy::new(bps(max));
					policy.lower = lower.map(bps);
					let mut model = Model {
						control: Control::new(policy),
						now: kio::clock::now(),
						grant: None,
						probed: false,
					};
					for (op, fail) in ops {
						model.apply(op, fail);
					}
					Ok(())
				})
				.unwrap();
		}
	}
}
