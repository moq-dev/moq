//! Bandwidth estimation, split into a [Producer] and [Consumer] handle.
//!
//! A [Producer] is used to set the current estimated bitrate, notifying consumers.
//! A [Consumer] can read the current estimate and wait for changes.
//!
//! One estimate covers a whole connection, so senders sharing one divide it with
//! an [Allocator] rather than each targeting the whole thing. How a sender then
//! *follows* its share is policy and lives with the sender: see
//! `moq_video::encode::rate` for the encoder's.

use std::task::Poll;

use crate::{Error, Result, track};

#[derive(Default)]
struct State {
	bitrate: Option<u64>,
	abort: Option<Error>,
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

	/// Set the current bandwidth estimate in bits per second.
	pub fn set(&self, bitrate: Option<u64>) -> Result<()> {
		let mut state = self.modify()?;
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
		}
	}

	/// Close the producer with an error, notifying all consumers.
	pub fn abort(&self, err: Error) -> Result<()> {
		let mut state = self.modify()?;
		state.abort = Some(err);
		state.close();
		Ok(())
	}

	/// Block until the channel is closed.
	pub async fn closed(&self) {
		self.state.closed().await
	}

	/// Block until there are no active consumers.
	pub async fn unused(&self) -> Result<()> {
		kio::wait(|waiter| self.poll_unused(waiter)).await
	}

	/// Poll until there are no active consumers. Errors if the channel closes first.
	pub fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<Result<()>> {
		self.state.poll_unused(waiter).map(|used| match used {
			Some(()) => Ok(()),
			None => Err(self.close_error()),
		})
	}

	/// Block until there is at least one active consumer.
	pub async fn used(&self) -> Result<()> {
		kio::wait(|waiter| self.poll_used(waiter)).await
	}

	/// Poll until at least one active consumer exists. Errors if the channel closes first.
	pub fn poll_used(&self, waiter: &kio::Waiter) -> Poll<Result<()>> {
		self.state.poll_used(waiter).map(|used| match used {
			Some(()) => Ok(()),
			None => Err(self.close_error()),
		})
	}

	fn modify(&self) -> Result<kio::Mut<'_, State>> {
		self.state
			.write()
			.map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
	}

	/// The close error, once the channel is closed.
	fn close_error(&self) -> Error {
		self.state.read().abort.clone().unwrap_or(Error::Dropped)
	}
}

impl Default for Producer {
	fn default() -> Self {
		Self::new()
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
/// sheds the excess by dropping groups. Bandwidth estimation isn't exact enough
/// for the difference to be worth policing.
///
/// Clones share one registry, so hand a clone to each sender.
#[derive(Clone)]
pub struct Allocator {
	estimate: Consumer,
	registry: kio::Producer<Registry>,
}

impl Allocator {
	/// Divide `estimate`, normally a connection's
	/// [`Session::send_bandwidth`](crate::Session::send_bandwidth).
	///
	/// Nesting works: the estimate can itself be a share, which splits one slice
	/// again among the tracks behind it.
	pub fn new(estimate: Consumer) -> Self {
		Self {
			estimate,
			registry: kio::Producer::default(),
		}
	}

	/// Reserve up to `max` bits per second for `track`, returning that track's slice.
	///
	/// `max` is a ceiling, not a measurement: reserve the most the track can ever
	/// send, not what it happens to be sending. A VBR encoder sitting on a black
	/// screen at 1 Mbps can jump to 6 Mbps between one frame and the next, and a
	/// reservation that had followed it down would have already handed that room
	/// to somebody else.
	///
	/// Priority comes from the track ([`track::Info::priority`], higher served
	/// first), so allocation and transmission order can't disagree. A tier is
	/// filled to its reservations before the next one sees a bit; within a tier
	/// the split is max-min fair, so a share asking for less than an even cut
	/// takes all of it and leaves the difference to the others.
	///
	/// That last part is what carries the common case, since publishers leave
	/// `priority` at its default today: one tier of audio and video still serves
	/// audio's small reservation in full before video takes the remainder.
	///
	/// The returned [`Consumer`] reports `None` while nothing is subscribed to the
	/// track or the connection has no estimate, which tells a sender to hold its
	/// current rate rather than to encode at zero. The share is released when the
	/// track closes: a [`track::Demand`] is a weak handle, so registering one never
	/// keeps a track alive.
	pub fn register(&self, track: &track::Demand, max: u64) -> Consumer {
		// Read the track before taking the registry lock, so the two are never held
		// at once and there's no order to get wrong.
		let priority = track.priority();
		let demand = track.clone();

		let id = {
			// Nothing ever closes this channel: the allocator holds the only producer
			// and never aborts it, so it's open for as long as `self` is.
			let Ok(mut registry) = self.registry.write() else {
				unreachable!("the allocator holds its own registry producer")
			};
			// Closed tracks can't be demanded again; drop them rather than walking
			// them on every poll for the rest of the connection.
			registry.entries.retain(|entry| !entry.demand.is_closed());

			let id = registry.next_id;
			registry.next_id += 1;
			registry.entries.push(Entry {
				id,
				demand,
				priority,
				max,
			});
			id
		};

		Consumer {
			inner: Inner::Share(Box::new(Share {
				estimate: self.estimate.clone(),
				registry: self.registry.consume(),
				id,
			})),
			last: None,
		}
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

/// Every track registered with one [`Allocator`].
#[derive(Default)]
struct Registry {
	entries: Vec<Entry>,
	next_id: u64,
}

/// One registered track's standing reservation.
struct Entry {
	id: u64,
	demand: track::Demand,
	priority: u8,
	max: u64,
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
	priority: u8,
	max: u64,
}

impl Share {
	/// This share's slice right now.
	fn grant(&self) -> Option<u64> {
		let estimate = self.estimate.peek()?;
		let wants: Vec<Want> = self
			.claims()
			.into_iter()
			.filter(|(_, demand)| demand.is_used())
			.map(|(want, _)| want)
			.collect();
		allocate(estimate, &wants, self.id)
	}

	/// This share's slice, arming `waiter` for everything that could move it: the
	/// estimate, the set of registered tracks, and each track's demand.
	fn poll_grant(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<u64>>> {
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
			.filter(|(_, demand)| match demand.poll_state(waiter) {
				track::DemandState::Active => true,
				// An idle track claims nothing, so an unwatched encoder's reservation
				// goes to whoever is actually sending.
				track::DemandState::Idle => false,
				// Gone for good, and pruned by the next `register`.
				track::DemandState::Closed => false,
			})
			.map(|(want, _)| want)
			.collect();

		let grant = self
			.estimate
			.peek()
			.and_then(|estimate| allocate(estimate, &wants, self.id));
		Poll::Ready(Ok(grant))
	}

	/// Snapshot the registry so the tracks can be read without holding its lock.
	fn claims(&self) -> Vec<(Want, track::Demand)> {
		self.registry
			.read()
			.entries
			.iter()
			.map(|entry| {
				(
					Want {
						id: entry.id,
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
/// `None` when `id` isn't among the wants, which is how an idle or closed track
/// reports "hold your rate" instead of a grant of zero.
fn allocate(estimate: u64, wants: &[Want], id: u64) -> Option<u64> {
	let mut budget = estimate;
	let mut tier = wants.iter().map(|want| want.priority).max();

	while let Some(priority) = tier {
		// Ascending by reservation: each share takes an even cut of what's left, or
		// all it asked for if that's less, which frees the difference for the rest.
		let mut members: Vec<&Want> = wants.iter().filter(|want| want.priority == priority).collect();
		members.sort_by_key(|want| want.max);

		let mut remaining = members.len() as u64;
		for want in members {
			let even = budget / remaining;
			let grant = want.max.min(even);
			if want.id == id {
				return Some(grant);
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
	last: Option<u64>,
}

/// What a [`Consumer`] is reading: the whole estimate, or one track's slice of it.
#[derive(Clone)]
enum Inner {
	Whole(kio::Consumer<State>),
	Share(Box<Share>),
}

impl Consumer {
	/// Get the current bandwidth estimate synchronously.
	pub fn peek(&self) -> Option<u64> {
		match &self.inner {
			Inner::Whole(state) => state.read().bitrate,
			Inner::Share(share) => share.grant(),
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
	/// A backend with no bandwidth estimation at all yields no [Consumer] in the
	/// first place, so that case never reaches here.
	pub fn poll_changed(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<u64>>> {
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
				Poll::Ready(Err(state)) => return Poll::Ready(Err(state.abort.clone().unwrap_or(Error::Dropped))),
				Poll::Pending => return Poll::Pending,
			},
			// A share recomputes its slice on every wakeup, so it filters the
			// unchanged case here rather than inside the poll. Every waker that could
			// move the slice was armed by `poll_grant` either way.
			Inner::Share(share) => match share.poll_grant(waiter) {
				Poll::Ready(Ok(grant)) if grant == last => return Poll::Pending,
				Poll::Ready(Ok(grant)) => grant,
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
	pub async fn changed(&mut self) -> Result<Option<u64>> {
		kio::wait(|waiter| self.poll_changed(waiter)).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::broadcast;

	/// Priorities matching `hang`'s, which is what the allocator sees in practice.
	const AUDIO: u8 = 80;
	const VIDEO: u8 = 60;

	fn want(id: u64, priority: u8, max: u64) -> Want {
		Want { id, priority, max }
	}

	/// A standalone track at `priority`, plus the broadcast keeping it alive.
	fn track(priority: u8) -> (broadcast::Producer, track::Producer) {
		let mut broadcast = broadcast::Info::default().produce();
		let track = broadcast
			.create_track("t", track::Info::default().with_priority(priority))
			.unwrap();
		(broadcast, track)
	}

	#[test]
	fn strict_priority_fills_the_top_tier_first() {
		let wants = [want(0, AUDIO, 128_000), want(1, VIDEO, 4_000_000)];

		// Audio takes its reservation off the top; video gets what's left. This is
		// the job `rate::Policy::headroom` used to approximate with a flat 10%.
		assert_eq!(allocate(2_000_000, &wants, 0), Some(128_000));
		assert_eq!(allocate(2_000_000, &wants, 1), Some(1_872_000));
	}

	#[test]
	fn a_starved_tier_gets_nothing() {
		let wants = [want(0, AUDIO, 2_000_000), want(1, VIDEO, 4_000_000)];

		assert_eq!(allocate(1_000_000, &wants, 0), Some(1_000_000));
		// Strict, not weighted: the lower tier is not owed a floor. Its encoder
		// clamps to `rate::Policy::min` and the transport sheds what won't fit.
		assert_eq!(allocate(1_000_000, &wants, 1), Some(0));
	}

	/// Publishers don't set [`track::Info::priority`] today (it defaults to 0 and
	/// `hang::container::track_info` leaves it there), so audio and video land in
	/// one tier. That has to come out right anyway, and it does: max-min fair
	/// satisfies the small claim first, so audio still gets its full reservation
	/// and video takes the rest. Priority only changes the answer once a tier's
	/// smaller claims outgrow an even split.
	#[test]
	fn one_tier_still_serves_audio_before_video() {
		let flat = [want(0, 0, 128_000), want(1, 0, 4_000_000)];
		let tiered = [want(0, AUDIO, 128_000), want(1, VIDEO, 4_000_000)];

		for wants in [flat, tiered] {
			assert_eq!(allocate(2_000_000, &wants, 0), Some(128_000));
			assert_eq!(allocate(2_000_000, &wants, 1), Some(1_872_000));
		}
	}

	#[test]
	fn an_even_tier_splits_evenly() {
		let wants = [want(0, VIDEO, 4_000_000), want(1, VIDEO, 4_000_000)];

		assert_eq!(allocate(6_000_000, &wants, 0), Some(3_000_000));
		assert_eq!(allocate(6_000_000, &wants, 1), Some(3_000_000));
	}

	/// Max-min fair, not an even split: a 360p rung sharing with a 1080p one takes
	/// only what it asked for and leaves the rest, instead of both being held to half.
	#[test]
	fn a_small_share_frees_what_it_does_not_want() {
		let wants = [want(0, VIDEO, 1_000_000), want(1, VIDEO, 8_000_000)];

		assert_eq!(allocate(6_000_000, &wants, 0), Some(1_000_000));
		assert_eq!(allocate(6_000_000, &wants, 1), Some(5_000_000));
	}

	/// Capping at the reservation is what keeps an uncongested link encoding at
	/// exactly the configured rate. Spreading the surplus instead would only matter
	/// if senders scaled a fraction of their grant, which is what headroom did.
	#[test]
	fn surplus_is_left_unclaimed() {
		assert_eq!(allocate(10_000_000, &[want(0, VIDEO, 4_000_000)], 0), Some(4_000_000));
	}

	#[test]
	fn an_unregistered_share_has_no_grant() {
		assert_eq!(allocate(1_000_000, &[], 0), None);
		assert_eq!(allocate(1_000_000, &[want(0, VIDEO, 1_000)], 7), None);
	}

	/// The headline case: two encoders on one connection must not each target the
	/// whole estimate.
	#[tokio::test]
	async fn concurrent_tracks_split_the_estimate() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());

		let (_first_broadcast, first) = track(VIDEO);
		let _first_sub = first.consume();
		let mut first_share = allocator.register(&first.demand(), 4_000_000);

		estimate.set(Some(2_000_000)).unwrap();
		// Alone, it gets everything it asked for that the link can carry.
		assert_eq!(first_share.changed().await.unwrap(), Some(2_000_000));

		// A second encoder starts. The first must give half back without anyone
		// telling it to, or the two together target 200% of the uplink.
		let (_second_broadcast, second) = track(VIDEO);
		let _second_sub = second.consume();
		let second_share = allocator.register(&second.demand(), 4_000_000);

		assert_eq!(first_share.changed().await.unwrap(), Some(1_000_000));
		assert_eq!(second_share.peek(), Some(1_000_000));
	}

	/// An unwatched track releases its reservation, since `publish_capture` stops
	/// encoding entirely while nothing is subscribed.
	#[tokio::test]
	async fn an_idle_track_claims_nothing() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());
		estimate.set(Some(2_000_000)).unwrap();

		let (_watched_broadcast, watched) = track(VIDEO);
		let _watched_sub = watched.consume();
		let watched_share = allocator.register(&watched.demand(), 4_000_000);

		let (_idle_broadcast, idle) = track(VIDEO);
		let idle_share = allocator.register(&idle.demand(), 4_000_000);

		assert_eq!(watched_share.peek(), Some(2_000_000));
		// Not zero: an idle share reports "no opinion" so a sender that is mid-shutdown
		// holds its rate instead of retuning to the floor on the way out.
		assert_eq!(idle_share.peek(), None);

		// It joins, and the split happens.
		let _idle_sub = idle.consume();
		assert_eq!(watched_share.peek(), Some(1_000_000));
		assert_eq!(idle_share.peek(), Some(1_000_000));
	}

	/// Demand transitions have to wake a parked share, not just change what a later
	/// `peek` would see; the encoder is sitting in a `select!` on `changed`.
	#[tokio::test]
	async fn a_share_wakes_when_a_sibling_goes_idle() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());
		estimate.set(Some(2_000_000)).unwrap();

		let (_mine_broadcast, mine) = track(VIDEO);
		let _mine_sub = mine.consume();
		let mut mine_share = allocator.register(&mine.demand(), 4_000_000);

		let (_sibling_broadcast, sibling) = track(VIDEO);
		let sibling_sub = sibling.consume();
		let _sibling_share = allocator.register(&sibling.demand(), 4_000_000);

		assert_eq!(mine_share.changed().await.unwrap(), Some(1_000_000));

		// The sibling's last viewer leaves, so its half comes back to us.
		drop(sibling_sub);
		assert_eq!(mine_share.changed().await.unwrap(), Some(2_000_000));
	}

	/// Records whether a parked poll was actually woken, which re-reading state on a
	/// fresh `poll` can't tell you: a lost wakeup still looks correct on the next poll
	/// and only shows up as a task that never runs again.
	#[derive(Default)]
	struct Woken(std::sync::atomic::AtomicBool);

	impl std::task::Wake for Woken {
		fn wake(self: std::sync::Arc<Self>) {
			self.wake_by_ref();
		}

		fn wake_by_ref(self: &std::sync::Arc<Self>) {
			self.0.store(true, std::sync::atomic::Ordering::SeqCst);
		}
	}

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
	#[tokio::test]
	async fn an_unchanged_slice_keeps_watching_the_estimate() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());

		let (_broadcast, track) = track(VIDEO);
		let _sub = track.consume();
		let mut share = allocator.register(&track.demand(), 4_000_000);

		estimate.set(Some(10_000_000)).unwrap();
		assert_eq!(share.changed().await.unwrap(), Some(4_000_000));

		// Still miles above the reservation, so the slice holds at 4 Mbps: the share
		// observes the change, decides it doesn't move, and parks.
		let (woken, waiter) = Woken::new();
		estimate.set(Some(9_000_000)).unwrap();
		assert!(share.poll_changed(&waiter).is_pending());

		// The estimate finally drops past the reservation: this has to reach it.
		estimate.set(Some(1_000_000)).unwrap();
		assert!(woken.woken(), "a parked share must be woken by the next estimate");
		assert_eq!(share.changed().await.unwrap(), Some(1_000_000));
	}

	/// The same, for the other input: a sibling's demand.
	#[tokio::test]
	async fn a_parked_share_is_woken_by_sibling_demand() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());
		estimate.set(Some(2_000_000)).unwrap();

		let (_mine_broadcast, mine) = track(VIDEO);
		let _mine_sub = mine.consume();
		let mut share = allocator.register(&mine.demand(), 4_000_000);

		let (_sibling_broadcast, sibling) = track(VIDEO);
		let sibling_sub = sibling.consume();
		let _sibling_share = allocator.register(&sibling.demand(), 4_000_000);

		assert_eq!(share.changed().await.unwrap(), Some(1_000_000));

		let (woken, waiter) = Woken::new();
		assert!(share.poll_changed(&waiter).is_pending());
		drop(sibling_sub);
		assert!(woken.woken(), "a sibling going idle must wake a parked share");
	}

	/// A share follows the estimate's own lifecycle: unavailable while disconnected
	/// (hold the current rate), terminal once the session is gone for good.
	#[tokio::test]
	async fn a_share_follows_the_estimate_lifecycle() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());

		let (_broadcast, track) = track(VIDEO);
		let _sub = track.consume();
		let mut share = allocator.register(&track.demand(), 4_000_000);

		estimate.set(Some(2_000_000)).unwrap();
		assert_eq!(share.changed().await.unwrap(), Some(2_000_000));

		estimate.set(None).unwrap();
		assert_eq!(share.changed().await.unwrap(), None);

		estimate.abort(Error::Cancel).unwrap();
		assert!(share.changed().await.is_err());
		assert!(share.changed().await.is_err());
	}

	/// A closed track's entry can't linger: it would be walked on every poll for the
	/// rest of the connection, and a long-lived publisher churns tracks.
	#[tokio::test]
	async fn a_closed_track_is_pruned() {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());

		let (_first_broadcast, first) = track(VIDEO);
		allocator.register(&first.demand(), 4_000_000);
		first.abort(Error::Cancel).unwrap();

		let (_second_broadcast, second) = track(VIDEO);
		allocator.register(&second.demand(), 4_000_000);

		assert_eq!(allocator.registry.read().entries.len(), 1);
	}

	/// An unavailable estimate and a dead producer must not look alike: a caller
	/// holds its rate for the former and stops watching for the latter.
	/// Reporting closure as `Ok(None)` would spin any `select!` over `changed()`,
	/// because a closed channel is always immediately ready.
	#[tokio::test]
	async fn closed_is_distinct_from_unavailable() {
		let producer = Producer::new();
		let mut consumer = producer.consume();

		producer.set(Some(1_000_000)).unwrap();
		assert_eq!(consumer.changed().await.unwrap(), Some(1_000_000));

		// Live, but the estimate went away (e.g. disconnected): still watchable.
		producer.set(None).unwrap();
		assert_eq!(consumer.changed().await.unwrap(), None);

		// Gone for good.
		producer.abort(Error::Cancel).unwrap();
		assert!(consumer.changed().await.is_err());
		// And it stays terminal rather than flapping back to a value.
		assert!(consumer.changed().await.is_err());
	}
}
