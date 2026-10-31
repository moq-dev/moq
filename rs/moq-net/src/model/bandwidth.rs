//! Rate estimation, split into a [Producer] and [Consumer] handle.
//!
//! A [Producer] is used to set the current estimated bitrate, notifying consumers.
//! A [Consumer] can read the current estimate and wait for changes.
//! A [Demand] watches whether anyone reads it, so a producer estimates only on demand.
//!
//! One estimate covers a whole connection, so senders sharing one divide it with
//! an [Allocator] rather than each targeting the whole thing. How a sender then
//! *follows* its share is shared media policy: see `moq_mux::rate`.
//!
//! The same reservations set the connection's probe target: their ceilings summed,
//! which the session hands to its transport's [`Probe`](crate::transport::Probe) so
//! the estimate can rise to fund them.
//!
//! The receive direction mirrors it: [Viewers] divides the receive estimate among the
//! broadcasts watched over one connection, and their ceilings summed are the PROBE
//! target the session asks its peer to probe up to.

use std::{collections::BTreeMap, task::Poll};

use crate::{Error, Result, track};

/// A rate, in bits per second.
///
/// A newtype rather than a bare integer because everything that meets in an
/// [`Allocator`] is the same quantity measured the same way: a congestion
/// controller's estimate, a track's reservation, an encoder's ceiling. One of them
/// reading bytes per second, or kilobits, is off by a factor of eight or a thousand
/// and still typechecks, which is the kind of wrong that reaches production.
///
/// Named for what it measures rather than for the module: `Session::stats` has called
/// this quantity a rate all along (`estimated_send_rate`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rate(u64);

impl Rate {
	/// No bandwidth at all.
	pub const ZERO: Self = Self(0);

	/// A rate in bits per second, the unit every wire and codec API here uses.
	pub const fn from_bps(bps: u64) -> Self {
		Self(bps)
	}

	/// A rate in kilobits (1000 bits) per second, saturating.
	pub const fn from_kbps(kbps: u64) -> Self {
		Self(kbps.saturating_mul(1_000))
	}

	/// A rate in megabits (1000 kilobits) per second, saturating.
	pub const fn from_mbps(mbps: u64) -> Self {
		Self(mbps.saturating_mul(1_000_000))
	}

	/// This rate in bits per second, for handing to a codec or FFI that wants a plain integer.
	pub const fn as_bps(self) -> u64 {
		self.0
	}

	/// This rate scaled by `factor`, saturating at both ends.
	///
	/// For fractional arithmetic, such as halving a rate, without spreading `as`
	/// casts across the callers.
	pub fn scaled(self, factor: f64) -> Self {
		let scaled = self.0 as f64 * factor.max(0.0);
		if scaled >= u64::MAX as f64 {
			Self(u64::MAX)
		} else {
			Self(scaled as u64)
		}
	}

	/// The absolute difference between two rates.
	pub const fn abs_diff(self, other: Self) -> Self {
		Self(self.0.abs_diff(other.0))
	}
}

#[derive(Default)]
struct State {
	bitrate: Option<Rate>,
}

/// Produces bandwidth estimates, notifying consumers when the value changes.
#[derive(Clone)]
pub struct Producer {
	state: kio::Producer<State>,
}

impl Producer {
	/// Create a fresh producer with no current estimate.
	pub fn new() -> Self {
		Self {
			state: kio::Producer::default(),
		}
	}

	/// Set the current bandwidth estimate, or `None` while the backend has none.
	pub fn set(&self, bitrate: Option<Rate>) -> Result<()> {
		let mut state = self.state.write().map_err(|_| Error::Dropped)?;
		if state.bitrate != bitrate {
			state.bitrate = bitrate;
		}
		Ok(())
	}

	/// Create a new consumer for the bandwidth estimate.
	pub fn consume(&self) -> Consumer {
		Consumer {
			inner: Inner::Whole(self.state.consume()),
			last: None,
			probed: false,
		}
	}

	/// A handle that mints consumers without being one, so holding it is not demand.
	pub fn weak(&self) -> Weak {
		Weak(self.state.weak())
	}

	/// A watch-only handle to whether anyone reads the estimate.
	pub fn demand(&self) -> Demand {
		Demand {
			state: self.state.weak(),
			prev: None,
		}
	}

	/// Poll for consumer presence to differ from `last`, with the new value once it does.
	/// Errors once the channel closes.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn poll_wanted_changed(&self, waiter: &kio::Waiter, last: bool) -> Poll<Result<bool>> {
		self.state.poll_wanted_changed(waiter, last).map_err(|_| Error::Dropped)
	}
}

/// A cloneable, watch-only handle to whether anyone reads a bandwidth estimate.
///
/// Holding it is not demand and keeps nothing open. Once every [`Producer`] is gone, its
/// waits end with [`Error::Dropped`]. Each clone watches on its own, so its first
/// [`poll_wanted_changed`](Self::poll_wanted_changed) reports the current value.
pub struct Demand {
	state: kio::ProducerWeak<State>,
	// The value `poll_wanted_changed` last returned, or `None` before it returned any.
	prev: Option<bool>,
}

impl Clone for Demand {
	fn clone(&self) -> Self {
		Self {
			state: self.state.clone(),
			prev: None,
		}
	}
}

impl Demand {
	/// Whether the estimate is still produced and a consumer reads it right now.
	pub fn wanted(&self) -> bool {
		!self.state.is_closed() && self.state.wanted()
	}

	/// Return [`wanted`](Self::wanted) once it differs from what this handle last returned:
	/// immediately on the first call, then on each change. Errors once every producer is gone.
	#[cfg(feature = "async")]
	pub async fn wanted_changed(&mut self) -> Result<bool> {
		kio::wait(|waiter| self.poll_wanted_changed(waiter)).await
	}

	/// Poll for [`wanted`](Self::wanted) to differ from what this handle last returned.
	pub fn poll_wanted_changed(&mut self, waiter: &kio::Waiter) -> Poll<Result<bool>> {
		loop {
			if self.state.is_closed() {
				return Poll::Ready(Err(Error::Dropped));
			}
			let wanted = self.state.wanted();
			if self.prev != Some(wanted) {
				self.prev = Some(wanted);
				return Poll::Ready(Ok(wanted));
			}
			// Closing fails the wait, which errors above.
			let _ = std::task::ready!(self.state.poll_wanted_changed(waiter, wanted));
		}
	}
}

impl Default for Producer {
	fn default() -> Self {
		Self::new()
	}
}

/// Mints [`Consumer`]s on demand without counting as one, nor keeping the channel open.
#[derive(Clone)]
pub struct Weak(kio::ProducerWeak<State>);

impl Weak {
	/// A new consumer, which reports the closure once the producer is gone.
	pub fn consume(&self) -> Consumer {
		Consumer {
			inner: Inner::Whole(self.0.consume()),
			last: None,
			probed: false,
		}
	}
}

/// Divides one connection's bandwidth estimate among the tracks sharing it.
///
/// Every sender on a connection reads the same estimate, so N senders each
/// targeting all of it oversubscribe the uplink N times over. Register a track
/// here and it gets a [`Consumer`] reporting only its own slice, so the slices
/// sum to the estimate instead of each matching it.
///
/// Advisory, not enforced. A track that ignores its slice, or can't follow it at
/// all (PCM audio has a fixed bitrate), still sends what it sends; the transport
/// sheds the excess by dropping groups. Rate estimation isn't exact enough
/// for the difference to be worth policing.
///
/// The reservations also set the session's probe target: the sum of every live
/// reservation's ceiling, demanded or not, so a sender that stopped for lack of
/// bandwidth keeps asking the connection for its room. A session probes for its own
/// allocator, so take it from `Session::allocator` (with the `async` feature) rather
/// than building one: an allocator from [`new`](Self::new) divides an estimate but
/// probes for nothing.
///
/// Clones share one registry, so hand a clone to each sender.
#[derive(Clone)]
pub struct Allocator {
	estimate: Consumer,
	registry: kio::Producer<Registry>,
}

impl Allocator {
	/// Divide `estimate`, with no session probing for the reservations.
	///
	/// For an estimate that is not a session's, such as a test's or another
	/// reservation's slice. A session's own comes from
	/// `Session::allocator` with the `async` feature.
	pub fn new(estimate: Consumer) -> Self {
		Self {
			estimate,
			registry: kio::Producer::default(),
		}
	}

	/// The same registry dividing `estimate` instead, or nothing when it is `None`.
	#[cfg(feature = "async")]
	// Sessions follow their allocator's target; a custom session has no public way to yet.
	pub(crate) fn with_estimate(&self, estimate: Option<Consumer>) -> Self {
		Self {
			estimate: estimate.unwrap_or(Consumer {
				inner: Inner::Unavailable,
				last: None,
				probed: false,
			}),
			registry: self.registry.clone(),
		}
	}

	/// Probe up to `target` instead of the sum of the reservations, or `None` to follow them again.
	///
	/// `Some(Rate::ZERO)` turns probing off. The override is the registry's, so it holds
	/// for every clone and across the reconnects a persistent allocator spans.
	pub fn set_target(&self, target: Option<Rate>) {
		let Ok(mut registry) = self.registry.write() else {
			unreachable!("the allocator holds its own registry producer")
		};
		if registry.target != target {
			registry.target = target;
		}
	}

	/// Follow the probe target, for the session that probes on the reservations' behalf.
	///
	/// Holding it is what marks the reservations [`probed`](Consumer::probed), so only a
	/// session whose transport has a sender takes one.
	#[cfg(any(feature = "async", test))]
	// Sessions follow their allocator's target; a custom session has no public way to yet.
	pub(crate) fn target(&self) -> Target {
		let Ok(mut registry) = self.registry.write() else {
			unreachable!("the allocator holds its own registry producer")
		};
		registry.probes += 1;
		drop(registry);
		Target {
			registry: Some(self.registry.consume()),
			probing: self.registry.downgrade(),
			last: None,
		}
	}

	/// An allocator with nothing to divide, so every reservation reports `None`.
	///
	/// `None` already means "no opinion, hold your rate" to a sender, so this is what
	/// a transport with no congestion estimate, a local file, or a test harness wants,
	/// and it saves every config struct on the way down from being an `Option`. It is
	/// also [`Default`], so a config that never sets one encodes at its configured rate.
	pub fn unlimited() -> Self {
		Self {
			estimate: Consumer {
				inner: Inner::Unavailable,
				last: None,
				probed: false,
			},
			registry: kio::Producer::default(),
		}
	}

	/// Reserve up to `max` for `track`, returning the reservation.
	///
	/// `max` is a ceiling, not a measurement: reserve the most the track can ever
	/// send, not what it happens to be sending. A VBR encoder sitting on a black
	/// screen at 1 Mbps can jump to 6 Mbps between one frame and the next, and a
	/// reservation that had followed it down would have already handed that room
	/// to somebody else.
	///
	/// Priority comes from the track ([`track::Info::priority`], higher served
	/// first). A tier is filled to its reservations before the next one sees a
	/// bit; within a tier the split is max-min fair, so a share asking for less
	/// than an even cut takes all of it and leaves the difference to the others.
	///
	/// That is the *publisher's* priority, which the send order only falls back on:
	/// it ranks by each subscription's own priority first, so a subscriber asking for
	/// video ahead of audio is served that way whatever this decides, and this one
	/// breaks the ties (or alone orders a broadcast's tracks on a session scheduled
	/// per broadcast). The publisher's is still the right one to divide by, since
	/// allocation is a decision about what to *produce*, and there is no single
	/// subscriber priority to read when several are watching one track.
	///
	/// That last part carries a publisher that leaves `priority` at its default:
	/// one tier of audio and video still serves audio's small reservation in full
	/// before video takes the remainder.
	///
	/// The reservation lasts as long as the returned [`Reservation`]: hold it for as
	/// long as the sender is publishing, change the ceiling with
	/// [`update`](Reservation::update), and drop it to hand the room back. It is
	/// released when the track closes either way, since a [`track::Demand`] is a weak
	/// handle and reserving never keeps a track alive.
	///
	/// Read the current slice through [`Reservation::consume`], which reports `None`
	/// while the connection has no estimate, telling a sender to hold its current rate
	/// rather than encode at zero. While nothing is subscribed to the track it reports
	/// the slice the track would get back, without claiming it from the others.
	pub fn reserve(&self, track: &track::Demand, max: Rate) -> Reservation {
		// Read the track before taking the registry lock, so the two are never held
		// at once and there's no order to get wrong.
		self.claim(self.estimate.clone(), Some(track.clone()), track.priority(), max)
	}

	/// Claim up to `max` of `estimate`, active whenever `demand` is, or always without one.
	fn claim(&self, estimate: Consumer, demand: Option<track::Demand>, priority: i8, max: Rate) -> Reservation {
		let id = {
			// Nothing ever closes this channel: the allocator holds the only producer
			// and never aborts it, so it's open for as long as `self` is.
			let Ok(mut registry) = self.registry.write() else {
				unreachable!("the allocator holds its own registry producer")
			};
			// Closed tracks can't be demanded again; drop them rather than walking
			// them on every poll for the rest of the connection.
			let closed: Vec<u64> = registry
				.entries
				.iter()
				.filter(|(_, entry)| entry.demand.as_ref().is_some_and(track::Demand::is_closed))
				.map(|(id, _)| *id)
				.collect();
			for id in closed {
				registry.remove(id);
			}

			let id = registry.next_id;
			registry.next_id += 1;
			registry.reserved += u128::from(max.as_bps());
			registry.entries.insert(id, Entry { demand, priority, max });
			id
		};

		Reservation {
			share: Share {
				estimate,
				registry: self.registry.consume(),
				id,
			},
			registry: self.registry.downgrade(),
		}
	}
}

/// One track's standing claim on an [`Allocator`], held for as long as the sender
/// that took it is publishing.
///
/// Separate from the [`Consumer`] that reads the slice, because the two have opposite
/// lifetimes: read handles are cloned around and dropped freely, while the claim itself
/// has to outlive every one of them or the sender silently stops claiming anything.
#[must_use = "a dropped Reservation is released, so the sender claims nothing and its siblings take the room"]
pub struct Reservation {
	share: Share,
	/// Weak so a reservation can't keep the registry alive: one outliving every
	/// [`Allocator`] reports the estimate as gone rather than holding the channel open.
	registry: kio::Weak<Registry>,
}

impl Reservation {
	/// This reservation's slice right now.
	///
	/// Stateless, unlike [`Consumer::poll_changed`], which carries a cursor over what it last
	/// reported and so needs a handle of its own.
	pub fn rate(&self) -> Option<Rate> {
		self.share.grant()
	}

	/// A handle reading this reservation's current slice of the estimate.
	///
	/// Cloneable and independent of the reservation: once the reservation is dropped
	/// these report `None`, the same "hold your rate" a missing estimate reports.
	pub fn consume(&self) -> Consumer {
		Consumer {
			inner: Inner::Share(Box::new(self.share.clone())),
			last: None,
			probed: false,
		}
	}

	/// Change the ceiling, keeping the same claim.
	///
	/// For a sender whose ceiling genuinely moved: an encoder reopening at a resolution
	/// it negotiated with the device, not an encoder observing its own output. A
	/// reservation that followed the rate a VBR source happens to be sending would hand
	/// the room away every time the picture went still, and not have it back when the
	/// picture moved again.
	///
	/// Does nothing once every [`Allocator`] is gone, since there is then nothing left
	/// dividing anything; a reader learns that from its [`consumer`](Self::consume).
	pub fn update(&self, max: Rate) {
		let Some(registry) = self.registry.upgrade() else {
			return;
		};
		let Ok(mut registry) = registry.write() else {
			return;
		};
		let registry = &mut *registry;
		if let Some(entry) = registry.entries.get_mut(&self.share.id) {
			registry.reserved = registry.reserved - u128::from(entry.max.as_bps()) + u128::from(max.as_bps());
			entry.max = max;
		}
	}
}

impl Drop for Reservation {
	fn drop(&mut self) {
		let Some(registry) = self.registry.upgrade() else {
			return;
		};
		let Ok(mut registry) = registry.write() else {
			return;
		};
		registry.remove(self.share.id);
	}
}

/// An allocator with nothing to divide, so a config that never sets one leaves its
/// senders at their configured rates. See [`Allocator::unlimited`].
impl Default for Allocator {
	fn default() -> Self {
		Self::unlimited()
	}
}

// Hand-written so the config structs that carry an allocator can still derive
// `Debug`. The registry is the only part worth printing.
impl std::fmt::Debug for Allocator {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Allocator")
			.field("registered", &self.registry.read().entries.len())
			.finish()
	}
}

/// Divides one connection's receive estimate among the broadcasts watched over it.
///
/// The receive-side mirror of [`Allocator`]. Every player on a connection reads the
/// same receive estimate, so N players each selecting against all of it oversubscribe
/// the downlink N times over. [`watch`](Self::watch) a broadcast here and its
/// [`Viewer`] reports only its own slice, split max-min fair by each viewer's ceiling,
/// so the slices sum to the estimate instead of each matching it. Viewers carry no
/// priority: a viewer asking for less than an even cut takes all it asked for and
/// leaves the rest to the others.
///
/// The viewers' ceilings summed are also the PROBE target the session sends its
/// peer, which probes the connection up to it when it advertised `Increase`, so the
/// estimate can rise to fund a larger rendition. The target never leaves this hop:
/// a relay probes its own downlink for its viewers, and never forwards them upstream.
///
/// Holding a [`Viewer`] (or a [`Consumer`] read from one) requests PROBE, like
/// holding `Session::recv_bandwidth`. Take it from `Session::viewers` (with the
/// `async` feature) rather than building one, since only a session sends the target.
/// Clones share one registry.
#[derive(Clone, Default)]
pub struct Viewers {
	/// The registry, whose own estimate is unused: each viewer reads one minted from
	/// `estimate`, so holding the registry is not demand.
	registry: Allocator,
	/// Mints the receive estimate each viewer reads, or `None` when there is none.
	estimate: Option<Weak>,
}

impl Viewers {
	/// Divide the receive estimate `estimate` mints, with no session sending the target.
	///
	/// For an estimate that outlives a session, such as one a reconnect loop feeds
	/// from each session it dials. Hand the same registry to `Client::with_viewers`
	/// (with the `async` feature) so every session it dials sends the target.
	pub fn new(estimate: Weak) -> Self {
		Self {
			registry: Allocator::unlimited(),
			estimate: Some(estimate),
		}
	}

	/// The same registry dividing the estimate `estimate` mints instead, or nothing when it is `None`.
	#[cfg(feature = "async")]
	pub(crate) fn with_estimate(&self, estimate: Option<Weak>) -> Self {
		Self {
			registry: self.registry.clone(),
			estimate,
		}
	}

	/// Watch a broadcast receiving at most `max`, returning its viewer.
	///
	/// `max` is a ceiling: what the largest rendition the player would switch to
	/// needs, not what it happens to be receiving. The claim lasts as long as the
	/// returned [`Viewer`]: change the ceiling with [`update`](Viewer::update), and drop
	/// it once the broadcast stops being watched.
	pub fn watch(&self, max: Rate) -> Viewer {
		let estimate = match &self.estimate {
			Some(estimate) => estimate.consume(),
			None => Consumer {
				inner: Inner::Unavailable,
				last: None,
				probed: false,
			},
		};
		Viewer(self.registry.claim(estimate, None, 0, max))
	}

	/// Send `target` as the PROBE target instead of the viewers' sum, or `None` to follow them again.
	///
	/// `Some(Rate::ZERO)` turns probing off: the session sends no target. The override is
	/// the registry's, so it holds for every clone and across the reconnects a persistent
	/// registry spans.
	pub fn set_target(&self, target: Option<Rate>) {
		self.registry.set_target(target);
	}

	/// Follow the PROBE target, for the session that sends it to a peer that probes up to it.
	///
	/// Holding it is what marks the viewers [`probed`](Consumer::probed).
	#[cfg(any(feature = "async", test))]
	pub(crate) fn target(&self) -> Target {
		self.registry.target()
	}
}

// Hand-written so the config structs that carry a registry can still derive `Debug`.
impl std::fmt::Debug for Viewers {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Viewers")
			.field("watched", &self.registry.registry.read().entries.len())
			.finish()
	}
}

/// One watched broadcast's standing claim on [`Viewers`], held for as long as it is watched.
///
/// Holding it requests PROBE. Dropping it releases the claim and lowers the PROBE target.
#[must_use = "a dropped Viewer is released, so the broadcast claims nothing and the PROBE target drops"]
pub struct Viewer(Reservation);

impl Viewer {
	/// This viewer's slice of the receive estimate right now.
	///
	/// `None` while the connection has no receive estimate, telling a player to hold its
	/// current selection.
	pub fn rate(&self) -> Option<Rate> {
		self.0.rate()
	}

	/// A handle reading this viewer's current slice of the receive estimate.
	///
	/// Cloneable and independent of the viewer: once the viewer is dropped these report
	/// `None`. [`Consumer::probed`] reports whether the peer is asked to probe for it.
	pub fn consume(&self) -> Consumer {
		self.0.consume()
	}

	/// Change the ceiling, keeping the same claim, as when a catalog enables or disables a rendition.
	pub fn update(&self, max: Rate) {
		self.0.update(max);
	}
}

/// Every track registered with one [`Allocator`].
#[derive(Default)]
struct Registry {
	/// Keyed by registration order, which [`allocate`] keeps as its tie-break.
	entries: BTreeMap<u64, Entry>,
	next_id: u64,
	/// The entries' ceilings summed, kept as they change so following the target
	/// never walks the table. Wide enough that no number of `u64` ceilings overflows.
	reserved: u128,
	/// The application's override of `reserved` as the probe target.
	target: Option<Rate>,
	/// How many sessions probe for these reservations, each through its [`Target`].
	probes: usize,
}

impl Registry {
	fn remove(&mut self, id: u64) {
		if let Some(entry) = self.entries.remove(&id) {
			self.reserved -= u128::from(entry.max.as_bps());
		}
	}

	/// What the session probes up to.
	fn target(&self) -> Rate {
		self.target
			.unwrap_or_else(|| Rate::from_bps(u64::try_from(self.reserved).unwrap_or(u64::MAX)))
	}
}

/// One registered track's standing reservation.
struct Entry {
	/// The track whose demand gates the claim, or `None` for a claim that is always active.
	demand: Option<track::Demand>,
	priority: i8,
	max: Rate,
}

/// An [`Allocator`]'s probe target, followed by the session that probes for it.
#[cfg(any(feature = "async", test))]
// Sessions follow their allocator's target; a custom session has no public way to yet.
pub(crate) struct Target {
	/// `None` once every allocator is gone.
	registry: Option<kio::Consumer<Registry>>,
	/// Weak, so a session never keeps the registry alive, only counts itself while it does.
	probing: kio::Weak<Registry>,
	last: Option<Rate>,
}

#[cfg(any(feature = "async", test))]
impl Drop for Target {
	fn drop(&mut self) {
		let Some(registry) = self.probing.upgrade() else {
			return;
		};
		let Ok(mut registry) = registry.write() else {
			return;
		};
		registry.probes -= 1;
	}
}

#[cfg(any(feature = "async", test))]
impl Target {
	/// The target once it differs from the last one returned, arming `waiter` otherwise.
	///
	/// Zero once every allocator is gone, since nothing is left reserving.
	// Sessions follow their allocator's target; a custom session has no public way to yet.
	pub(crate) fn poll_changed(&mut self, waiter: &kio::Waiter) -> Poll<Rate> {
		let Some(registry) = &self.registry else {
			return Poll::Pending;
		};
		let last = self.last;
		let polled = registry
			.poll(waiter, |registry| match registry.target() {
				target if Some(target) == last => Poll::Pending,
				target => Poll::Ready(target),
			})
			.map(|res| res.ok());
		let target = match polled {
			Poll::Ready(Some(target)) => target,
			Poll::Ready(None) => {
				self.registry = None;
				Rate::ZERO
			}
			Poll::Pending => return Poll::Pending,
		};
		if Some(target) == last {
			return Poll::Pending;
		}
		self.last = Some(target);
		Poll::Ready(target)
	}
}

/// A share's view of the estimate it divides.
#[derive(Clone)]
struct Share {
	/// What's being divided, which may itself be a share.
	estimate: Consumer,
	registry: kio::Consumer<Registry>,
	id: u64,
}

/// One demanded track's claim, snapshotted out of the [`Registry`] so no lock is
/// held while the tracks themselves are read.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Want {
	id: u64,
	priority: i8,
	max: Rate,
}

impl Share {
	/// This share's slice right now.
	fn grant(&self) -> Option<Rate> {
		let estimate = self.estimate.peek()?;
		let wants: Vec<Want> = self
			.claims()
			.into_iter()
			// Its own track counts while idle too, so an unwatched sender reads the slice
			// it would get back: a rendition stopped for lack of bandwidth, whose viewers
			// then left, needs that to know when to resume.
			.filter(|(want, demand)| match (demand, want.id == self.id) {
				(None, _) => true,
				(Some(demand), true) => !demand.is_closed(),
				(Some(demand), false) => demand.is_wanted(),
			})
			.map(|(want, _)| want)
			.collect();
		allocate(estimate, &wants, self.id)
	}

	/// Whether a session probes for this share's registry, toward a target above zero,
	/// which turns probing off.
	fn probed(&self) -> bool {
		let registry = self.registry.read();
		registry.probes > 0 && registry.target() > Rate::ZERO
	}

	/// This share's slice and whether a session probes for it, arming `waiter` for
	/// everything that could move either: the estimate, the registry, and each
	/// track's demand.
	fn poll_grant(&mut self, waiter: &kio::Waiter) -> Poll<Result<(Option<Rate>, bool)>> {
		// Drain the estimate rather than polling it once. A poll that returns Ready
		// registers no waker, and this share may still conclude its own slice didn't
		// move (its reservation caps it, so most estimate changes don't reach it) and
		// park. Without the drain that park would never be woken by the next move.
		// The value itself is read below, since an unchanged estimate still needs
		// re-dividing when the tracks sharing it change.
		loop {
			match self.estimate.poll_changed(waiter) {
				// Moved: go round again, which either finds it settled and arms the
				// waker, or finds it moved again and makes progress toward the latest.
				Poll::Ready(Ok(_)) => continue,
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Pending => break,
			}
		}

		// Wake on any registration change. The closure never completes, so the only
		// ready outcome is the registry being gone, i.e. every allocator dropped.
		if let Poll::Ready(Err(_)) = self.registry.poll(waiter, |_| Poll::<()>::Pending) {
			return Poll::Ready(Err(Error::Dropped));
		}

		let wants: Vec<Want> = self
			.claims()
			.into_iter()
			.filter(
				|(want, demand)| match demand.as_ref().map(|demand| demand.poll_state(waiter)) {
					None | Some(track::DemandState::Active) => true,
					// An idle track claims nothing from its siblings, so an unwatched encoder's
					// reservation goes to whoever is actually sending. Its own reader still
					// sees the slice it would get back; see `grant`.
					Some(track::DemandState::Idle) => want.id == self.id,
					// Gone for good, and pruned by the next `register`.
					Some(track::DemandState::Closed) => false,
				},
			)
			.map(|(want, _)| want)
			.collect();

		let grant = self
			.estimate
			.peek()
			.and_then(|estimate| allocate(estimate, &wants, self.id));
		Poll::Ready(Ok((grant, self.probed())))
	}

	/// Snapshot the registry so the tracks can be read without holding its lock.
	fn claims(&self) -> Vec<(Want, Option<track::Demand>)> {
		self.registry
			.read()
			.entries
			.iter()
			.map(|(id, entry)| {
				(
					Want {
						id: *id,
						priority: entry.priority,
						max: entry.max,
					},
					entry.demand.clone(),
				)
			})
			.collect()
	}
}

/// Divide `estimate` among `wants`, returning the slice for `id`.
///
/// Strict priority: a tier is filled to its reservations before the next tier
/// sees a bit. Within a tier the split is max-min fair, so a share asking for
/// less than an even split takes all of it and leaves the rest to the others.
///
/// Surplus above the total reserved is left unclaimed rather than spread around.
/// A reservation is what a sender can use, so handing it more is not a reason to
/// send more than it was configured for.
///
/// `None` when `id` isn't among the wants, which is how a released or closed
/// reservation reports "hold your rate" instead of a grant of zero.
fn allocate(estimate: Rate, wants: &[Want], id: u64) -> Option<Rate> {
	// Plain bits per second inside: this is where the division actually happens, and
	// wrapping every intermediate would need arithmetic on `Rate` that no caller wants.
	let mut budget = estimate.as_bps();
	let mut tier = wants.iter().map(|want| want.priority).max();

	while let Some(priority) = tier {
		// Ascending by reservation: each share takes an even cut of what's left, or
		// all it asked for if that's less, which frees the difference for the rest.
		let mut members: Vec<&Want> = wants.iter().filter(|want| want.priority == priority).collect();
		members.sort_by_key(|want| want.max);

		let mut remaining = members.len() as u64;
		for want in members {
			let even = budget / remaining;
			let grant = want.max.as_bps().min(even);
			if want.id == id {
				return Some(Rate::from_bps(grant));
			}
			budget -= grant;
			remaining -= 1;
		}

		tier = wants
			.iter()
			.map(|want| want.priority)
			.filter(|other| *other < priority)
			.max();
	}

	None
}

/// Consumes bandwidth estimates, allowing reads and async change notifications.
#[derive(Clone)]
pub struct Consumer {
	inner: Inner,
	last: Option<Rate>,
	/// The [`probed`](Self::probed) state [`poll_changed`](Self::poll_changed) last reported.
	probed: bool,
}

/// What a [`Consumer`] is reading: the whole estimate, or one track's slice of it.
#[derive(Clone)]
enum Inner {
	Whole(kio::Consumer<State>),
	Share(Box<Share>),
	/// [`Allocator::unlimited`]'s: no estimate, and never will be.
	Unavailable,
}

impl Consumer {
	/// Get the current bandwidth estimate synchronously.
	pub fn peek(&self) -> Option<Rate> {
		match &self.inner {
			Inner::Whole(state) => state.read().bitrate,
			Inner::Share(share) => share.grant(),
			Inner::Unavailable => None,
		}
	}

	/// Whether a session probes the connection for this slice's reservation, so the
	/// estimate can rise to fund it even while its sender is stopped.
	///
	/// Only a session whose transport has a sender probes, and none does while
	/// [`Allocator::set_target`] turns probing off. For a [`Viewer`]'s slice, whether
	/// the session asks a peer that advertised `Increase` to probe for it, which
	/// [`Viewers::set_target`] can turn off the same way. Without one the estimate
	/// stops learning once the connection is app-limited, so a sender that stops for
	/// lack of bandwidth could never see enough to resume. Always `false` for a whole
	/// estimate, which no reservation backs. [`poll_changed`](Self::poll_changed)
	/// reports a change to this too.
	pub fn probed(&self) -> bool {
		match &self.inner {
			Inner::Share(share) => share.probed(),
			Inner::Whole(_) | Inner::Unavailable => false,
		}
	}

	/// Poll for a bandwidth change without blocking.
	///
	/// `Ok(None)` means the estimate is unavailable *for now*: the backend
	/// stopped reporting one, or the handle spans reconnects and is between
	/// sessions. `Err` means the producer is gone and no further change will ever
	/// arrive. They're distinct because a caller holds its current rate for the
	/// first and stops watching for the second.
	///
	/// A slice is also reported, unchanged, when [`probed`](Self::probed) flips.
	///
	/// A backend with no bandwidth estimation at all yields no [Consumer] in the
	/// first place, so that case never reaches here.
	pub fn poll_changed(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Rate>>> {
		let last = self.last;

		let bitrate = match &mut self.inner {
			Inner::Whole(state) => match state.poll(waiter, |state| {
				if state.bitrate != last {
					Poll::Ready(state.bitrate)
				} else {
					Poll::Pending
				}
			}) {
				Poll::Ready(Ok(bitrate)) => bitrate,
				// Closed, and the value hasn't moved since the last read: report it as
				// terminal. Collapsing this into `Ok(None)` would be indistinguishable
				// from a live-but-unavailable estimate, and since a closed channel is
				// always immediately ready, a `select!` over it would spin forever.
				Poll::Ready(Err(_)) => return Poll::Ready(Err(Error::Dropped)),
				Poll::Pending => return Poll::Pending,
			},
			// A share recomputes its slice on every wakeup, so it filters the
			// unchanged case here rather than inside the poll. Every waker that could
			// move the slice was armed by `poll_grant` either way.
			// Nothing to report and nothing that could ever change it, so park without
			// arming anything rather than reporting a `None` the caller would re-read forever.
			Inner::Unavailable => return Poll::Pending,
			Inner::Share(share) => match share.poll_grant(waiter) {
				Poll::Ready(Ok((grant, probed))) if grant == last && probed == self.probed => return Poll::Pending,
				Poll::Ready(Ok((grant, probed))) => {
					self.probed = probed;
					grant
				}
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Pending => return Poll::Pending,
			},
		};

		self.last = bitrate;
		Poll::Ready(Ok(bitrate))
	}

	/// Block until the bandwidth estimate changes, returning the new value, or
	/// `None` when the estimate has become unavailable.
	///
	/// # Errors
	///
	/// Returns an error once the producer is closed or dropped, so a caller can
	/// stop watching. See [`poll_changed`](Self::poll_changed).
	#[cfg(feature = "async")]
	pub async fn changed(&mut self) -> Result<Option<Rate>> {
		kio::wait(|waiter| self.poll_changed(waiter)).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::broadcast;

	/// Priorities matching `hang`'s, which is what the allocator sees in practice.
	const AUDIO: i8 = 80;
	const VIDEO: i8 = 60;

	/// Bits per second, so the tables below stay readable.
	fn bps(bps: u64) -> Rate {
		Rate::from_bps(bps)
	}

	fn want(id: u64, priority: i8, max: u64) -> Want {
		Want {
			id,
			priority,
			max: bps(max),
		}
	}

	/// A standalone track at `priority`, plus the broadcast keeping it alive.
	fn track(priority: i8) -> (broadcast::Producer, track::Producer) {
		let broadcast = broadcast::Info::default().produce();
		let track = broadcast
			.create_track("t", track::Info::timed().with_priority(priority))
			.unwrap();
		(broadcast, track)
	}

	#[test]
	fn strict_priority_fills_the_top_tier_first() {
		let wants = [want(0, AUDIO, 128_000), want(1, VIDEO, 4_000_000)];

		// Audio takes its reservation off the top; video gets what's left. This is
		// the job `rate::Policy::headroom` used to approximate with a flat 10%.
		assert_eq!(allocate(bps(2_000_000), &wants, 0), Some(bps(128_000)));
		assert_eq!(allocate(bps(2_000_000), &wants, 1), Some(bps(1_872_000)));
	}

	#[test]
	fn a_starved_tier_gets_nothing() {
		let wants = [want(0, AUDIO, 2_000_000), want(1, VIDEO, 4_000_000)];

		assert_eq!(allocate(bps(1_000_000), &wants, 0), Some(bps(1_000_000)));
		// Strict, not weighted: the lower tier is not owed a floor. Its encoder
		// clamps to `rate::Policy::min` and the transport sheds what won't fit.
		assert_eq!(allocate(bps(1_000_000), &wants, 1), Some(bps(0)));
	}

	/// A publisher that doesn't set [`track::Info::priority`] puts audio and video
	/// in one tier. That has to come out right anyway, and it does: max-min fair
	/// satisfies the small claim first, so audio still gets its full reservation
	/// and video takes the rest. Priority only changes the answer once a tier's
	/// smaller claims outgrow an even split.
	#[test]
	fn one_tier_still_serves_audio_before_video() {
		let flat = [want(0, 0, 128_000), want(1, 0, 4_000_000)];
		let tiered = [want(0, AUDIO, 128_000), want(1, VIDEO, 4_000_000)];

		for wants in [flat, tiered] {
			assert_eq!(allocate(bps(2_000_000), &wants, 0), Some(bps(128_000)));
			assert_eq!(allocate(bps(2_000_000), &wants, 1), Some(bps(1_872_000)));
		}
	}

	#[test]
	fn an_even_tier_splits_evenly() {
		let wants = [want(0, VIDEO, 4_000_000), want(1, VIDEO, 4_000_000)];

		assert_eq!(allocate(bps(6_000_000), &wants, 0), Some(bps(3_000_000)));
		assert_eq!(allocate(bps(6_000_000), &wants, 1), Some(bps(3_000_000)));
	}

	/// Max-min fair, not an even split: a 360p rung sharing with a 1080p one takes
	/// only what it asked for and leaves the rest, instead of both being held to half.
	#[test]
	fn a_small_share_frees_what_it_does_not_want() {
		let wants = [want(0, VIDEO, 1_000_000), want(1, VIDEO, 8_000_000)];

		assert_eq!(allocate(bps(6_000_000), &wants, 0), Some(bps(1_000_000)));
		assert_eq!(allocate(bps(6_000_000), &wants, 1), Some(bps(5_000_000)));
	}

	/// Capping at the reservation is what keeps an uncongested link encoding at
	/// exactly the configured rate. Spreading the surplus instead would only matter
	/// if senders scaled a fraction of their grant, which is what headroom did.
	#[test]
	fn surplus_is_left_unclaimed() {
		assert_eq!(
			allocate(bps(10_000_000), &[want(0, VIDEO, 4_000_000)], 0),
			Some(bps(4_000_000))
		);
	}

	#[test]
	fn an_unregistered_share_has_no_grant() {
		assert_eq!(allocate(bps(1_000_000), &[], 0), None);
		assert_eq!(allocate(bps(1_000_000), &[want(0, VIDEO, 1_000)], 7), None);
	}

	/// The headline case: two encoders on one connection must not each target the
	/// whole estimate.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn concurrent_tracks_split_the_estimate() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());

		let (_first_broadcast, first) = track(VIDEO);
		let _first_sub = first.subscribe(None);
		let first = allocator.reserve(&first.demand(), bps(4_000_000));
		let mut first_share = first.consume();

		estimate.set(Some(bps(2_000_000))).unwrap();
		// Alone, it gets everything it asked for that the link can carry.
		assert_eq!(first_share.changed().await.unwrap(), Some(bps(2_000_000)));

		// A second encoder starts. The first must give half back without anyone
		// telling it to, or the two together target 200% of the uplink.
		let (_second_broadcast, second) = track(VIDEO);
		let _second_sub = second.subscribe(None);
		let second_share = allocator.reserve(&second.demand(), bps(4_000_000));

		assert_eq!(first_share.changed().await.unwrap(), Some(bps(1_000_000)));
		assert_eq!(second_share.rate(), Some(bps(1_000_000)));
	}

	/// An unwatched track releases its reservation to the others, since an on-demand sender
	/// stops encoding entirely while nothing is subscribed, but still reads the slice it
	/// would get back, which is what a stopped rendition its viewers left resumes on.
	#[test]
	fn an_idle_track_claims_nothing() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());
		estimate.set(Some(bps(2_000_000))).unwrap();

		let (_watched_broadcast, watched) = track(VIDEO);
		let _watched_sub = watched.subscribe(None);
		let watched_share = allocator.reserve(&watched.demand(), bps(4_000_000));

		let (_idle_broadcast, idle) = track(VIDEO);
		let idle_share = allocator.reserve(&idle.demand(), bps(4_000_000));

		assert_eq!(watched_share.rate(), Some(bps(2_000_000)));
		// What it would get if a viewer joined: an even split with the watched track.
		assert_eq!(idle_share.rate(), Some(bps(1_000_000)));

		// It joins, and the split happens.
		let _idle_sub = idle.subscribe(None);
		assert_eq!(watched_share.rate(), Some(bps(1_000_000)));
		assert_eq!(idle_share.rate(), Some(bps(1_000_000)));
	}

	/// Demand transitions have to wake a parked share, not just change what a later
	/// `peek` would see; the encoder is sitting in a `select!` on `changed`.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_share_wakes_when_a_sibling_goes_idle() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());
		estimate.set(Some(bps(2_000_000))).unwrap();

		let (_mine_broadcast, mine) = track(VIDEO);
		let _mine_sub = mine.subscribe(None);
		let mine_share_reserved = allocator.reserve(&mine.demand(), bps(4_000_000));
		let mut mine_share = mine_share_reserved.consume();

		let (_sibling_broadcast, sibling) = track(VIDEO);
		let sibling_sub = sibling.subscribe(None);
		let _sibling_share = allocator.reserve(&sibling.demand(), bps(4_000_000));

		assert_eq!(mine_share.changed().await.unwrap(), Some(bps(1_000_000)));

		// The sibling's last viewer leaves, so its half comes back to us.
		drop(sibling_sub);
		assert_eq!(mine_share.changed().await.unwrap(), Some(bps(2_000_000)));
	}

	/// Records whether a parked poll was actually woken, which re-reading state on a
	/// fresh `poll` can't tell you: a lost wakeup still looks correct on the next poll
	/// and only shows up as a task that never runs again.
	#[derive(Default)]
	#[cfg(feature = "async")]
	struct Woken(std::sync::atomic::AtomicBool);

	#[cfg(feature = "async")]
	impl std::task::Wake for Woken {
		fn wake(self: std::sync::Arc<Self>) {
			self.wake_by_ref();
		}

		fn wake_by_ref(self: &std::sync::Arc<Self>) {
			self.0.store(true, std::sync::atomic::Ordering::SeqCst);
		}
	}

	#[cfg(feature = "async")]
	impl Woken {
		/// A flag and its waiter. Both are returned because a [`kio::WaiterList`]
		/// holds only a `Weak`, so a waiter dropped at the end of the calling
		/// statement takes its own registration with it and never fires.
		fn new() -> (std::sync::Arc<Self>, kio::Waiter) {
			let flag = std::sync::Arc::new(Self::default());
			let waiter = kio::Waiter::new(std::task::Waker::from(flag.clone()));
			(flag, waiter)
		}

		fn woken(&self) -> bool {
			self.0.load(std::sync::atomic::Ordering::SeqCst)
		}
	}

	/// Regression: a share whose slice didn't move still has to leave the estimate's
	/// waker armed. Reservations cap the slice, so most estimate changes don't reach
	/// it, and a poll that returned the value without arming anything would park the
	/// encoder with nothing left to wake it.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn an_unchanged_slice_keeps_watching_the_estimate() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());

		let (_broadcast, track) = track(VIDEO);
		let _sub = track.subscribe(None);
		let share_reserved = allocator.reserve(&track.demand(), bps(4_000_000));
		let mut share = share_reserved.consume();

		estimate.set(Some(bps(10_000_000))).unwrap();
		assert_eq!(share.changed().await.unwrap(), Some(bps(4_000_000)));

		// Still miles above the reservation, so the slice holds at 4 Mbps: the share
		// observes the change, decides it doesn't move, and parks.
		let (woken, waiter) = Woken::new();
		estimate.set(Some(bps(9_000_000))).unwrap();
		assert!(share.poll_changed(&waiter).is_pending());

		// The estimate finally drops past the reservation: this has to reach it.
		estimate.set(Some(bps(1_000_000))).unwrap();
		assert!(woken.woken(), "a parked share must be woken by the next estimate");
		assert_eq!(share.changed().await.unwrap(), Some(bps(1_000_000)));
	}

	/// The same, for the other input: a sibling's demand.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_parked_share_is_woken_by_sibling_demand() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());
		estimate.set(Some(bps(2_000_000))).unwrap();

		let (_mine_broadcast, mine) = track(VIDEO);
		let _mine_sub = mine.subscribe(None);
		let share_reserved = allocator.reserve(&mine.demand(), bps(4_000_000));
		let mut share = share_reserved.consume();

		let (_sibling_broadcast, sibling) = track(VIDEO);
		let sibling_sub = sibling.subscribe(None);
		let _sibling_share = allocator.reserve(&sibling.demand(), bps(4_000_000));

		assert_eq!(share.changed().await.unwrap(), Some(bps(1_000_000)));

		let (woken, waiter) = Woken::new();
		assert!(share.poll_changed(&waiter).is_pending());
		drop(sibling_sub);
		assert!(woken.woken(), "a sibling going idle must wake a parked share");
	}

	/// A share follows the estimate's own lifecycle: unavailable while disconnected
	/// (hold the current rate), terminal once the session is gone for good.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_share_follows_the_estimate_lifecycle() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());

		let (_broadcast, track) = track(VIDEO);
		let _sub = track.subscribe(None);
		let share_reserved = allocator.reserve(&track.demand(), bps(4_000_000));
		let mut share = share_reserved.consume();

		estimate.set(Some(bps(2_000_000))).unwrap();
		assert_eq!(share.changed().await.unwrap(), Some(bps(2_000_000)));

		estimate.set(None).unwrap();
		assert_eq!(share.changed().await.unwrap(), None);

		drop(estimate);
		assert!(share.changed().await.is_err());
		assert!(share.changed().await.is_err());
	}

	/// A closed track's entry can't linger: it would be walked on every poll for the
	/// rest of the connection, and a long-lived publisher churns tracks.
	#[test]
	fn a_closed_track_is_pruned() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());

		// Both shares are held: dropping one releases its reservation on its own,
		// which would prove nothing about pruning the track that closed.
		let (_first_broadcast, first) = track(VIDEO);
		let _first_share = allocator.reserve(&first.demand(), bps(4_000_000));
		first.abort(Error::Cancel).unwrap();

		let (_second_broadcast, second) = track(VIDEO);
		let _second_share = allocator.reserve(&second.demand(), bps(4_000_000));

		assert_eq!(allocator.registry.read().entries.len(), 1);
	}

	/// Dropping the reservation hands the room back. The registry only ever prunes
	/// tracks that have *closed*, so a claim left behind by a track that is still
	/// publishing would stand for the rest of the connection.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn dropping_a_reservation_releases_it() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());
		estimate.set(Some(bps(2_000_000))).unwrap();

		let (_first_broadcast, first) = track(VIDEO);
		let _first_sub = first.subscribe(None);
		let first_reserved = allocator.reserve(&first.demand(), bps(4_000_000));

		let (_second_broadcast, second) = track(VIDEO);
		let _second_sub = second.subscribe(None);
		let second_reserved = allocator.reserve(&second.demand(), bps(4_000_000));
		assert_eq!(second_reserved.rate(), Some(bps(1_000_000)));

		// A read handle is not the claim: the reservation outliving it is what keeps the
		// room, and the reservation going away is what returns it.
		let mut orphan = first_reserved.consume();
		assert_eq!(orphan.changed().await.unwrap(), Some(bps(1_000_000)));

		drop(first_reserved);
		assert_eq!(allocator.registry.read().entries.len(), 1);
		assert_eq!(second_reserved.rate(), Some(bps(2_000_000)));

		// The orphaned reader is woken and told, rather than being left parked on a slice
		// that will never move again. It reports the same "hold your rate" as a missing
		// estimate, not a grant of zero that would tell an encoder to stop.
		assert_eq!(orphan.changed().await.unwrap(), None);
		assert_eq!(orphan.peek(), None);
	}

	/// A sender whose ceiling moved (a capture reopening at a resolution it negotiated
	/// with the device) changes the claim in place. Re-reserving instead would claim
	/// twice, since nothing releases the first entry while the track is still alive.
	#[test]
	fn update_changes_the_claim_in_place() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());
		estimate.set(Some(bps(6_000_000))).unwrap();

		let (_small_broadcast, small) = track(VIDEO);
		let _small_sub = small.subscribe(None);
		let small_reserved = allocator.reserve(&small.demand(), bps(1_000_000));

		let (_large_broadcast, large) = track(VIDEO);
		let _large_sub = large.subscribe(None);
		let large_reserved = allocator.reserve(&large.demand(), bps(8_000_000));

		// Max-min fair: the small claim is satisfied in full, the rest goes to the other.
		assert_eq!(small_reserved.rate(), Some(bps(1_000_000)));
		assert_eq!(large_reserved.rate(), Some(bps(5_000_000)));

		// It reopens at a mode that can use much more, and the split follows without a
		// second entry appearing.
		small_reserved.update(bps(4_000_000));
		assert_eq!(allocator.registry.read().entries.len(), 2);
		assert_eq!(small_reserved.rate(), Some(bps(3_000_000)));
		assert_eq!(large_reserved.rate(), Some(bps(3_000_000)));

		// And back down, which has to release the difference rather than hold it.
		small_reserved.update(bps(1_000_000));
		assert_eq!(large_reserved.rate(), Some(bps(5_000_000)));
	}

	/// Collected-language callers can relinquish a ceiling before their retained
	/// handle is finalized, without ending the track or waiting for its subscribers.
	#[test]
	fn zero_ceiling_releases_share_and_probe_target() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());
		estimate.set(Some(bps(3_000_000))).unwrap();
		let mut target = allocator.target();
		let waiter = kio::Waiter::noop();

		let (_encoder_broadcast, encoder) = track(VIDEO);
		let _encoder_sub = encoder.subscribe(None);
		let retired = allocator.reserve(&encoder.demand(), bps(4_000_000));
		let replacement = allocator.reserve(&encoder.demand(), bps(4_000_000));
		let (_sibling_broadcast, sibling) = track(VIDEO);
		let _sibling_sub = sibling.subscribe(None);
		let sibling = allocator.reserve(&sibling.demand(), bps(4_000_000));
		assert_eq!(sibling.rate(), Some(bps(1_000_000)));
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(bps(12_000_000)));

		retired.update(Rate::ZERO);
		assert_eq!(retired.rate(), Some(Rate::ZERO));
		assert_eq!(replacement.rate(), Some(bps(1_500_000)));
		assert_eq!(sibling.rate(), Some(bps(1_500_000)));
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(bps(8_000_000)));
		retired.update(Rate::ZERO);
		assert!(target.poll_changed(&waiter).is_pending());

		// The registration remains usable, so an encoder may resume in place.
		retired.update(bps(4_000_000));
		assert_eq!(sibling.rate(), Some(bps(1_000_000)));
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(bps(12_000_000)));
		assert!(encoder.demand().is_wanted());
	}

	/// A reader parked on `changed` has to be woken by its own reservation moving, not
	/// just by the estimate or a sibling: the encoder is sitting in a `select!` on it.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn update_wakes_a_parked_reader() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());
		estimate.set(Some(bps(6_000_000))).unwrap();

		let (_broadcast, producer) = track(VIDEO);
		let _sub = producer.subscribe(None);
		let reserved = allocator.reserve(&producer.demand(), bps(1_000_000));
		let mut share = reserved.consume();
		assert_eq!(share.changed().await.unwrap(), Some(bps(1_000_000)));

		let (woken, waiter) = Woken::new();
		assert!(share.poll_changed(&waiter).is_pending());

		reserved.update(bps(4_000_000));
		assert!(woken.woken(), "raising the ceiling must wake the reader");
		assert_eq!(share.changed().await.unwrap(), Some(bps(4_000_000)));
	}

	/// The registry is the allocator's, not a share's: a share that outlives every
	/// allocator reports the estimate as gone rather than keeping the channel open.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_share_outliving_the_allocator_reports_closed() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());

		let (_broadcast, producer) = track(VIDEO);
		let _sub = producer.subscribe(None);
		let share_reserved = allocator.reserve(&producer.demand(), bps(4_000_000));
		let mut share = share_reserved.consume();

		drop(allocator);
		drop(estimate);
		assert!(share.changed().await.is_err());
	}

	/// An unavailable estimate and a dead producer must not look alike: a caller
	/// holds its rate for the former and stops watching for the latter.
	/// Reporting closure as `Ok(None)` would spin any `select!` over `changed()`,
	/// because a closed channel is always immediately ready.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn closed_is_distinct_from_unavailable() {
		let producer = Producer::new();
		let mut consumer = producer.consume();

		producer.set(Some(bps(1_000_000))).unwrap();
		assert_eq!(consumer.changed().await.unwrap(), Some(bps(1_000_000)));

		// Live, but the estimate went away (e.g. disconnected): still watchable.
		producer.set(None).unwrap();
		assert_eq!(consumer.changed().await.unwrap(), None);

		// Gone for good.
		drop(producer);
		assert!(consumer.changed().await.is_err());
		// And it stays terminal rather than flapping back to a value.
		assert!(consumer.changed().await.is_err());
	}

	/// The target sums every reservation, demanded or not, and follows each one
	/// being added, moved, released, or pruned with its track, waking the session
	/// that follows it.
	#[test]
	#[cfg(feature = "async")]
	fn the_target_sums_every_reservation() {
		let allocator = Allocator::unlimited();
		let mut target = allocator.target();
		let (woken, waiter) = Woken::new();
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(Rate::ZERO));
		assert!(target.poll_changed(&waiter).is_pending());

		let (_watched_broadcast, watched) = track(VIDEO);
		let _sub = watched.subscribe(None);
		let watched = allocator.reserve(&watched.demand(), bps(4_000_000));
		assert!(woken.woken(), "a reservation must wake the session");
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(bps(4_000_000)));

		// Nobody watches this one, which is exactly the rung that must still be funded.
		let (_idle_broadcast, idle_track) = track(VIDEO);
		let idle = allocator.reserve(&idle_track.demand(), bps(1_000_000));
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(bps(5_000_000)));

		idle.update(bps(2_000_000));
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(bps(6_000_000)));

		drop(watched);
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(bps(2_000_000)));

		// A closed track's claim leaves with the next registration, even while its
		// reservation is still held.
		idle_track.abort(Error::Cancel).unwrap();
		let (_other_broadcast, other) = track(VIDEO);
		let _other = allocator.reserve(&other.demand(), bps(500_000));
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(bps(500_000)));
		drop(idle);
		assert!(
			target.poll_changed(&waiter).is_pending(),
			"a pruned claim is not released twice"
		);
	}

	/// A reservation is probed while a session follows its allocator's target, and a
	/// reader parked on an unchanged slice is woken when that flips.
	#[test]
	#[cfg(feature = "async")]
	fn a_following_session_marks_reservations_probed() {
		let estimate = Producer::new();
		estimate.set(Some(bps(2_000_000))).unwrap();
		let allocator = Allocator::new(estimate.consume());
		let (_broadcast, producer) = track(VIDEO);
		let _sub = producer.subscribe(None);
		let reservation = allocator.reserve(&producer.demand(), bps(4_000_000));
		let mut grant = reservation.consume();
		let (woken, waiter) = Woken::new();
		assert!(matches!(grant.poll_changed(&waiter), Poll::Ready(Ok(Some(rate))) if rate == bps(2_000_000)));
		assert!(!grant.probed());
		assert!(grant.poll_changed(&waiter).is_pending());

		let target = allocator.target();
		assert!(woken.woken(), "a session starting to probe must wake the reader");
		assert!(matches!(grant.poll_changed(&waiter), Poll::Ready(Ok(Some(rate))) if rate == bps(2_000_000)));
		assert!(grant.probed());
		assert!(grant.poll_changed(&waiter).is_pending());

		// A zero target turns probing off, though the session still follows it.
		let (woken, waiter) = Woken::new();
		assert!(grant.poll_changed(&waiter).is_pending());
		allocator.set_target(Some(Rate::ZERO));
		assert!(woken.woken(), "turning probing off must wake the reader");
		assert!(matches!(grant.poll_changed(&waiter), Poll::Ready(Ok(Some(rate))) if rate == bps(2_000_000)));
		assert!(!grant.probed());
		allocator.set_target(None);
		assert!(matches!(grant.poll_changed(&waiter), Poll::Ready(Ok(Some(rate))) if rate == bps(2_000_000)));
		assert!(grant.probed());

		drop(target);
		assert!(matches!(grant.poll_changed(&waiter), Poll::Ready(Ok(Some(rate))) if rate == bps(2_000_000)));
		assert!(!grant.probed());
		assert!(!estimate.consume().probed(), "a whole estimate is never probed");
	}

	/// The override replaces the sum for every clone until it is cleared.
	#[test]
	fn an_override_replaces_the_sum() {
		let allocator = Allocator::unlimited();
		let mut target = allocator.target();
		let waiter = kio::Waiter::noop();

		let (_broadcast, producer) = track(VIDEO);
		let _reserved = allocator.reserve(&producer.demand(), bps(4_000_000));
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(bps(4_000_000)));

		allocator.clone().set_target(Some(Rate::ZERO));
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(Rate::ZERO));

		allocator.set_target(None);
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(bps(4_000_000)));
	}

	/// Several broadcasts on one connection each read their own slice of the receive
	/// estimate, max-min fair by ceiling, so together they never claim more than it.
	#[test]
	fn viewers_split_the_estimate_max_min_fair() {
		let estimate = Producer::new();
		let viewers = Viewers::new(estimate.weak());
		let small = viewers.watch(bps(1_000_000));
		let large = viewers.watch(bps(8_000_000));
		assert_eq!(small.rate(), None, "no estimate yet: hold the selection");

		estimate.set(Some(bps(6_000_000))).unwrap();
		assert_eq!(small.rate(), Some(bps(1_000_000)));
		assert_eq!(large.rate(), Some(bps(5_000_000)));

		let third = viewers.watch(bps(4_000_000));
		let grants = [&small, &large, &third].map(|viewer| viewer.rate().unwrap());
		assert_eq!(grants, [bps(1_000_000), bps(2_500_000), bps(2_500_000)]);
		assert!(grants.iter().map(|rate| rate.as_bps()).sum::<u64>() <= 6_000_000);

		// A viewer that leaves hands its slice back.
		drop(third);
		assert_eq!(large.rate(), Some(bps(5_000_000)));
	}

	/// The PROBE target sums every live viewer's ceiling until overridden.
	#[test]
	fn the_viewers_target_sums_their_ceilings() {
		let viewers = Viewers::default();
		let mut target = viewers.target();
		let waiter = kio::Waiter::noop();
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(Rate::ZERO));

		let first = viewers.watch(bps(3_000_000));
		let second = viewers.clone().watch(bps(2_000_000));
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(bps(5_000_000)));
		first.update(bps(1_000_000));
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(bps(3_000_000)));

		viewers.set_target(Some(Rate::ZERO));
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(Rate::ZERO));
		viewers.set_target(None);
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(bps(3_000_000)));

		drop(second);
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(bps(1_000_000)));
		drop(first);
		assert_eq!(target.poll_changed(&waiter), Poll::Ready(Rate::ZERO));
	}

	/// A held viewer is what requests the receive estimate (and so PROBE); the registry
	/// alone is not, so a session holding one opens no PROBE stream.
	#[test]
	fn a_viewer_requests_the_estimate() {
		let estimate = Producer::new();
		let viewers = Viewers::new(estimate.weak());
		assert!(!estimate.demand().wanted());

		let viewer = viewers.watch(bps(1_000_000));
		assert!(estimate.demand().wanted());
		let grant = viewer.consume();
		drop(viewer);
		assert!(
			estimate.demand().wanted(),
			"a grant read from the viewer still wants it"
		);
		drop(grant);
		assert!(!estimate.demand().wanted());
	}
}
