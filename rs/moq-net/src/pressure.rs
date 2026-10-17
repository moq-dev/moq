//! Receive-side recovery for when held connection credit fills the allowance.
//!
//! A [`Reader`](crate::coding::Reader) tracked here keeps its buffered bytes and their
//! connection credit in a [`Hold`], and lends the credit for a partial unit's bytes to
//! whatever owns them as a [`Lent`], so incomplete messages are bounded by the
//! transport's receive allowance. When the transport reports
//! that the allowance is spent ([`poll_credit_exhausted`](crate::transport::poll::Session::poll_credit_exhausted)),
//! nothing can arrive to complete any unit, so the [`Monitor`] evicts the least useful
//! holder: the eviction itself drops the buffer and the credit and stops the stream with
//! STOP_SENDING, whatever the stream's handler is waiting on, and the handler's next read
//! fails with [`Error::Evicted`](crate::Error::Evicted).
//!
//! Victims, first to last:
//! - Media, from the broadcast holding the most credit, lowest priority first. Within a
//!   broadcast the subscription's priority and group order rank the streams, as on the
//!   send side; raw priorities of unrelated broadcasts are never compared.
//! - Unidentified streams, whose header has yet to name a subscription, so a header
//!   gets the chance to identify itself before it is cancelled.
//! - Control streams. Cancelling one may end the session under its protocol, which the
//!   stream's handler reports like any other error.
//!
//! Ties go to the larger holder, then to the newest stream. There is no protected class:
//! the allowance fits the largest supported unit, so cancelling every other holder
//! always leaves room for one to complete.

use std::{
	collections::HashMap,
	sync::{
		Arc, Mutex, MutexGuard, PoisonError, Weak,
		atomic::{AtomicBool, AtomicU64, Ordering},
	},
	task::{Context, Waker},
};

use bytes::{Buf, Bytes, BytesMut};

use crate::{
	PathOwned,
	priority::Priority,
	transport::{Credit, MaybeSend, MaybeSync, Stopper, poll::RecvStream},
};

/// How useful a tracked stream is, which orders the victims.
#[derive(Clone, Debug)]
pub(crate) enum Rank {
	/// A stream whose header has not identified it yet.
	Unidentified,
	/// A media stream of one broadcast, ranked within it.
	Media {
		/// The broadcast, the domain inside which priorities compare. Shared, since every
		/// group stream of a subscription names the same one.
		broadcast: Arc<PathOwned>,
		/// The stream's rank within its broadcast.
		priority: Priority,
	},
	/// A control stream.
	Control,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
	mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A tracked stream's buffer and credit, and what evicting it takes, shared by its reader
/// and the registry.
pub(crate) struct Hold<R: RecvStream> {
	/// Bytes of credit the slot covers, read by the registry without the lock.
	held: AtomicU64,
	/// Bytes of credit lent out with partial units, until each [`Lent`] drops.
	lent: AtomicU64,
	/// The monitor chose this stream: its buffer and credit are gone and the stream is
	/// stopped.
	evicted: AtomicBool,
	/// The reader's task, woken on eviction.
	waker: Mutex<Option<Waker>>,
	rank: Mutex<Rank>,
	/// Registration order, the last tiebreak: the newest stream goes first.
	seq: u64,
	slot: Mutex<Slot<R::Credit>>,
	stopper: R::Stopper,
	/// The STOP_SENDING code for an eviction, in the stream's negotiated registry.
	code: u32,
}

/// A tracked reader's bytes and their credit, under one lock. Credit travels with the
/// bytes it covers and returns only once they are freed: credit returned for bytes still
/// allocated would let a peer refill the window while readers keep the memory.
struct Slot<C> {
	/// Declared before `credit`: fields drop in order, so a dropped slot frees the bytes
	/// before their credit returns.
	buffer: BytesMut,
	/// Leading bytes of `buffer` read before tracking began, which hold no credit.
	free: u64,
	/// The credit for `buffer` past `free`, which an eviction frees with it.
	credit: Option<C>,
}

impl<C: Credit> Slot<C> {
	fn held(&self) -> u64 {
		self.credit.as_ref().map_or(0, C::len)
	}

	/// Split off the credit for the first `bytes` of the buffer, which are leaving it.
	fn split(&mut self, bytes: u64) -> Option<C> {
		let free = self.free.min(bytes);
		self.free -= free;
		split(&mut self.credit, bytes - free)
	}

	/// Empty the buffer, handing back its bytes and their credit to drop in that order.
	fn empty(&mut self) -> (BytesMut, Option<C>) {
		self.free = 0;
		(std::mem::take(&mut self.buffer), self.credit.take())
	}
}

/// Add `credit` to `into`.
fn merge<C: Credit>(into: &mut Option<C>, credit: C) {
	match into {
		Some(held) => held.merge(credit),
		None => *into = Some(credit),
	}
}

/// Split the credit for `bytes` off `from`, if it holds any.
fn split<C: Credit>(from: &mut Option<C>, bytes: u64) -> Option<C> {
	from.as_mut().map(|credit| credit.split(bytes))
}

/// A [`Hold`]'s slot, locked by its reader.
pub(crate) struct Locked<'a, R: RecvStream> {
	hold: &'a Arc<Hold<R>>,
	slot: MutexGuard<'a, Slot<R::Credit>>,
}

impl<R: RecvStream> Locked<'_, R> {
	/// The reader's buffered bytes.
	pub fn buffer(&self) -> &BytesMut {
		&self.slot.buffer
	}

	/// The reader's buffered bytes, to read into or consume.
	pub fn buffer_mut(&mut self) -> &mut BytesMut {
		&mut self.slot.buffer
	}

	/// Whether the monitor chose this stream. An eviction sets it under the lock, so it
	/// holds for as long as this does: a `false` means bytes read now keep their credit
	/// until an eviction frees both.
	pub fn is_evicted(&self) -> bool {
		self.hold.is_evicted()
	}

	/// Take over bytes buffered before tracking began, whose credit already returned.
	pub fn adopt(&mut self, buffer: BytesMut) {
		self.slot.free = buffer.len() as u64;
		self.slot.buffer = buffer;
	}

	/// Keep the credit for bytes just read into the buffer. An eviction sets its flag under
	/// this lock, so a reader that checked it first cannot be reading into an evicted slot.
	pub fn take(&mut self, credit: R::Credit) {
		debug_assert!(!self.hold.is_evicted(), "read into an evicted buffer");
		merge(&mut self.slot.credit, credit);
		self.store();
	}

	/// Discard the first `bytes` of the buffer, a unit consumed whole, then return their
	/// credit.
	pub fn consume(&mut self, bytes: usize) {
		self.slot.buffer.advance(bytes);
		let credit = self.slot.split(bytes as u64);
		self.store();
		drop(credit);
	}

	/// Hand off the first `bytes` of the buffer as a complete unit, whose credit returns as
	/// it leaves.
	pub fn hand_off(&mut self, bytes: usize) -> Bytes {
		let unit = self.slot.buffer.split_to(bytes).freeze();
		let credit = self.slot.split(bytes as u64);
		self.store();
		drop(credit);
		unit
	}

	/// Move the first `bytes` of the buffer into a partial unit, lending their credit to
	/// whatever owns them next.
	pub fn lend_buffered(&mut self, bytes: usize) -> (Bytes, Option<Lent<R>>) {
		let unit = self.slot.buffer.split_to(bytes).freeze();
		let lent = self.slot.split(bytes as u64).map(|credit| self.hold.lend(credit));
		self.store();
		(unit, lent)
	}

	/// Lend the credit for bytes read straight into a partial unit to whatever owns them.
	pub fn lend(&self, credit: R::Credit) -> Lent<R> {
		self.hold.lend(credit)
	}

	fn store(&self) {
		self.hold.held.store(self.slot.held(), Ordering::Relaxed);
	}
}

/// Credit lent out of a [`Hold`] with the bytes of a partial unit: still counted toward
/// the hold, and returned when dropped. Whatever owns those bytes owns this beside them, so
/// the credit returns only once they are given up.
pub(crate) struct Lent<R: RecvStream> {
	credit: Option<R::Credit>,
	hold: Arc<Hold<R>>,
}

impl<R: RecvStream> Lent<R> {
	/// Take over `other`'s credit, lent by the same hold for the same unit.
	pub fn merge(&mut self, mut other: Self) {
		if let Some(credit) = other.credit.take() {
			merge(&mut self.credit, credit);
		}
	}
}

impl<R: RecvStream> Drop for Lent<R> {
	fn drop(&mut self) {
		let Some(credit) = self.credit.take() else {
			return;
		};
		let len = credit.len();
		// Returned before it stops counting, so a fill reported in between still sees an
		// eviction's release in flight rather than cancelling another stream.
		drop(credit);
		self.hold.lent.fetch_sub(len, Ordering::Relaxed);
	}
}

impl<R: RecvStream> Hold<R> {
	/// Lock the buffer and credit, which an eviction drops together.
	pub fn lock(self: &Arc<Self>) -> Locked<'_, R> {
		Locked {
			hold: self,
			slot: lock(&self.slot),
		}
	}

	fn lend(self: &Arc<Self>, credit: R::Credit) -> Lent<R> {
		self.lent.fetch_add(credit.len(), Ordering::Relaxed);
		Lent {
			credit: Some(credit),
			hold: self.clone(),
		}
	}

	/// Drop the buffer and return its credit: the reader gave up on the stream. Credit lent
	/// to a partial unit returns when its owner gives that up.
	pub fn clear(&self) {
		let mut slot = lock(&self.slot);
		let (buffer, credit) = slot.empty();
		self.held.store(0, Ordering::Relaxed);
		drop(slot);
		// Freed before the credit returns, as on eviction.
		drop(buffer);
		drop(credit);
	}

	/// Re-rank the stream, once its header identifies it.
	pub fn rank(&self, rank: Rank) {
		*lock(&self.rank) = rank;
	}

	/// Whether the monitor chose this stream, without registering to hear when it does.
	pub fn is_evicted(&self) -> bool {
		self.evicted.load(Ordering::Acquire)
	}

	/// Whether the monitor chose this stream, registering `cx` to hear when it does.
	pub fn poll_evicted(&self, cx: &Context<'_>) -> bool {
		if self.is_evicted() {
			return true;
		}
		let mut waker = lock(&self.waker);
		match waker.as_mut() {
			Some(waker) => waker.clone_from(cx.waker()),
			None => *waker = Some(cx.waker().clone()),
		}
		drop(waker);
		// Checked again: an eviction between the load and the store took no waker.
		self.is_evicted()
	}
}

/// What the registry needs from a [`Hold`], whatever its stream type.
pub(crate) trait Holder {
	/// Bytes of credit held.
	fn held(&self) -> u64;
	/// Whether it was evicted already.
	fn is_evicted(&self) -> bool;
	/// How useful the stream is.
	fn rank(&self) -> Rank;
	/// Registration order.
	fn seq(&self) -> u64;
	/// Drop the buffer and credit, stop the stream, and wake its reader to fail.
	fn evict(&self);
}

impl<R: RecvStream> Holder for Hold<R> {
	fn held(&self) -> u64 {
		self.held.load(Ordering::Relaxed) + self.lent.load(Ordering::Relaxed)
	}

	fn is_evicted(&self) -> bool {
		Hold::is_evicted(self)
	}

	fn rank(&self) -> Rank {
		lock(&self.rank).clone()
	}

	fn seq(&self) -> u64 {
		self.seq
	}

	fn evict(&self) {
		let mut slot = lock(&self.slot);
		// Set under the lock, so a reader holding it reads and keeps that credit as one
		// step, and one locking after sees the flag and reads nothing more in.
		self.evicted.store(true, Ordering::Release);
		let (buffer, credit) = slot.empty();
		// Credit lent to a partial unit stays counted until the owner of its bytes, woken
		// below through the reader, gives the unit up.
		self.held.store(0, Ordering::Relaxed);
		drop(slot);
		let freed = credit.as_ref().map_or(0, Credit::len);
		tracing::debug!(rank = ?Holder::rank(self), freed, "receive credit exhausted, cancelling a stream");
		// Stopped first, so the window the credit frees does not invite more of this stream.
		self.stopper.stop(self.code);
		// The bytes go before their credit: returned credit lets the peer send again at
		// once, and the memory it bounds must already be free.
		drop(buffer);
		drop(credit);
		let waker = lock(&self.waker).take();
		if let Some(waker) = waker {
			waker.wake();
		}
	}
}

/// The registry entry of a session whose tracked readers differ in stream type: the
/// moq-transport path, which reads both raw streams and its control adapter's.
#[cfg(not(target_family = "wasm"))]
pub(crate) type Erased = dyn Holder + Send + Sync;
#[cfg(target_family = "wasm")]
pub(crate) type Erased = dyn Holder;

/// The registry of a session whose tracked readers all read its own streams: moq-lite's.
///
/// An alias rather than the spelled-out type, so a `derive` on a struct holding it bounds
/// only `S`, not its stream type.
pub(crate) type Of<S> = Pressure<Hold<<S as crate::transport::poll::Session>::RecvStream>>;

/// Turns a reader's [`Hold`] into the registry's entry type.
pub(crate) trait Erase<H: ?Sized> {
	fn erase(self: Arc<Self>) -> Arc<H>;
}

impl<R: RecvStream> Erase<Hold<R>> for Hold<R> {
	fn erase(self: Arc<Self>) -> Arc<Self> {
		self
	}
}

impl<R: RecvStream> Erase<Erased> for Hold<R>
where
	Self: MaybeSend + MaybeSync,
{
	fn erase(self: Arc<Self>) -> Arc<Erased> {
		self
	}
}

/// One session's tracked receivers, each a [`Hold`] seen as `H`.
pub(crate) struct Pressure<H: ?Sized> {
	state: kio::Lock<State<H>>,
}

impl<H: ?Sized> Clone for Pressure<H> {
	fn clone(&self) -> Self {
		Self {
			state: self.state.clone(),
		}
	}
}

impl<H: ?Sized> Default for Pressure<H> {
	fn default() -> Self {
		Self {
			state: kio::Lock::new(State {
				entries: Vec::new(),
				next: 0,
				prune: PRUNE,
			}),
		}
	}
}

/// Entries kept before the first sweep of dropped readers.
const PRUNE: usize = 64;

struct State<H: ?Sized> {
	/// Weak, so a reader unregisters by being dropped, and swept on growth.
	entries: Vec<Weak<H>>,
	next: u64,
	/// Sweep dropped readers once `entries` reaches this.
	prune: usize,
}

/// A live holder, as of the choice.
struct Candidate<H: ?Sized> {
	hold: Arc<H>,
	rank: Rank,
	held: u64,
	seq: u64,
}

impl<H: ?Sized + Holder> Pressure<H> {
	/// Track a new receiver at `rank`, which `stopper` stops with `code` on eviction.
	pub fn track<R: RecvStream>(&self, rank: Rank, stopper: R::Stopper, code: u32) -> Arc<Hold<R>>
	where
		Hold<R>: Erase<H>,
	{
		let mut state = self.state.lock();
		let hold = Arc::new(Hold {
			held: AtomicU64::new(0),
			lent: AtomicU64::new(0),
			evicted: AtomicBool::new(false),
			waker: Mutex::new(None),
			rank: Mutex::new(rank),
			seq: state.next,
			slot: Mutex::new(Slot {
				buffer: BytesMut::new(),
				free: 0,
				credit: None,
			}),
			stopper,
			code,
		});
		state.next += 1;
		if state.entries.len() >= state.prune {
			state.entries.retain(|entry| entry.strong_count() > 0);
			state.prune = PRUNE.max(2 * state.entries.len());
		}
		state.entries.push(Arc::downgrade(&hold.clone().erase()));
		hold
	}

	/// Evict the least useful holder of credit, returning whether there was one.
	///
	/// An evicted stream still holding a partial unit is one: its reader, woken by the
	/// eviction, gives the unit up and frees what this fill needs, so cancelling another
	/// stream meanwhile would cost it for nothing.
	pub fn evict(&self) -> bool {
		let victim = {
			let state = self.state.lock();
			let releasing = state
				.entries
				.iter()
				.filter_map(Weak::upgrade)
				.any(|hold| hold.is_evicted() && hold.held() > 0);
			if releasing {
				return true;
			}
			victim(&state.entries)
		};
		// Evicted without the registry lock: dropping credit and stopping call into the
		// transport, and readers track and drop meanwhile.
		match victim {
			Some(hold) => {
				hold.evict();
				true
			}
			None => false,
		}
	}

	/// The bytes every tracked receiver holds, for tests.
	#[cfg(test)]
	pub fn held(&self) -> u64 {
		let state = self.state.lock();
		state
			.entries
			.iter()
			.filter_map(Weak::upgrade)
			.map(|hold| hold.held())
			.sum()
	}
}

/// Pick the victim: see the module docs for the order.
fn victim<H: ?Sized + Holder>(entries: &[Weak<H>]) -> Option<Arc<H>> {
	let holding: Vec<_> = entries
		.iter()
		.filter_map(Weak::upgrade)
		.filter_map(|hold| {
			let held = hold.held();
			(held > 0 && !hold.is_evicted()).then(|| Candidate {
				rank: hold.rank(),
				seq: hold.seq(),
				held,
				hold,
			})
		})
		.collect();

	// Media: the broadcast holding the most, then its least useful stream. Keyed by path
	// rather than by `Arc`, since separate subscriptions to one broadcast share no `Arc`.
	// A run of streams sharing one `Arc` sums before it is hashed.
	let mut domains: HashMap<&PathOwned, u64> = HashMap::new();
	let mut run: Option<(&Arc<PathOwned>, u64)> = None;
	for entry in &holding {
		if let Rank::Media { broadcast, .. } = &entry.rank {
			match &mut run {
				Some((path, total)) if Arc::ptr_eq(path, broadcast) => *total += entry.held,
				_ => {
					if let Some((path, total)) = run.replace((broadcast, entry.held)) {
						*domains.entry(path.as_ref()).or_default() += total;
					}
				}
			}
		}
	}
	if let Some((path, total)) = run {
		*domains.entry(path.as_ref()).or_default() += total;
	}
	// The larger holder, then the lower path, so the choice never depends on order.
	let domain = domains
		.into_iter()
		.max_by(|(a, a_held), (b, b_held)| a_held.cmp(b_held).then_with(|| b.as_str().cmp(a.as_str())));
	let chosen = match domain {
		Some((domain, _)) => holding
			.iter()
			.filter_map(|entry| match &entry.rank {
				Rank::Media { broadcast, priority } if broadcast.as_ref() == domain => Some((priority, entry)),
				_ => None,
			})
			// `Priority` sorts the most urgent first, so the greatest is the least useful.
			.max_by(|(a, a_entry), (b, b_entry)| {
				a.cmp(b)
					.then_with(|| a_entry.held.cmp(&b_entry.held))
					.then_with(|| a_entry.seq.cmp(&b_entry.seq))
			})
			.map(|(_, entry)| entry),
		None => {
			let largest = |control: bool| {
				holding
					.iter()
					.filter(|entry| matches!(entry.rank, Rank::Control) == control)
					.max_by(|a, b| a.held.cmp(&b.held).then_with(|| a.seq.cmp(&b.seq)))
			};
			largest(false).or_else(|| largest(true))
		}
	};
	chosen.map(|entry| entry.hold.clone())
}

/// Cancels a stream each time the session's receive allowance fills.
pub(crate) struct Monitor<S, H: ?Sized> {
	session: S,
	pressure: Pressure<H>,
}

impl<S: crate::transport::poll::Session, H: ?Sized + Holder> Monitor<S, H> {
	/// Watch `session`, cancelling streams tracked by `pressure`.
	pub fn new(session: S, pressure: Pressure<H>) -> Self {
		Self { session, pressure }
	}

	/// Run until dropped. Each fill cancels one stream: the next fill can only come after
	/// it let go of its credit and the peer spent what that freed.
	pub fn poll(&mut self, cx: &mut Context<'_>) -> std::task::Poll<()> {
		while self.session.poll_credit_exhausted(cx).is_ready() {
			if !self.pressure.evict() {
				// Untracked readers or transport read-ahead hold it all; whoever holds it
				// lets go on their own, and a later fill is reported again.
				tracing::warn!("receive credit exhausted with no tracked holder to cancel");
			}
		}
		std::task::Poll::Pending
	}
}

#[cfg(test)]
mod tests {
	use std::task::{Poll, Waker};

	use super::*;
	use crate::{Path, track::Order, transport::Unheld};

	/// Logs the codes its streams were stopped with.
	#[derive(Clone, Default)]
	struct Stops(Arc<Mutex<Vec<u32>>>);

	impl Stopper for Stops {
		fn stop(&self, code: u32) {
			lock(&self.0).push(code);
		}
	}

	/// A stream that only exists to name its credit and stopper.
	struct Probe;

	impl RecvStream for Probe {
		type Error = crate::lite::test_transport::SinkError;
		type Credit = Unheld;
		type Stopper = Stops;

		fn poll_read(&mut self, _: &mut Context<'_>, _: &mut [u8]) -> Poll<Result<Option<Unheld>, Self::Error>> {
			Poll::Pending
		}

		fn stop(&mut self, _: u32) {}

		fn stopper(&self) -> Stops {
			Stops::default()
		}

		fn poll_closed(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
			Poll::Pending
		}
	}

	type Probes = Pressure<Hold<Probe>>;

	/// Credit that records, as it returns, whether the bytes it covers were freed first:
	/// it shares their allocation, so it is the last owner once the buffer is gone.
	struct Watched {
		len: u64,
		bytes: Option<bytes::Bytes>,
		freed_first: Arc<AtomicBool>,
	}

	impl Credit for Watched {
		fn len(&self) -> u64 {
			self.len
		}

		fn split(&mut self, bytes: u64) -> Self {
			self.len -= bytes;
			Self {
				len: bytes,
				bytes: None,
				freed_first: self.freed_first.clone(),
			}
		}

		fn merge(&mut self, other: Self) {
			self.len += other.len;
		}
	}

	impl Drop for Watched {
		fn drop(&mut self) {
			if let Some(bytes) = &self.bytes {
				self.freed_first.store(bytes.is_unique(), Ordering::Relaxed);
			}
		}
	}

	/// A stream whose credit is [`Watched`].
	struct Watch;

	impl RecvStream for Watch {
		type Error = crate::lite::test_transport::SinkError;
		type Credit = Watched;
		type Stopper = Stops;

		fn poll_read(&mut self, _: &mut Context<'_>, _: &mut [u8]) -> Poll<Result<Option<Watched>, Self::Error>> {
			Poll::Pending
		}

		fn stop(&mut self, _: u32) {}

		fn stopper(&self) -> Stops {
			Stops::default()
		}

		fn poll_closed(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
			Poll::Pending
		}
	}

	/// An evicted stream still holding a partial unit answers the next fill: its reader
	/// frees that credit once it wakes, and another stream goes only if the window is still
	/// full after.
	#[test]
	fn a_release_in_flight_spares_the_next_stream() {
		let pressure = Probes::default();
		let partial = pressure.track::<Probe>(Rank::Unidentified, Stops::default(), 0x35);
		let lent = partial.lock().lend(Unheld(10));
		let other = hold(&pressure, Rank::Unidentified, 5);

		assert!(pressure.evict());
		assert!(Holder::is_evicted(&*partial));
		assert_eq!(pressure.held(), 15, "the partial unit stays counted");

		assert!(pressure.evict());
		assert!(
			!Holder::is_evicted(&*other),
			"cancelled while the partial unit was letting go"
		);

		drop(lent);
		assert!(pressure.evict());
		assert!(Holder::is_evicted(&*other));
	}

	/// Eviction and abort free the buffered bytes before their credit returns, so the
	/// window the peer regains never covers memory still allocated.
	#[test]
	fn buffered_bytes_are_freed_before_their_credit_returns() {
		let pressure = Pressure::<Hold<Watch>>::default();
		for evict in [true, false] {
			let hold = pressure.track::<Watch>(Rank::Unidentified, Stops::default(), 0x35);
			let freed_first = Arc::new(AtomicBool::new(false));
			{
				let mut locked = hold.lock();
				locked.buffer_mut().extend_from_slice(&[7; 64]);
				let bytes = locked.buffer_mut().split_off(32).freeze();
				locked.take(Watched {
					len: 32,
					bytes: Some(bytes),
					freed_first: freed_first.clone(),
				});
			}
			match evict {
				true => assert!(pressure.evict()),
				false => hold.clear(),
			}
			assert!(
				freed_first.load(Ordering::Relaxed),
				"evict={evict}: the credit returned while its bytes were still allocated"
			);
			assert!(hold.lock().buffer().is_empty());
		}
	}

	fn media(broadcast: &str, track: u8, group: u64) -> Rank {
		Rank::Media {
			broadcast: Arc::new(Path::new(broadcast).to_owned()),
			priority: Priority::new(track, 0, group),
		}
	}

	/// Track a receiver holding `held` bytes.
	fn hold(pressure: &Probes, rank: Rank, held: u64) -> Arc<Hold<Probe>> {
		let hold = pressure.track::<Probe>(rank, Stops::default(), 0x35);
		hold.lock().take(Unheld(held));
		hold
	}

	/// Evict once, returning every hold chosen so far.
	fn evict(pressure: &Probes, holds: &[&Arc<Hold<Probe>>]) -> Vec<usize> {
		assert!(pressure.evict());
		holds
			.iter()
			.enumerate()
			.filter(|(_, hold)| hold.is_evicted())
			.map(|(i, _)| i)
			.collect()
	}

	#[test]
	fn media_goes_before_unidentified_before_control() {
		let pressure = Probes::default();
		let control = hold(&pressure, Rank::Control, 100);
		let unidentified = hold(&pressure, Rank::Unidentified, 50);
		let media = hold(&pressure, media("room", 255, 0), 10);
		let all = [&control, &unidentified, &media];

		assert_eq!(evict(&pressure, &all), [2]);
		assert_eq!(evict(&pressure, &all), [1, 2]);
		assert_eq!(evict(&pressure, &all), [0, 1, 2]);
		// Everything holding was chosen already.
		assert!(!pressure.evict());
	}

	#[test]
	fn the_least_urgent_stream_of_the_fullest_broadcast_goes() {
		let pressure = Probes::default();
		// `small` holds less in total, so its less urgent track is not compared.
		let small = hold(&pressure, media("small", 0, 0), 30);
		let urgent = hold(&pressure, media("big", 200, 0), 20);
		let lax = hold(&pressure, media("big", 100, 0), 20);
		let all = [&small, &urgent, &lax];

		assert_eq!(evict(&pressure, &all), [2]);
		// `big` is left with 20, now less than `small`.
		assert_eq!(evict(&pressure, &all), [0, 2]);
	}

	#[test]
	fn group_order_ranks_within_a_subscription() {
		let pressure = Probes::default();
		let old = hold(&pressure, media("room", 1, 1), 10);
		let new = hold(&pressure, media("room", 1, 2), 10);
		// Newest first by default, so the older group is the less useful one.
		assert_eq!(evict(&pressure, &[&old, &new]), [0]);

		let ascending = |group| Rank::Media {
			broadcast: Arc::new(Path::new("room").to_owned()),
			priority: Priority::new(1, 0, group).with_order(Order::Ascending),
		};
		let pressure = Probes::default();
		let old = hold(&pressure, ascending(1), 10);
		let new = hold(&pressure, ascending(2), 10);
		assert_eq!(evict(&pressure, &[&old, &new]), [1]);
	}

	#[test]
	fn ties_go_to_the_larger_then_the_newer() {
		let pressure = Probes::default();
		let first = hold(&pressure, Rank::Unidentified, 10);
		let larger = hold(&pressure, Rank::Unidentified, 20);
		let newest = hold(&pressure, Rank::Unidentified, 10);
		let all = [&first, &larger, &newest];
		assert_eq!(evict(&pressure, &all), [1]);
		assert_eq!(evict(&pressure, &all), [1, 2]);
	}

	#[test]
	fn empty_and_dropped_receivers_are_never_chosen() {
		let pressure = Probes::default();
		let idle = hold(&pressure, media("room", 0, 0), 0);
		drop(hold(&pressure, media("room", 0, 0), 10));
		assert!(!pressure.evict());
		assert!(!idle.is_evicted());
	}

	/// Dropped readers are swept as the registry grows, so it stays the size of what is live.
	#[test]
	fn dropped_receivers_are_swept() {
		let pressure = Probes::default();
		let live = hold(&pressure, Rank::Control, 1);
		for _ in 0..10 * PRUNE {
			drop(hold(&pressure, Rank::Control, 1));
		}
		assert!(pressure.state.lock().entries.len() <= PRUNE + 1);
		assert_eq!(pressure.held(), 1);
		drop(live);
	}

	#[test]
	fn rerank_once_identified() {
		let pressure = Probes::default();
		let header = hold(&pressure, Rank::Unidentified, 10);
		let control = hold(&pressure, Rank::Control, 10);
		header.rank(media("room", 0, 0));
		assert_eq!(evict(&pressure, &[&header, &control]), [0]);
	}

	/// The eviction itself returns the credit and stops the stream, so a reader parked
	/// on something else holds nothing; it hears of it on its next poll.
	#[test]
	fn eviction_lets_go_without_the_reader() {
		#[derive(Default)]
		struct Count(std::sync::atomic::AtomicUsize);
		impl std::task::Wake for Count {
			fn wake(self: Arc<Self>) {
				self.0.fetch_add(1, Ordering::Relaxed);
			}
		}

		let pressure = Probes::default();
		let stops = Stops::default();
		let hold = pressure.track::<Probe>(Rank::Unidentified, stops.clone(), 0x35);
		hold.lock().take(Unheld(10));
		let count = Arc::new(Count::default());
		let waker = Waker::from(count.clone());
		assert!(!hold.poll_evicted(&Context::from_waker(&waker)));

		assert!(pressure.evict());
		assert_eq!(pressure.held(), 0);
		assert_eq!(*lock(&stops.0), [0x35]);
		assert_eq!(count.0.load(Ordering::Relaxed), 1);
		assert!(hold.poll_evicted(&Context::from_waker(&waker)));
	}

	/// Choose a victim among streams spread across broadcasts. The cost per stream must stay
	/// flat along both axes. Run by nightly.
	#[test]
	#[ignore = "release benchmark, run by nightly"]
	fn benchmark_victim() {
		println!("streams,broadcasts,ns_per_choice,ns_per_stream");
		for streams in [64usize, 1024, 16384] {
			for broadcasts in [1usize, 64, 1024, 16384] {
				if broadcasts > streams {
					continue;
				}
				let paths: Vec<_> = (0..broadcasts)
					.map(|b| Arc::new(Path::new(&format!("broadcast/{b}")).to_owned()))
					.collect();
				let pressure = Probes::default();
				let _holds: Vec<_> = (0..streams)
					.map(|i| {
						let rank = Rank::Media {
							broadcast: paths[i % broadcasts].clone(),
							priority: Priority::new((i % 256) as u8, 0, i as u64),
						};
						hold(&pressure, rank, 1000 + i as u64)
					})
					.collect();
				let rounds = (1 << 22) / streams;
				let state = pressure.state.lock();
				let start = std::time::Instant::now();
				for _ in 0..rounds {
					std::hint::black_box(victim(std::hint::black_box(&state.entries)));
				}
				let ns = start.elapsed().as_nanos() as f64 / rounds as f64;
				println!("{streams},{broadcasts},{ns:.0},{:.1}", ns / streams as f64);
			}
		}
	}
}
