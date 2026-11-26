//! The one transport type the session code is compiled for.
//!
//! [`Client`](crate::Client) and [`Server`](crate::Server) box the caller's transport
//! here before running any protocol code, so the lite and ietf drivers are
//! monomorphized once per binary instead of once per transport and per crate that
//! awaits them. Each transport only instantiates the thin forwarding impls below.

use std::any::Any;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::task::{Poll, ready};

use bytes::{Buf, Bytes};

use super::open::{self, Dir, Open};
use super::poll::{self, SendOptions};
use super::{Address, CloseKind, Limits, LimitsError, Probe, ReceiveWindow, Wire};

/// A boxed [`poll::Boxable`] session, shared by every task that uses it.
#[derive(Clone)]
pub(crate) struct Session {
	inner: Arc<Shared<dyn ErasedSession>>,
	/// Shared by every clone, so whichever task closes the transport first says why.
	closing: std::sync::Arc<crate::session::Closing>,
	/// Shared by every clone, so each task counts the subscriptions and fetches it carries.
	work: crate::session::Work,
	/// Shared by every clone, so every open waits for the peer's stream credit in one line.
	pub(super) opens: Arc<open::Lines>,
}

impl Session {
	pub(crate) fn new<S: poll::Boxable>(session: S) -> Self {
		let inner: Arc<Shared<dyn ErasedSession>> = Arc::new(Shared {
			paused: AtomicU64::new(0),
			slots: Mutex::default(),
			session,
		});
		Self {
			opens: Default::default(),
			inner,
			closing: Default::default(),
			work: Default::default(),
		}
	}

	/// Poll opening a unidirectional stream scheduled by `options`, once `open` is the most
	/// important open waiting for the peer's stream credit.
	pub(crate) fn poll_open_uni(
		&self,
		open: &mut Open,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
	) -> Poll<Result<SendStream, Error>> {
		open.poll(&self.opens, Dir::Uni, waiter, &options, |waiter| {
			self.inner.session.poll_open_uni(waiter, options)
		})
	}

	/// Poll opening a unidirectional stream for a group, like [`Self::poll_open_uni`], which the
	/// session lists among its open group streams until it is finished, reset, or dropped.
	///
	/// A higher-ranked open waiting for credit may reset it with `preempt`, the stream code
	/// the negotiated version gives the reason; its writer then reads that code back.
	pub(crate) fn poll_open_group(
		&self,
		open: &mut Open,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
		preempt: u32,
	) -> Poll<Result<SendStream, Error>> {
		let SendStream(Sender::Own(stream)) = ready!(self.poll_open_uni(open, waiter, options))? else {
			unreachable!("a stream is listed only here");
		};
		let listed = Arc::new(Listed {
			state: Mutex::new(ListedState {
				stream,
				group: options.group.map(|group| group.rank.clone()),
				priority: options.priority,
				preempt,
				preempted: false,
				waiters: Default::default(),
			}),
		});
		let key = self.opens.list(&listed);
		Poll::Ready(Ok(SendStream(Sender::Listed(Listing {
			listed,
			lines: self.opens.clone(),
			key: Some(key),
		}))))
	}

	/// The session's open group streams, the lowest ranked first.
	#[cfg(test)]
	pub(super) fn open_streams(&self) -> Vec<(open::Rank, Arc<Listed>)> {
		self.opens.open_streams()
	}

	/// Poll opening a bidirectional stream, its sending half scheduled by `options`, once
	/// `open` is the most important open waiting for the peer's stream credit.
	pub(crate) fn poll_open_bi(
		&self,
		open: &mut Open,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
	) -> Poll<Result<(SendStream, RecvStream), Error>> {
		open.poll(&self.opens, Dir::Bi, waiter, &options, |waiter| {
			self.inner.session.poll_open_bi(waiter, options, self.budget())
		})
	}

	/// The handle each stream this session yields reserves its paused credit through.
	fn budget(&self) -> Budget {
		Arc::downgrade(&self.inner)
	}

	/// How this side closed the transport.
	pub(crate) fn closing(&self) -> &std::sync::Arc<crate::session::Closing> {
		&self.closing
	}

	/// Resolve once the session is ending: this side closed the transport, or it closed.
	///
	/// Unlike `poll_closed`, which a backend holds until its close is on the wire, this sees a
	/// local close the moment it is sent.
	pub(crate) fn poll_ending(&self, waiter: &kio::Waiter) -> Poll<()> {
		if self.closing.is_sent() {
			return Poll::Ready(());
		}
		poll::Session::poll_closed(self, waiter).map(|_| ())
	}

	/// The subscriptions and fetches the session's tasks carry over this transport.
	pub(crate) fn work(&self) -> &crate::session::Work {
		&self.work
	}

	/// Close with `code` over `cause`, this side's reason for ending the session.
	pub(crate) fn abort(&self, cause: &crate::Error, code: u32, reason: &str) {
		self.closing.sent(code, Some(cause));
		self.inner.session.close(code, reason);
	}
}

/// A boxed transport and the receive credit its paused streams hold.
pub(super) struct Shared<S: ?Sized> {
	/// The connection credit held by streams this session stopped reading to pace a group's
	/// writer, at most half the connection window; see [`ReceiveWindow`].
	paused: AtomicU64,
	/// The slots of the streams this session shapes the credit of; see [`Pacing::allowance`].
	slots: Mutex<Slots>,
	session: S,
}

impl Shared<dyn ErasedSession> {
	/// Reserve `charge` for one more paused stream, unless it would push the paused total over
	/// half the `connection` window.
	fn reserve(&self, charge: u64, connection: u64) -> bool {
		let cap = connection / 2;
		self.paused
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |paused| {
				paused.checked_add(charge).filter(|total| *total <= cap)
			})
			.is_ok()
	}
}

/// The slots of a session's shaped streams, the lowest free one first, so a session with few
/// streams gives each a large allowance.
#[derive(Default)]
struct Slots {
	next: u64,
	free: BTreeSet<u64>,
}

impl Slots {
	fn take(&mut self) -> u64 {
		self.free.pop_first().unwrap_or_else(|| {
			self.next += 1;
			self.next - 1
		})
	}

	fn give(&mut self, slot: u64) {
		self.free.insert(slot);
	}
}

/// How ingest paces a stream under its session's receive windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pacing {
	/// How far the group's writer reads ahead of its fastest reader before pausing, on a
	/// transport that cannot shape credit, whose paused streams hold their stream credit.
	window: u64,
	/// The stream's full receive credit.
	stream: u64,
	/// What the session's paused streams may hold together, half its connection window; zero
	/// without one to share.
	cap: u64,
}

impl Pacing {
	fn new(receive: Option<ReceiveWindow>) -> Self {
		let window = crate::model::pace::window(receive);
		let Some(receive) = receive else {
			return Self {
				window,
				stream: 0,
				cap: 0,
			};
		};
		Self {
			window,
			stream: receive.stream,
			cap: receive.connection.map_or(0, |connection| connection.get() / 2),
		}
	}

	/// The credit a paced stream in `slot` lets the peer send past the room its group has
	/// left, so a paused one holds about this much.
	///
	/// Credit once granted cannot be taken back, so a paused stream holds its allowance until
	/// it resumes, however many streams pause after it. The cap is therefore split by slot,
	/// not by how many are paused: slot `i` gets `cap / ((i + 1) * (i + 2))`, which sums to
	/// under the cap over any number of slots, so every stream fits at its own allowance.
	fn allowance(&self, slot: u64) -> u64 {
		let share = slot.saturating_add(1).saturating_mul(slot.saturating_add(2));
		(self.cap / share).min(self.stream)
	}
}

/// The window a stream gets for the room its group has left, as [`RecvStream::shape`] sets it,
/// copied out of the stream so a lease, which borrows it, can set the window as it is handed
/// back. The default shapes nothing.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Shaper {
	/// Whether the transport shapes credit, and a connection window is shared.
	pub(crate) active: bool,
	/// The stream's full receive credit.
	pub(crate) stream: u64,
	/// The credit the stream lets the peer send past the room, from its slot.
	pub(crate) allowance: u64,
	/// The window set last.
	pub(crate) current: u64,
}

impl Shaper {
	/// The window to set for the `room` the group has left, at least the `unsealed` rest of
	/// the segment being written, or `None` while the current one stands; `None` for the room
	/// while no reader holds the writer back.
	///
	/// Credit granted stays granted, so a window left above its target is credit the stream
	/// holds once paused, and a smaller one is always set. A larger one only is once it moves
	/// half an allowance, as it pays for the transport's lock.
	pub(crate) fn window(&self, room: Option<u64>, unsealed: u64) -> Option<u64> {
		if !self.active {
			return None;
		}
		let target = match room {
			Some(room) => room.max(unsealed).saturating_add(self.allowance).min(self.stream),
			None => self.stream,
		};
		(target < self.current || target - self.current >= self.allowance / 2).then_some(target)
	}
}

/// The session a stream reserves paused credit from, which a stream outliving it never keeps
/// open: once it is gone, nothing is paused.
type Budget = Weak<Shared<dyn ErasedSession>>;

/// A boxed transport error, keeping both code registries and the message.
#[derive(Debug)]
pub(crate) struct Error(Box<dyn super::Error>);

impl Error {
	fn new(err: impl super::Error) -> Self {
		Self(Box::new(err))
	}
}

impl std::fmt::Display for Error {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		self.0.fmt(f)
	}
}

impl std::error::Error for Error {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		self.0.source()
	}
}

impl super::Error for Error {
	fn close_kind(&self) -> CloseKind {
		self.0.close_kind()
	}

	fn session_error(&self) -> Option<(u32, String)> {
		self.0.session_error()
	}

	fn stream_error(&self) -> Option<u32> {
		self.0.stream_error()
	}
}

/// A boxed send group. It only ever goes back to the session that opened it.
pub(crate) struct Group {
	inner: Box<dyn ErasedGroup>,
	/// Shared by every clone, like the group itself, so an open ranks by the group's current
	/// priority and turn.
	rank: Arc<GroupRank>,
}

/// Where a send group ranks against the session's others: its priority, then its turn.
pub(super) struct GroupRank {
	priority: std::sync::atomic::AtomicI64,
	/// When the group last took stream credit, counted by its session's [`open::Lines`].
	turn: AtomicU64,
}

impl GroupRank {
	/// The group's priority and turn.
	pub(super) fn read(&self) -> (i64, u64) {
		(self.priority.load(Ordering::Relaxed), self.turn.load(Ordering::Relaxed))
	}
}

impl Group {
	/// The group's priority and turn, as its opens rank.
	pub(super) fn rank(&self) -> (i64, u64) {
		self.rank.read()
	}

	/// Mark the group as having just taken stream credit, at `turn`.
	pub(super) fn take_turn(&self, turn: u64) {
		self.rank.turn.store(turn, Ordering::Relaxed);
	}
}

impl Clone for Group {
	fn clone(&self) -> Self {
		Self {
			inner: self.inner.clone_box(),
			rank: self.rank.clone(),
		}
	}
}

impl poll::SendGroup for Group {
	fn set_priority(&mut self, priority: i64) {
		self.rank.priority.store(priority, Ordering::Relaxed);
		self.inner.set_priority(priority)
	}
}

/// A boxed stream's offset, holding the transport's own behind the boundary.
pub(crate) struct Offset(Box<dyn Any + Send + Sync>);

/// A boxed outgoing stream.
pub(crate) struct SendStream(Sender);

enum Sender {
	Own(Held),
	/// A group stream its session lists, so the list can read it too.
	Listed(Listing),
}

/// A transport's stream, and the offset its resets still deliver every byte before.
struct Held {
	stream: Box<dyn ErasedSend>,
	floor: Option<Offset>,
}

impl Held {
	fn new(stream: Box<dyn ErasedSend>) -> Self {
		Self { stream, floor: None }
	}

	/// Reset with `code`, still delivering what precedes the floor.
	fn reset(&mut self, code: u32) {
		match &self.floor {
			Some(floor) => self.stream.reset_at(floor, code),
			None => self.stream.reset(code),
		}
	}
}

impl SendStream {
	/// Box a stream, keeping its own send-group type behind the boundary.
	#[cfg(test)]
	pub(crate) fn new(stream: impl poll::SendStream<Group: Send + Sync, Offset: Send + Sync> + Send + Sync) -> Self {
		Self(Sender::Own(Held::new(Box::new(stream))))
	}

	fn own(stream: impl poll::SendStream<Group: Send + Sync, Offset: Send + Sync> + Send + Sync + 'static) -> Self {
		Self(Sender::Own(Held::new(Box::new(stream))))
	}

	/// Run `f` on the transport's stream, then leave the session's list if `unlist`.
	fn with<T>(&mut self, unlist: bool, f: impl FnOnce(&mut Held) -> T) -> T {
		match &mut self.0 {
			Sender::Own(held) => f(held),
			Sender::Listed(listing) => {
				let res = f(&mut listing.listed.lock().stream);
				if unlist {
					listing.unlist();
				}
				res
			}
		}
	}

	/// From now on, every reset of the stream, a preemption included, still delivers the bytes
	/// written so far. Dropping it unreset leaves that to the transport's own drop.
	pub(crate) fn hold(&mut self) {
		self.with(false, |held| held.floor = Some(held.stream.offset()));
	}

	/// Whether [`Self::hold`] marked the bytes every later reset still delivers.
	pub(crate) fn held(&self) -> bool {
		self.get().held().floor.is_some()
	}

	/// Whether the peer acknowledged every byte [held](Self::hold), false when nothing is.
	pub(crate) fn held_acked(&self) -> bool {
		let read = self.get();
		let held = read.held();
		held.floor
			.as_ref()
			.is_some_and(|floor| matches!(held.stream.poll_acked(&kio::Waiter::noop(), floor), Poll::Ready(Ok(()))))
	}

	/// Whether the session reset the stream to make room for a higher-ranked one.
	pub(crate) fn preempted(&self) -> bool {
		match &self.0 {
			Sender::Own(_) => false,
			Sender::Listed(listing) => listing.listed.lock().preempted,
		}
	}

	/// Poll a write, failing once the session preempted the stream, and waking a parked
	/// write when it does.
	fn poll_writing(
		&mut self,
		waiter: &kio::Waiter,
		f: impl FnOnce(&mut dyn ErasedSend) -> Poll<Result<usize, Error>>,
	) -> Poll<Result<usize, Error>> {
		match &mut self.0 {
			Sender::Own(held) => f(held.stream.as_mut()),
			Sender::Listed(listing) => {
				let mut state = listing.listed.lock();
				if let Some(err) = state.preempted() {
					return Poll::Ready(Err(err));
				}
				let res = f(state.stream.stream.as_mut());
				if res.is_pending() {
					state.waiters.register(waiter);
				}
				res
			}
		}
	}

	fn get(&self) -> Read<'_> {
		match &self.0 {
			Sender::Own(held) => Read::Own(held),
			Sender::Listed(listing) => Read::Listed(listing.listed.lock()),
		}
	}
}

/// A boxed stream borrowed for a read through `&self`.
enum Read<'a> {
	Own(&'a Held),
	Listed(std::sync::MutexGuard<'a, ListedState>),
}

impl Read<'_> {
	fn held(&self) -> &Held {
		match self {
			Read::Own(held) => held,
			Read::Listed(state) => &state.stream,
		}
	}
}

impl std::ops::Deref for Read<'_> {
	type Target = dyn ErasedSend;

	fn deref(&self) -> &Self::Target {
		self.held().stream.as_ref()
	}
}

/// An open group stream, shared by the task writing it and its session's list.
pub(super) struct Listed {
	state: Mutex<ListedState>,
}

struct ListedState {
	stream: Held,
	/// The rank of the send group the stream is in, shared with the group's handles.
	group: Option<Arc<GroupRank>>,
	priority: i64,
	/// The stream code a reset to make room uses.
	preempt: u32,
	/// Whether the session reset the stream to make room, so its writer fails.
	preempted: bool,
	/// The writer's waits, woken once the stream is preempted.
	waiters: kio::WaiterList,
}

impl ListedState {
	/// The writer's error once the session reset the stream to make room.
	fn preempted(&self) -> Option<Error> {
		self.preempted.then(|| Error::new(Preempted(self.preempt)))
	}
}

/// A group stream the session reset to make room for a higher-ranked open.
#[derive(Debug, thiserror::Error)]
#[error("reset to make room for a higher-priority stream")]
struct Preempted(u32);

impl super::Error for Preempted {
	fn session_error(&self) -> Option<(u32, String)> {
		None
	}

	fn stream_error(&self) -> Option<u32> {
		Some(self.0)
	}
}

impl Listed {
	fn lock(&self) -> std::sync::MutexGuard<'_, ListedState> {
		self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
	}

	/// Where the stream ranks now.
	pub(super) fn rank(&self) -> open::Rank {
		let state = self.lock();
		open::Rank::new(state.group.as_ref().map(|group| group.read()), state.priority)
	}

	/// Whether the peer acknowledged everything written to it, always so for a transport that
	/// cannot see acknowledgments.
	pub(super) fn acknowledged(&self) -> bool {
		let state = self.lock();
		let offset = state.stream.stream.offset();
		matches!(
			state.stream.stream.poll_acked(&kio::Waiter::noop(), &offset),
			Poll::Ready(Ok(()))
		)
	}

	/// Reset the stream to make room, failing its writer with the reset's code, still delivering
	/// what it [held](SendStream::hold).
	pub(super) fn preempt(&self) {
		let mut state = self.lock();
		if state.preempted {
			return;
		}
		state.preempted = true;
		let code = state.preempt;
		state.stream.reset(code);
		state.waiters.wake();
	}
}

/// The writing task's hold on a listed stream, which leaves the list once the stream is
/// finished, reset, or dropped.
struct Listing {
	listed: Arc<Listed>,
	lines: Arc<open::Lines>,
	/// The key the stream is listed under, until it leaves.
	key: Option<u64>,
}

impl Listing {
	fn unlist(&mut self) {
		if let Some(key) = self.key.take() {
			self.lines.unlist(key);
		}
	}
}

impl Drop for Listing {
	fn drop(&mut self) {
		self.unlist();
	}
}

/// A boxed incoming stream.
pub(crate) struct RecvStream(Box<dyn ErasedRecv>);

impl RecvStream {
	/// Box a stream outside any session, which therefore never pauses.
	#[cfg(test)]
	pub(crate) fn new(stream: impl poll::RecvStream + Send + Sync) -> Self {
		Self::budgeted(stream, Weak::<Shared<super::memory::Session>>::new())
	}

	/// Box a stream of `session`, whose receive windows size its pacing [`window`](Self::window).
	fn budgeted(stream: impl poll::RecvStream + Send + Sync, session: Budget) -> Self {
		Self(Box::new(Budgeted {
			stream,
			session,
			held: None,
			pacing: OnceLock::new(),
			shaped: None,
			unshaped: false,
			probed: false,
			slot: None,
		}))
	}

	/// How far ingest reads this stream ahead of its group's fastest reader before pausing:
	/// the credit the stream may hold, so a paced hop buffers about what its peer can have
	/// in flight on it, and on a transport that shapes credit a stream window more, which a
	/// paused stream no longer holds in the transport.
	pub(crate) fn window(&mut self) -> u64 {
		self.0.window()
	}

	/// Let the peer send only what the group's writer has `room` for, at least the `unsealed`
	/// rest of the segment being written, plus an allowance, or the full stream credit while
	/// no reader holds the writer back (`None`), so the stream holds about the allowance once
	/// it pauses. See [`poll::RecvStream::set_window`].
	pub(crate) fn shape(&mut self, room: Option<u64>, unsealed: u64) {
		self.0.shape(room, unsealed)
	}

	/// What [`Self::shape`] would set for a room, readable while a lease borrows the stream, so
	/// the window can be set as the lease is handed back. Read after [`Self::window`], which
	/// learns whether the transport shapes credit.
	pub(crate) fn shaper(&mut self) -> Shaper {
		self.0.shaper()
	}

	/// Note the window a lease set from [`Self::shaper`], as [`Self::shape`] would have.
	pub(crate) fn shaped(&mut self, window: u64) {
		self.0.shaped(window)
	}

	/// Reserve the connection credit this stream holds while ingest stops reading it, returning
	/// whether it may stop: not once the session's paused streams hold half its connection
	/// window, nor on a transport that reports no window.
	pub(crate) fn pause(&mut self) -> bool {
		self.0.pause()
	}

	/// Return the credit [`Self::pause`] reserved, as ingest reads the stream again.
	pub(crate) fn resume(&mut self) {
		self.0.resume()
	}
}

/// A transport's receive stream and the paused credit it reserved from its session.
struct Budgeted<T> {
	stream: T,
	session: Budget,
	/// The credit reserved while paused, `None` while read.
	held: Option<u64>,
	/// Read from the session's receive windows when ingest first paces the stream, so one
	/// that arrived before admission widened them paces at the wider ones.
	pacing: OnceLock<Pacing>,
	/// The window last set with [`poll::RecvStream::set_window`], `None` for the full credit.
	shaped: Option<u64>,
	/// The transport cannot set a stream's window, so a paused stream holds its full credit.
	unshaped: bool,
	/// Whether the transport has been asked yet to set the stream's window.
	probed: bool,
	/// The stream's slot among the session's shaped streams, until it drops.
	slot: Option<u64>,
}

impl<T> Budgeted<T> {
	/// Return the credit reserved while paused, if any.
	fn release(&mut self) {
		if let Some(held) = self.held.take()
			&& held > 0
			&& let Some(shared) = self.session.upgrade()
		{
			shared.paused.fetch_sub(held, Ordering::AcqRel);
		}
	}

	/// The credit this stream lets the peer send past the room its group has left, from its
	/// slot, taking one on first use; see [`Pacing::allowance`].
	fn allowance(&mut self, pacing: &Pacing) -> u64 {
		let slot = match self.slot {
			Some(slot) => slot,
			None => {
				let Some(shared) = self.session.upgrade() else {
					return 0;
				};
				let slot = shared.slots.lock().expect("slots poisoned").take();
				*self.slot.insert(slot)
			}
		};
		pacing.allowance(slot)
	}

	/// Give the slot back, once the stream drops or its transport cannot shape credit.
	fn free(&mut self) {
		if let Some(slot) = self.slot.take()
			&& let Some(shared) = self.session.upgrade()
		{
			shared.slots.lock().expect("slots poisoned").give(slot);
		}
	}

	fn pacing(&self) -> Pacing {
		*self.pacing.get_or_init(|| {
			let receive = self
				.session
				.upgrade()
				.and_then(|shared| shared.session.receive_window());
			Pacing::new(receive)
		})
	}
}

impl<T: poll::RecvStream> Budgeted<T> {
	/// Set the stream's window to `window`, returning the connection credit it may hold, or
	/// `None` once the transport turned out not to support it.
	fn set_window(&mut self, window: u64) -> Option<u64> {
		if self.unshaped {
			return None;
		}
		let credit = self.stream.set_window(window);
		self.unshaped = credit.is_none();
		if self.unshaped {
			self.free();
		}
		self.shaped = credit.map(|_| window);
		credit
	}
}

impl<T> Drop for Budgeted<T> {
	fn drop(&mut self) {
		self.release();
		self.free();
	}
}

/// A boxed [`poll::Lease`], borrowed from a [`RecvStream`].
pub(crate) struct Lease<'a>(Box<dyn ErasedLease + 'a>);

/// The transport's connection metrics, read once.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Metrics {
	bytes_lost: Option<u64>,
	packets_sent: Option<u64>,
	packets_received: Option<u64>,
	packets_lost: Option<u64>,
	rtt: Option<std::time::Duration>,
	estimated_send_rate: Option<u64>,
	streams_evicted: Option<u64>,
}

impl Metrics {
	fn read(metrics: impl super::Metrics) -> Self {
		Self {
			bytes_lost: metrics.bytes_lost(),
			packets_sent: metrics.packets_sent(),
			packets_received: metrics.packets_received(),
			packets_lost: metrics.packets_lost(),
			rtt: metrics.rtt(),
			estimated_send_rate: metrics.estimated_send_rate(),
			streams_evicted: metrics.streams_evicted(),
		}
	}
}

impl super::Metrics for Metrics {
	fn bytes_lost(&self) -> Option<u64> {
		self.bytes_lost
	}
	fn packets_sent(&self) -> Option<u64> {
		self.packets_sent
	}
	fn packets_received(&self) -> Option<u64> {
		self.packets_received
	}
	fn packets_lost(&self) -> Option<u64> {
		self.packets_lost
	}
	fn rtt(&self) -> Option<std::time::Duration> {
		self.rtt
	}
	fn estimated_send_rate(&self) -> Option<u64> {
		self.estimated_send_rate
	}
	fn streams_evicted(&self) -> Option<u64> {
		self.streams_evicted
	}
}

pub(super) trait ErasedSession: Send + Sync {
	fn poll_accept_uni(&self, waiter: &kio::Waiter, budget: Budget) -> Poll<Result<RecvStream, Error>>;
	fn poll_accept_bi(&self, waiter: &kio::Waiter, budget: Budget) -> Poll<Result<(SendStream, RecvStream), Error>>;
	fn poll_open_uni(&self, waiter: &kio::Waiter, options: SendOptions<'_, Group>) -> Poll<Result<SendStream, Error>>;
	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
		budget: Budget,
	) -> Poll<Result<(SendStream, RecvStream), Error>>;
	fn send_group(&self, priority: i64) -> Option<Group>;
	fn send_datagram(&self, payload: Bytes, options: SendOptions<'_, Group>) -> Result<(), Error>;
	fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>>;
	fn max_datagram_size(&self) -> usize;
	fn protocol(&self) -> Option<&str>;
	fn address(&self) -> Address;
	fn close(&self, code: u32, reason: &str);
	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error>;
	fn metrics(&self) -> Metrics;
	fn wire(&self) -> Option<Wire>;
	fn probe(&self) -> Option<Probe>;
	fn receive_window(&self) -> Option<ReceiveWindow>;
	fn reliable_reset(&self) -> bool;
	fn set_limits(&self, limits: Limits) -> Result<(), LimitsError>;
}

trait ErasedGroup: Send + Sync {
	fn clone_box(&self) -> Box<dyn ErasedGroup>;
	fn set_priority(&mut self, priority: i64);
	fn as_any(&self) -> &dyn Any;
}

trait ErasedSend: Send + Sync {
	fn poll_write(&mut self, waiter: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Error>>;
	fn poll_write_buf(&mut self, waiter: &kio::Waiter, buf: &mut dyn Buf) -> Poll<Result<usize, Error>>;
	fn set_priority(&mut self, priority: i64);
	fn set_group(&mut self, group: &Group);
	fn finish(&mut self) -> Result<(), Error>;
	fn reset(&mut self, code: u32);
	fn reset_at(&mut self, offset: &Offset, code: u32);
	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>>;
	fn offset(&self) -> Offset;
	fn poll_acked(&self, waiter: &kio::Waiter, offset: &Offset) -> Poll<Result<(), Error>>;
}

trait ErasedRecv: Send + Sync {
	fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<Lease<'_>, Error>>;
	fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Error>>;
	fn set_priority(&mut self, priority: i64);
	fn stop(&mut self, code: u32);
	fn poll_reset(&mut self, waiter: &kio::Waiter) -> Poll<Error>;
	fn set_window(&mut self, window: u64) -> Option<u64>;
	fn window(&mut self) -> u64;
	fn shape(&mut self, room: Option<u64>, unsealed: u64);
	fn shaper(&mut self) -> Shaper;
	fn shaped(&mut self, window: u64);
	fn pause(&mut self) -> bool;
	fn resume(&mut self);
}

// Not `Send`: a lease may hold its transport's lock, and never outlives a poll.
trait ErasedLease: Buf {
	fn chunks(&self) -> Box<dyn Iterator<Item = &[u8]> + '_>;
	fn is_fin(&self) -> bool;
	fn release(self: Box<Self>);
	fn set_window(&mut self, window: u64);
}

type GroupOf<S> = <<S as poll::Session>::SendStream as poll::SendStream>::Group;

/// Recover the transport's own group from a boxed one.
fn unbox<'a, G: 'static>(options: SendOptions<'a, Group>) -> SendOptions<'a, G> {
	SendOptions {
		group: options.group.map(|group| {
			group
				.inner
				.as_any()
				.downcast_ref::<G>()
				.expect("send group belongs to another transport")
		}),
		priority: options.priority,
	}
}

fn bi<S: poll::Boxable>((send, recv): poll::BiStreams<S>, budget: Budget) -> (SendStream, RecvStream) {
	(SendStream::own(send), RecvStream::budgeted(recv, budget))
}

impl<S: poll::Boxable> ErasedSession for S {
	fn poll_accept_uni(&self, waiter: &kio::Waiter, budget: Budget) -> Poll<Result<RecvStream, Error>> {
		poll::Session::poll_accept_uni(self, waiter)
			.map(|res| res.map(|recv| RecvStream::budgeted(recv, budget)).map_err(Error::new))
	}

	fn poll_accept_bi(&self, waiter: &kio::Waiter, budget: Budget) -> Poll<Result<(SendStream, RecvStream), Error>> {
		poll::Session::poll_accept_bi(self, waiter).map(|res| res.map(|bis| bi::<S>(bis, budget)).map_err(Error::new))
	}

	fn poll_open_uni(&self, waiter: &kio::Waiter, options: SendOptions<'_, Group>) -> Poll<Result<SendStream, Error>> {
		poll::Session::poll_open_uni(self, waiter, unbox(options))
			.map(|res| res.map(SendStream::own).map_err(Error::new))
	}

	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
		budget: Budget,
	) -> Poll<Result<(SendStream, RecvStream), Error>> {
		poll::Session::poll_open_bi(self, waiter, unbox(options))
			.map(|res| res.map(|bis| bi::<S>(bis, budget)).map_err(Error::new))
	}

	fn send_group(&self, priority: i64) -> Option<Group> {
		poll::Session::send_group(self, priority).map(|group| Group {
			inner: Box::new(group),
			rank: Arc::new(GroupRank {
				priority: priority.into(),
				turn: AtomicU64::new(0),
			}),
		})
	}

	fn send_datagram(&self, payload: Bytes, options: SendOptions<'_, Group>) -> Result<(), Error> {
		poll::Session::send_datagram(self, payload, unbox::<GroupOf<S>>(options)).map_err(Error::new)
	}

	fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>> {
		poll::Session::poll_recv_datagram(self, waiter).map_err(Error::new)
	}

	fn max_datagram_size(&self) -> usize {
		poll::Session::max_datagram_size(self)
	}

	fn protocol(&self) -> Option<&str> {
		poll::Session::protocol(self)
	}

	fn address(&self) -> Address {
		poll::Session::address(self)
	}

	fn close(&self, code: u32, reason: &str) {
		poll::Session::close(self, code, reason)
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		poll::Session::poll_closed(self, waiter).map(Error::new)
	}

	fn metrics(&self) -> Metrics {
		Metrics::read(poll::Session::metrics(self))
	}

	fn wire(&self) -> Option<Wire> {
		poll::Session::wire(self)
	}

	fn probe(&self) -> Option<Probe> {
		poll::Session::probe(self)
	}

	fn receive_window(&self) -> Option<ReceiveWindow> {
		poll::Session::receive_window(self)
	}

	fn reliable_reset(&self) -> bool {
		poll::Session::reliable_reset(self)
	}

	fn set_limits(&self, limits: Limits) -> Result<(), LimitsError> {
		poll::Session::set_limits(self, limits)
	}
}

impl<G: poll::SendGroup + Send + Sync> ErasedGroup for G {
	fn clone_box(&self) -> Box<dyn ErasedGroup> {
		Box::new(self.clone())
	}

	fn set_priority(&mut self, priority: i64) {
		poll::SendGroup::set_priority(self, priority)
	}

	fn as_any(&self) -> &dyn Any {
		self
	}
}

impl<T: poll::SendStream<Group: Send + Sync, Offset: Send + Sync> + Send + Sync> ErasedSend for T {
	fn poll_write(&mut self, waiter: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Error>> {
		poll::SendStream::poll_write(self, waiter, buf).map_err(Error::new)
	}

	fn poll_write_buf(&mut self, waiter: &kio::Waiter, mut buf: &mut dyn Buf) -> Poll<Result<usize, Error>> {
		// `&mut dyn Buf` forwards `copy_to_bytes`, so a `Bytes` source stays zero-copy.
		poll::SendStream::poll_write_buf(self, waiter, &mut buf).map_err(Error::new)
	}

	fn set_priority(&mut self, priority: i64) {
		poll::SendStream::set_priority(self, priority)
	}

	fn set_group(&mut self, group: &Group) {
		let group = group
			.inner
			.as_any()
			.downcast_ref::<T::Group>()
			.expect("send group belongs to another transport");
		poll::SendStream::set_group(self, group)
	}

	fn finish(&mut self) -> Result<(), Error> {
		poll::SendStream::finish(self).map_err(Error::new)
	}

	fn reset(&mut self, code: u32) {
		poll::SendStream::reset(self, code)
	}

	fn reset_at(&mut self, offset: &Offset, code: u32) {
		poll::SendStream::reset_at(self, own_offset::<T>(offset), code)
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		poll::SendStream::poll_closed(self, waiter).map_err(Error::new)
	}

	fn offset(&self) -> Offset {
		Offset(Box::new(poll::SendStream::offset(self)))
	}

	fn poll_acked(&self, waiter: &kio::Waiter, offset: &Offset) -> Poll<Result<(), Error>> {
		poll::SendStream::poll_acked(self, waiter, own_offset::<T>(offset)).map_err(Error::new)
	}
}

/// The transport's own offset behind a boxed one.
fn own_offset<T: poll::SendStream<Offset: Send + Sync>>(offset: &Offset) -> &T::Offset {
	offset
		.0
		.downcast_ref::<T::Offset>()
		.expect("offset belongs to another transport")
}

impl<T: poll::RecvStream + Send + Sync> ErasedRecv for Budgeted<T> {
	fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<Lease<'_>, Error>> {
		poll::RecvStream::poll_lease(&mut self.stream, waiter, want)
			.map(|res| res.map(|lease| Lease(Box::new(lease))).map_err(Error::new))
	}

	// Forwarded rather than left to the provided method, so reading a chunk never boxes a lease.
	fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		poll::RecvStream::poll_read_chunk(&mut self.stream, waiter, max).map_err(Error::new)
	}

	fn set_priority(&mut self, priority: i64) {
		poll::RecvStream::set_priority(&mut self.stream, priority)
	}

	fn stop(&mut self, code: u32) {
		poll::RecvStream::stop(&mut self.stream, code)
	}

	fn poll_reset(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		poll::RecvStream::poll_reset(&mut self.stream, waiter).map(Error::new)
	}

	fn set_window(&mut self, window: u64) -> Option<u64> {
		poll::RecvStream::set_window(&mut self.stream, window)
	}

	fn window(&mut self) -> u64 {
		let pacing = self.pacing();
		// Whether the transport shapes credit decides the window, so ask before the first one
		// rather than learn it from the first shaping that moves.
		if pacing.cap > 0 && !self.probed {
			self.probed = true;
			Budgeted::set_window(self, pacing.stream);
		}
		match self.unshaped || pacing.cap == 0 {
			true => pacing.window,
			// A shaped stream holds a stream window less once paused, so its group holds it
			// instead, and a paced reader has as much buffered ahead as before.
			false => pacing
				.window
				.saturating_add(pacing.stream)
				.min(crate::model::pace::AHEAD),
		}
	}

	fn shape(&mut self, room: Option<u64>, unsealed: u64) {
		if let Some(window) = self.shaper().window(room, unsealed) {
			Budgeted::set_window(self, window);
		}
	}

	fn shaper(&mut self) -> Shaper {
		let pacing = self.pacing();
		// Nothing is shared without a connection window, so nothing needs to stay small.
		if pacing.cap == 0 || self.unshaped {
			return Shaper::default();
		}
		Shaper {
			active: true,
			stream: pacing.stream,
			allowance: self.allowance(&pacing),
			current: self.shaped.unwrap_or(pacing.stream),
		}
	}

	fn shaped(&mut self, window: u64) {
		self.shaped = Some(window);
	}

	fn pause(&mut self) -> bool {
		if self.held.is_some() {
			return true;
		}
		let Some(shared) = self.session.upgrade() else {
			return false;
		};
		let Some(receive) = shared.session.receive_window() else {
			return false;
		};
		// Without a connection window a paused stream holds only its own credit.
		let Some(connection) = receive.connection.map(std::num::NonZeroU64::get) else {
			self.held = Some(0);
			return true;
		};
		let pacing = self.pacing();
		// The least it could hold, checked first, so a stream past the cap reads on without
		// taking the transport's lock.
		let least = match self.unshaped {
			true => receive.stream,
			false => 0,
		};
		if shared.paused.load(Ordering::Acquire).saturating_add(least) > pacing.cap {
			return false;
		}
		// A transport that shapes says what the stream holds; any other holds its full credit.
		let credit = match self.unshaped {
			true => None,
			false => {
				let allowance = self.allowance(&pacing);
				Budgeted::set_window(self, allowance)
			}
		};
		let charge = credit.unwrap_or(receive.stream).min(connection);
		// Refused, it reads on and sheds, still holding only its shaped credit, so it may pause
		// once others resume.
		if !shared.reserve(charge, connection) {
			return false;
		}
		self.held = Some(charge);
		true
	}

	fn resume(&mut self) {
		self.release();
	}
}

impl<L: poll::Lease> ErasedLease for L {
	fn chunks(&self) -> Box<dyn Iterator<Item = &[u8]> + '_> {
		Box::new(poll::Lease::chunks(self))
	}

	fn is_fin(&self) -> bool {
		poll::Lease::is_fin(self)
	}

	fn release(self: Box<Self>) {
		poll::Lease::release(*self)
	}

	fn set_window(&mut self, window: u64) {
		poll::Lease::set_window(self, window)
	}
}

impl poll::Session for Session {
	type SendStream = SendStream;
	type RecvStream = RecvStream;
	type Error = Error;

	fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<RecvStream, Error>> {
		self.inner.session.poll_accept_uni(waiter, self.budget())
	}

	fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<(SendStream, RecvStream), Error>> {
		self.inner.session.poll_accept_bi(waiter, self.budget())
	}

	// These skip the line for stream credit. Session code opens through the inherent
	// `poll_open_uni` and `poll_open_bi`, which shadow them and take the open's place in line.
	fn poll_open_uni(&self, waiter: &kio::Waiter, options: SendOptions<'_, Group>) -> Poll<Result<SendStream, Error>> {
		self.inner.session.poll_open_uni(waiter, options)
	}

	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
	) -> Poll<Result<(SendStream, RecvStream), Error>> {
		self.inner.session.poll_open_bi(waiter, options, self.budget())
	}

	fn send_group(&self, priority: i64) -> Option<Group> {
		let group = self.inner.session.send_group(priority)?;
		// A new group waits its turn behind the groups already waiting, not ahead of them.
		group.take_turn(self.opens.next_turn());
		Some(group)
	}

	fn send_datagram(&self, payload: Bytes, options: SendOptions<'_, Group>) -> Result<(), Error> {
		self.inner.session.send_datagram(payload, options)
	}

	fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>> {
		self.inner.session.poll_recv_datagram(waiter)
	}

	fn max_datagram_size(&self) -> usize {
		self.inner.session.max_datagram_size()
	}

	fn protocol(&self) -> Option<&str> {
		self.inner.session.protocol()
	}

	fn address(&self) -> Address {
		self.inner.session.address()
	}

	fn close(&self, code: u32, reason: &str) {
		self.closing.sent(code, None);
		self.inner.session.close(code, reason)
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		self.inner.session.poll_closed(waiter)
	}

	fn metrics(&self) -> impl super::Metrics {
		self.inner.session.metrics()
	}

	fn wire(&self) -> Option<Wire> {
		self.inner.session.wire()
	}

	fn probe(&self) -> Option<Probe> {
		self.inner.session.probe()
	}

	fn receive_window(&self) -> Option<ReceiveWindow> {
		self.inner.session.receive_window()
	}

	fn reliable_reset(&self) -> bool {
		self.inner.session.reliable_reset()
	}

	fn set_limits(&self, limits: Limits) -> Result<(), LimitsError> {
		self.inner.session.set_limits(limits)
	}
}

impl poll::SendStream for SendStream {
	type Error = Error;
	type Group = Group;
	type Offset = Offset;

	fn offset(&self) -> Offset {
		self.get().offset()
	}

	fn poll_acked(&self, waiter: &kio::Waiter, offset: &Offset) -> Poll<Result<(), Error>> {
		self.get().poll_acked(waiter, offset)
	}

	fn poll_write(&mut self, waiter: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Error>> {
		self.poll_writing(waiter, |stream| stream.poll_write(waiter, buf))
	}

	fn poll_write_buf<B: Buf>(&mut self, waiter: &kio::Waiter, buf: &mut B) -> Poll<Result<usize, Error>> {
		self.poll_writing(waiter, |stream| stream.poll_write_buf(waiter, buf))
	}

	fn set_priority(&mut self, priority: i64) {
		if let Sender::Listed(listing) = &self.0 {
			listing.listed.lock().priority = priority;
		}
		self.with(false, |held| held.stream.set_priority(priority))
	}

	fn set_group(&mut self, group: &Group) {
		if let Sender::Listed(listing) = &self.0 {
			listing.listed.lock().group = Some(group.rank.clone());
		}
		self.with(false, |held| held.stream.set_group(group))
	}

	fn finish(&mut self) -> Result<(), Error> {
		if let Sender::Listed(listing) = &self.0
			&& let Some(err) = listing.listed.lock().preempted()
		{
			return Err(err);
		}
		self.with(true, |held| held.stream.finish())
	}

	/// Resets at the [held](Self::hold) offset once there is one.
	fn reset(&mut self, code: u32) {
		// A preempted stream keeps the code it was reset with.
		if let Sender::Listed(listing) = &self.0
			&& listing.listed.lock().preempted
		{
			return;
		}
		self.with(true, |held| held.reset(code))
	}

	fn reset_at(&mut self, offset: &Offset, code: u32) {
		if let Sender::Listed(listing) = &self.0
			&& listing.listed.lock().preempted
		{
			return;
		}
		self.with(true, |held| held.stream.reset_at(offset, code))
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		match &self.0 {
			Sender::Own(held) => held.stream.poll_closed(waiter),
			Sender::Listed(listing) => {
				let mut state = listing.listed.lock();
				if let Some(err) = state.preempted() {
					return Poll::Ready(Err(err));
				}
				state.waiters.register(waiter);
				state.stream.stream.poll_closed(waiter)
			}
		}
	}
}

impl poll::RecvStream for RecvStream {
	type Error = Error;
	type Lease<'a> = Lease<'a>;

	fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<Lease<'_>, Error>> {
		self.0.poll_lease(waiter, want)
	}

	fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		self.0.poll_read_chunk(waiter, max)
	}

	fn set_priority(&mut self, priority: i64) {
		self.0.set_priority(priority)
	}

	fn stop(&mut self, code: u32) {
		self.0.stop(code)
	}

	fn poll_reset(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		self.0.poll_reset(waiter)
	}

	fn set_window(&mut self, window: u64) -> Option<u64> {
		self.0.set_window(window)
	}
}

impl Buf for Lease<'_> {
	fn remaining(&self) -> usize {
		self.0.remaining()
	}

	fn chunk(&self) -> &[u8] {
		self.0.chunk()
	}

	fn advance(&mut self, n: usize) {
		self.0.advance(n)
	}

	// Forwarded so a transport's zero-copy override survives the box.
	fn copy_to_bytes(&mut self, n: usize) -> Bytes {
		self.0.copy_to_bytes(n)
	}
}

impl poll::Lease for Lease<'_> {
	fn chunks(&self) -> impl Iterator<Item = &[u8]> {
		self.0.chunks()
	}

	fn is_fin(&self) -> bool {
		self.0.is_fin()
	}

	fn release(self) {
		self.0.release()
	}

	fn set_window(&mut self, window: u64) {
		self.0.set_window(window)
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use crate::lite::test_transport::SinkSession;

	fn accept(session: &Session) -> RecvStream {
		let Poll::Ready(Ok((_, recv))) = poll::Session::poll_accept_bi(session, &kio::Waiter::noop()) else {
			panic!("the sink accepts at once");
		};
		recv
	}

	/// On a transport that cannot shape credit, each paused stream reserves its stream window,
	/// and the session pauses no more than fit in half the connection window; a stream
	/// resuming or dropped returns its share.
	#[test]
	fn paused_streams_hold_at_most_half_the_connection_window() {
		let gate = kio::Producer::new(true);
		let session = Session::new(
			SinkSession::accepted_bi(gate.consume())
				.with_receive_window(ReceiveWindow::new(std::num::NonZeroU64::new(100), 20)),
		);
		let mut streams: Vec<_> = (0..3).map(|_| accept(&session)).collect();
		assert!(streams[0].pause());
		assert!(streams[0].pause(), "a paused stream reserves once");
		assert!(streams[1].pause());
		assert!(!streams[2].pause(), "a third would hold 60 of 100");

		streams[0].resume();
		assert!(streams[2].pause(), "a resumed stream returned its share");
		drop(streams.remove(1));
		assert!(accept(&session).pause(), "so did a dropped one");

		// Without a connection window nothing is shared, so every stream may pause.
		let session = Session::new(
			SinkSession::accepted_bi(gate.consume()).with_receive_window(ReceiveWindow::new(None, u64::MAX)),
		);
		let mut streams: Vec<_> = (0..3).map(|_| accept(&session)).collect();
		assert!(streams.iter_mut().all(|stream| stream.pause()));
	}

	/// A stream that records each window it is set to, and holds that much credit once set.
	struct Shaping {
		windows: Arc<Mutex<Vec<u64>>>,
	}

	impl poll::RecvStream for Shaping {
		type Error = crate::lite::test_transport::SinkError;
		type Lease<'a> = poll::BufferedLease<'a>;

		fn poll_lease(&mut self, _: &kio::Waiter, _: usize) -> Poll<Result<Self::Lease<'_>, Self::Error>> {
			Poll::Pending
		}

		fn set_priority(&mut self, _: i64) {}

		fn stop(&mut self, _: u32) {}

		fn set_window(&mut self, window: u64) -> Option<u64> {
			self.windows.lock().unwrap().push(window);
			Some(window)
		}
	}

	const CONNECTION: u64 = 128 * 1024;
	const STREAM: u64 = 64 * 1024;
	/// What a session's paused streams may hold together in these tests.
	const CAP: u64 = CONNECTION / 2;

	fn shaping_session() -> Session {
		let gate = kio::Producer::new(true);
		Session::new(
			SinkSession::accepted_bi(gate.consume())
				.with_receive_window(ReceiveWindow::new(std::num::NonZeroU64::new(CONNECTION), STREAM)),
		)
	}

	fn shaping(session: &Session) -> (RecvStream, Arc<Mutex<Vec<u64>>>) {
		let windows = Arc::new(Mutex::new(Vec::new()));
		let stream = Shaping {
			windows: windows.clone(),
		};
		(RecvStream::budgeted(stream, session.budget()), windows)
	}

	/// A stream on a transport that shapes credit lets the peer send what its group has room
	/// for plus the allowance of its slot, and pauses holding what the transport says it holds.
	#[test]
	fn a_shaped_stream_pauses_at_the_credit_it_holds() {
		let session = shaping_session();
		let (mut first, first_windows) = shaping(&session);
		let (mut stream, windows) = shaping(&session);
		assert_eq!(
			stream.window(),
			2 * STREAM,
			"its group holds what the transport does not"
		);
		// The first to shape takes the first slot, the next the second: a sixth of the cap.
		first.shape(None, 0);
		assert!(first_windows.lock().unwrap().is_empty(), "already at its full credit");
		let allowance = CAP / 6;
		stream.shape(Some(10_000), 0);
		stream.shape(Some(9_800), 0);
		stream.shape(Some(100), 2_000);
		stream.shape(Some(100), 2_100);
		stream.shape(None, 0);
		assert_eq!(
			*windows.lock().unwrap(),
			[STREAM, 10_000 + allowance, 9_800 + allowance, 2_000 + allowance, STREAM,],
			"asked once, then a smaller window always applies, a larger only past half an allowance, and never short of the segment's rest"
		);

		// Each pauses at its allowance: the first a half of the cap, the second a sixth.
		assert!(first.pause());
		assert!(stream.pause());
		assert_eq!(*first_windows.lock().unwrap(), [CAP / 2]);
		assert_eq!(windows.lock().unwrap().last(), Some(&allowance));
		first.resume();
		stream.resume();
	}

	/// However many streams pause at once, and however they arrive, every one fits: the
	/// allowances of any number of slots add up to under the cap, and a stream that ends gives
	/// its slot, and so its allowance, to the next.
	#[test]
	fn any_number_of_shaped_streams_pause() {
		let session = shaping_session();
		let mut streams: Vec<_> = (0..1000).map(|_| shaping(&session)).collect();
		for (stream, _) in &mut streams {
			assert!(stream.pause(), "every stream fits");
		}
		let held = streams.iter().map(|(_, w)| *w.lock().unwrap().last().unwrap());
		assert!(held.sum::<u64>() <= CAP, "within the cap");

		// The lowest free slot goes first: a new stream takes the one just given back.
		let (_, windows) = streams.remove(2);
		let allowance = *windows.lock().unwrap().last().unwrap();
		let (mut stream, windows) = shaping(&session);
		assert!(stream.pause());
		assert_eq!(*windows.lock().unwrap(), [allowance]);
	}

	/// A stream paces at the credit its session grants it, so a hop holds about what its
	/// peer can have in flight; a transport without windows takes the cap, never pausing.
	#[test]
	fn a_stream_paces_at_its_credit() {
		use crate::model::pace::AHEAD;

		let gate = kio::Producer::new(true);
		let session = Session::new(
			SinkSession::accepted_bi(gate.consume())
				.with_receive_window(ReceiveWindow::new(std::num::NonZeroU64::new(100), 20)),
		);
		assert_eq!(accept(&session).window(), 20, "a transport that cannot shape credit");

		let session = Session::new(
			SinkSession::accepted_bi(gate.consume()).with_receive_window(ReceiveWindow::new(None, u64::MAX)),
		);
		assert_eq!(accept(&session).window(), AHEAD, "capped at half the live tail");

		let session = Session::new(SinkSession::accepted_bi(gate.consume()));
		assert_eq!(accept(&session).window(), AHEAD);
	}

	fn open_group(session: &Session, options: SendOptions<'_, Group>) -> SendStream {
		let Poll::Ready(Ok(stream)) = session.poll_open_group(&mut Open::default(), &kio::Waiter::noop(), options, 1)
		else {
			panic!("the transport opens at once");
		};
		stream
	}

	fn ranks(session: &Session) -> Vec<open::Rank> {
		session.open_streams().into_iter().map(|(rank, _)| rank).collect()
	}

	fn ungrouped(priority: i64) -> SendOptions<'static, Group> {
		SendOptions { group: None, priority }
	}

	/// Group streams are listed lowest ranked first, following a moved priority, until they
	/// finish, reset, or drop; other streams are never listed.
	#[test]
	fn group_streams_are_listed_until_they_end() {
		let (a, _b) = super::super::memory::pair(Default::default());
		let session = Session::new(a);
		let mut first = open_group(&session, ungrouped(1));
		let mut second = open_group(&session, ungrouped(3));
		let _control = session
			.poll_open_uni(&mut Open::default(), &kio::Waiter::noop(), ungrouped(0))
			.map(Result::unwrap);
		let third = open_group(&session, ungrouped(2));
		let rank = |stream| open::Rank::Ungrouped { stream };
		assert_eq!(ranks(&session), [rank(1), rank(2), rank(3)]);

		poll::SendStream::set_priority(&mut second, 0);
		assert_eq!(ranks(&session), [rank(0), rank(1), rank(2)]);

		poll::SendStream::finish(&mut first).unwrap();
		poll::SendStream::reset(&mut second, 1);
		assert_eq!(ranks(&session), [rank(2)]);
		drop(third);
		assert!(session.open_streams().is_empty());
	}

	/// A stream that counts written bytes as its offset and reports them acknowledged up to
	/// a mark, to check the boxed layer carries its offset through and back, and records the
	/// offset of its reset.
	struct Acking {
		written: u64,
		acked: u64,
		reset: std::sync::Arc<std::sync::Mutex<Option<Reset>>>,
	}

	/// The offset a reset kept bytes below, if any, and its code.
	type Reset = (Option<u64>, u32);

	impl poll::SendStream for Acking {
		type Error = super::super::memory::Error;
		type Group = poll::Ungrouped;
		type Offset = u64;

		fn offset(&self) -> u64 {
			self.written
		}

		fn poll_acked(&self, _: &kio::Waiter, offset: &u64) -> Poll<Result<(), Self::Error>> {
			match *offset <= self.acked {
				true => Poll::Ready(Ok(())),
				false => Poll::Pending,
			}
		}

		fn poll_write(&mut self, _: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
			self.written += buf.len() as u64;
			Poll::Ready(Ok(buf.len()))
		}

		fn set_priority(&mut self, _: i64) {}

		fn set_group(&mut self, group: &Self::Group) {
			match *group {}
		}

		fn finish(&mut self) -> Result<(), Self::Error> {
			Ok(())
		}

		fn reset(&mut self, code: u32) {
			*self.reset.lock().unwrap() = Some((None, code));
		}

		fn reset_at(&mut self, offset: &u64, code: u32) {
			*self.reset.lock().unwrap() = Some((Some(*offset), code));
		}

		fn poll_closed(&self, _: &kio::Waiter) -> Poll<Result<(), Self::Error>> {
			Poll::Pending
		}
	}

	/// A boxed stream hands back the transport's own offset, and a transport without
	/// acknowledgments answers at once, listed or not.
	#[test]
	fn a_boxed_stream_forwards_acknowledgment() {
		let waiter = kio::Waiter::noop();
		let mut stream = SendStream::new(Acking {
			written: 0,
			acked: 3,
			reset: Default::default(),
		});
		assert!(matches!(
			poll::SendStream::poll_write(&mut stream, &waiter, b"abc"),
			Poll::Ready(Ok(3))
		));
		let three = poll::SendStream::offset(&stream);
		assert!(matches!(
			poll::SendStream::poll_write(&mut stream, &waiter, b"de"),
			Poll::Ready(Ok(2))
		));
		let five = poll::SendStream::offset(&stream);
		assert!(matches!(
			poll::SendStream::poll_acked(&stream, &waiter, &three),
			Poll::Ready(Ok(()))
		));
		assert!(poll::SendStream::poll_acked(&stream, &waiter, &five).is_pending());

		let session = Session::new(SinkSession::new(Default::default()));
		let listed = open_group(&session, ungrouped(0));
		let offset = poll::SendStream::offset(&listed);
		assert!(matches!(
			poll::SendStream::poll_acked(&listed, &waiter, &offset),
			Poll::Ready(Ok(()))
		));
	}

	/// A held stream's reset still delivers what it held, whether its writer resets it or the
	/// session preempts it, and the stream reads as acknowledged once the peer took that much.
	#[test]
	fn a_held_stream_resets_after_what_it_held() {
		let waiter = kio::Waiter::noop();
		for preempt in [false, true] {
			let (a, b) = super::super::memory::pair(Default::default());
			let session = Session::new(a);
			let mut stream = open_group(&session, ungrouped(0));
			assert!(!stream.held_acked(), "nothing is held yet");
			assert!(matches!(
				poll::SendStream::poll_write(&mut stream, &waiter, b"group"),
				Poll::Ready(Ok(5))
			));
			stream.hold();
			assert!(matches!(
				poll::SendStream::poll_write(&mut stream, &waiter, b"frames"),
				Poll::Ready(Ok(6))
			));
			match preempt {
				true => session.open_streams()[0].1.preempt(),
				false => poll::SendStream::reset(&mut stream, 2),
			}
			assert!(!stream.held_acked());
			assert_eq!(stream.preempted(), preempt);

			let Poll::Ready(Ok(mut recv)) = poll::Session::poll_accept_uni(&b, &waiter) else {
				panic!("the peer accepts the stream");
			};
			let Poll::Ready(Ok(Some(held))) = poll::RecvStream::poll_read_chunk(&mut recv, &waiter, 16) else {
				panic!("the held bytes arrive");
			};
			assert_eq!(held, "group", "preempt: {preempt}");
			let Poll::Ready(Err(err)) = poll::RecvStream::poll_read_chunk(&mut recv, &waiter, 16) else {
				panic!("the reset follows them");
			};
			let code = match preempt {
				true => 1,
				false => 2,
			};
			assert_eq!(super::super::Error::stream_error(&err), Some(code));
			assert!(stream.held_acked(), "the peer took what was held");
		}
	}

	/// A listed stream reads as acknowledged once the peer has everything written to it.
	#[test]
	fn a_listed_stream_reports_its_acknowledgement() {
		let (a, b) = super::super::memory::pair(Default::default());
		let session = Session::new(a);
		let mut stream = open_group(&session, ungrouped(0));
		let acknowledged = || session.open_streams()[0].1.acknowledged();
		assert!(acknowledged(), "nothing written is all acknowledged");

		let waiter = kio::Waiter::noop();
		assert!(matches!(
			poll::SendStream::poll_write(&mut stream, &waiter, b"group"),
			Poll::Ready(Ok(5))
		));
		assert!(!acknowledged());

		let Poll::Ready(Ok(mut recv)) = poll::Session::poll_accept_uni(&b, &waiter) else {
			panic!("the peer accepts the stream");
		};
		assert!(matches!(
			poll::RecvStream::poll_read_chunk(&mut recv, &waiter, 16),
			Poll::Ready(Ok(Some(_)))
		));
		assert!(acknowledged());
	}

	/// A grouped stream ranks by its group's current priority, and follows a move to another
	/// group; a transport that cannot see acknowledgements reports its bytes acknowledged.
	#[test]
	fn a_listed_stream_ranks_by_its_group() {
		let session = Session::new(SinkSession::new(Default::default()).with_groups());
		let mut low = poll::Session::send_group(&session, 1).expect("the sink opens groups");
		let high = poll::Session::send_group(&session, 5).expect("the sink opens groups");
		let mut stream = open_group(
			&session,
			SendOptions {
				group: Some(&low),
				priority: 2,
			},
		);
		let grouped = |session: &Session| match session.open_streams()[0].0 {
			open::Rank::Grouped { group, stream, .. } => (group, stream),
			open::Rank::Ungrouped { .. } => panic!("the stream is in a group"),
		};
		assert_eq!(grouped(&session), (1, 2));

		poll::SendGroup::set_priority(&mut low, 3);
		assert_eq!(grouped(&session), (3, 2));
		poll::SendStream::set_group(&mut stream, &high);
		assert_eq!(grouped(&session), (5, 2));
		assert!(session.open_streams()[0].1.acknowledged());
	}

	/// A stream accepted while the connection window was held back, such as one buffered
	/// before admission, paces at the window in force once ingest reads it.
	#[test]
	fn a_stream_paces_at_the_window_widened_after_it_arrived() {
		let gate = kio::Producer::new(true);
		let session = Session::new(
			SinkSession::accepted_bi(gate.consume())
				.with_receive_window(ReceiveWindow::new(std::num::NonZeroU64::new(10), 20)),
		);
		let mut early = accept(&session);
		let limits = Limits {
			receive_window: std::num::NonZeroU64::new(100),
		};
		session.set_limits(limits).unwrap();
		assert_eq!(early.window(), 20);
	}

	/// A transport that reports no window never pauses a stream, since nothing bounds what
	/// it would hold; nor does a stream that outlived its session.
	#[test]
	fn an_unbounded_stream_never_pauses() {
		let gate = kio::Producer::new(true);
		let session = Session::new(SinkSession::accepted_bi(gate.consume()));
		assert!(!accept(&session).pause());

		let session = Session::new(
			SinkSession::accepted_bi(gate.consume())
				.with_receive_window(ReceiveWindow::new(std::num::NonZeroU64::new(100), 20)),
		);
		let mut stream = accept(&session);
		drop(session);
		assert!(!stream.pause());
	}
}
