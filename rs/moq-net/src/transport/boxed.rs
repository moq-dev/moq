//! The one transport type the session code is compiled for.
//!
//! [`Client`](crate::Client) and [`Server`](crate::Server) box the caller's transport
//! here before running any protocol code, so the lite and ietf drivers are
//! monomorphized once per binary instead of once per transport and per crate that
//! awaits them. Each transport only instantiates the thin forwarding impls below.

use std::any::Any;
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
	pub(crate) fn open_streams(&self) -> Vec<open::OpenStream> {
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
	/// The stream credit reserved by streams this session stopped reading to pace a group's
	/// writer, at most half the connection window; see [`ReceiveWindow`].
	paused: AtomicU64,
	session: S,
}

impl Shared<dyn ErasedSession> {
	/// Reserve the credit of one more paused stream, unless it would push the paused total
	/// over half the connection window, returning how much it reserved. Without a connection
	/// window a paused stream holds only its own credit, so it reserves nothing.
	fn reserve(&self) -> Option<u64> {
		let window = self.session.receive_window()?;
		let Some(connection) = window.connection else {
			return Some(0);
		};
		let charge = window.stream.min(connection.get());
		let cap = connection.get() / 2;
		self.paused
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |paused| {
				paused.checked_add(charge).filter(|total| *total <= cap)
			})
			.ok()?;
		Some(charge)
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

/// A boxed outgoing stream.
pub(crate) struct SendStream(Sender);

enum Sender {
	Own(Box<dyn ErasedSend>),
	/// A group stream its session lists, so the list can read it too.
	Listed(Listing),
}

impl SendStream {
	/// Box a stream, keeping its own send-group type behind the boundary.
	#[cfg(test)]
	pub(crate) fn new(stream: impl poll::SendStream<Group: Send + Sync> + Send + Sync) -> Self {
		Self(Sender::Own(Box::new(stream)))
	}

	fn own(stream: impl poll::SendStream<Group: Send + Sync> + Send + Sync + 'static) -> Self {
		Self(Sender::Own(Box::new(stream)))
	}

	/// Whether the session reset this group stream to make room for a higher-ranked open.
	pub(crate) fn preempted(&self) -> bool {
		match &self.0 {
			Sender::Own(_) => false,
			Sender::Listed(listing) => listing.listed.lock().preempted,
		}
	}

	/// Run `f` on the transport's stream, then leave the session's list if `unlist`.
	fn with<T>(&mut self, unlist: bool, f: impl FnOnce(&mut dyn ErasedSend) -> T) -> T {
		match &mut self.0 {
			Sender::Own(stream) => f(stream.as_mut()),
			Sender::Listed(listing) => {
				let res = f(listing.listed.lock().stream.as_mut());
				if unlist {
					listing.unlist();
				}
				res
			}
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
			Sender::Own(stream) => f(stream.as_mut()),
			Sender::Listed(listing) => {
				let mut state = listing.listed.lock();
				if let Some(err) = state.preempted() {
					return Poll::Ready(Err(err));
				}
				let res = f(state.stream.as_mut());
				if res.is_pending() {
					state.waiters.register(waiter);
				}
				res
			}
		}
	}

	fn get(&self) -> Read<'_> {
		match &self.0 {
			Sender::Own(stream) => Read::Own(stream.as_ref()),
			Sender::Listed(listing) => Read::Listed(listing.listed.lock()),
		}
	}
}

/// A boxed stream borrowed for a read through `&self`.
enum Read<'a> {
	Own(&'a (dyn ErasedSend + 'static)),
	Listed(std::sync::MutexGuard<'a, ListedState>),
}

impl std::ops::Deref for Read<'_> {
	type Target = dyn ErasedSend;

	fn deref(&self) -> &Self::Target {
		match self {
			Read::Own(stream) => *stream,
			Read::Listed(state) => state.stream.as_ref(),
		}
	}
}

/// An open group stream, shared by the task writing it and its session's list.
pub(super) struct Listed {
	state: Mutex<ListedState>,
}

struct ListedState {
	stream: Box<dyn ErasedSend>,
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

	/// The stream's rank, and whether the peer acknowledged everything written to it.
	pub(super) fn read(&self) -> open::OpenStream {
		let state = self.lock();
		open::OpenStream {
			rank: open::Rank::new(state.group.as_ref().map(|group| group.read()), state.priority),
			acknowledged: state.stream.acknowledged(),
		}
	}

	/// Reset the stream to make room, failing its writer with the reset's code.
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
			held: 0,
			window: OnceLock::new(),
		}))
	}

	/// How far ingest reads this stream ahead of its group's fastest reader before pausing:
	/// the credit the stream may hold, so a paced hop buffers about what its peer can have
	/// in flight on it.
	pub(crate) fn window(&self) -> u64 {
		self.0.window()
	}

	/// Reserve the receive credit this stream may hold while ingest stops reading it, returning
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
	/// The credit reserved while paused, zero while read.
	held: u64,
	/// The pacing window, read from the session's receive windows when ingest first paces
	/// the stream, so one that arrived before admission widened them paces at the wider ones.
	window: OnceLock<u64>,
}

impl<T> Budgeted<T> {
	/// Return the credit reserved while paused, if any.
	fn release(&mut self) {
		let held = std::mem::take(&mut self.held);
		if held > 0
			&& let Some(shared) = self.session.upgrade()
		{
			shared.paused.fetch_sub(held, Ordering::AcqRel);
		}
	}
}

impl<T> Drop for Budgeted<T> {
	fn drop(&mut self) {
		self.release();
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
	fn set_limits(&self, limits: Limits) -> Result<(), LimitsError>;
	fn export_keying_material(&self, label: &[u8], context: &[u8]) -> Option<[u8; 32]>;
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
	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>>;
	fn acknowledged(&self) -> Option<bool>;
	fn delivered(&self) -> Option<u64>;
	fn poll_delivered(&self, waiter: &kio::Waiter) -> Poll<()>;
}

trait ErasedRecv: Send + Sync {
	fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<Lease<'_>, Error>>;
	fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Error>>;
	fn set_priority(&mut self, priority: i64);
	fn stop(&mut self, code: u32);
	fn poll_reset(&mut self, waiter: &kio::Waiter) -> Poll<Error>;
	fn window(&self) -> u64;
	fn pause(&mut self) -> bool;
	fn resume(&mut self);
}

// Not `Send`: a lease may hold its transport's lock, and never outlives a poll.
trait ErasedLease: Buf {
	fn chunks(&self) -> Box<dyn Iterator<Item = &[u8]> + '_>;
	fn is_fin(&self) -> bool;
	fn release(self: Box<Self>);
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

	fn set_limits(&self, limits: Limits) -> Result<(), LimitsError> {
		poll::Session::set_limits(self, limits)
	}

	fn export_keying_material(&self, label: &[u8], context: &[u8]) -> Option<[u8; 32]> {
		poll::Session::export_keying_material(self, label, context)
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

impl<T: poll::SendStream<Group: Send + Sync> + Send + Sync> ErasedSend for T {
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

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		poll::SendStream::poll_closed(self, waiter).map_err(Error::new)
	}

	fn acknowledged(&self) -> Option<bool> {
		poll::SendStream::acknowledged(self)
	}

	fn delivered(&self) -> Option<u64> {
		poll::SendStream::delivered(self)
	}

	fn poll_delivered(&self, waiter: &kio::Waiter) -> Poll<()> {
		poll::SendStream::poll_delivered(self, waiter)
	}
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

	fn window(&self) -> u64 {
		*self.window.get_or_init(|| {
			let receive = self
				.session
				.upgrade()
				.and_then(|shared| shared.session.receive_window());
			crate::model::pace::window(receive)
		})
	}

	fn pause(&mut self) -> bool {
		if self.held > 0 {
			return true;
		}
		let Some(held) = self.session.upgrade().and_then(|shared| shared.reserve()) else {
			return false;
		};
		self.held = held;
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

	fn set_limits(&self, limits: Limits) -> Result<(), LimitsError> {
		self.inner.session.set_limits(limits)
	}

	fn export_keying_material(&self, label: &[u8], context: &[u8]) -> Option<[u8; 32]> {
		self.inner.session.export_keying_material(label, context)
	}
}

impl poll::SendStream for SendStream {
	type Error = Error;
	type Group = Group;

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
		self.with(false, |stream| stream.set_priority(priority))
	}

	fn set_group(&mut self, group: &Group) {
		if let Sender::Listed(listing) = &self.0 {
			listing.listed.lock().group = Some(group.rank.clone());
		}
		self.with(false, |stream| stream.set_group(group))
	}

	fn finish(&mut self) -> Result<(), Error> {
		if let Sender::Listed(listing) = &self.0
			&& let Some(err) = listing.listed.lock().preempted()
		{
			return Err(err);
		}
		self.with(true, |stream| stream.finish())
	}

	fn reset(&mut self, code: u32) {
		// A preempted stream keeps the code it was reset with.
		if let Sender::Listed(listing) = &self.0
			&& listing.listed.lock().preempted
		{
			return;
		}
		self.with(true, |stream| stream.reset(code))
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		match &self.0 {
			Sender::Own(stream) => stream.poll_closed(waiter),
			Sender::Listed(listing) => {
				let mut state = listing.listed.lock();
				if let Some(err) = state.preempted() {
					return Poll::Ready(Err(err));
				}
				state.waiters.register(waiter);
				state.stream.poll_closed(waiter)
			}
		}
	}

	fn acknowledged(&self) -> Option<bool> {
		self.get().acknowledged()
	}

	fn delivered(&self) -> Option<u64> {
		self.get().delivered()
	}

	fn poll_delivered(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.get().poll_delivered(waiter)
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

	/// Each paused stream reserves its stream window, and the session pauses no more than
	/// fit in half the connection window; a stream resuming or dropped returns its share.
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
		assert_eq!(accept(&session).window(), 20);

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
		session.open_streams().into_iter().map(|stream| stream.rank).collect()
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

	/// A listed stream reports the peer acknowledging what was written to it, and a reset one
	/// never is.
	#[test]
	fn a_listed_stream_reports_its_acknowledgement() {
		let (a, b) = super::super::memory::pair(Default::default());
		let session = Session::new(a);
		let mut stream = open_group(&session, ungrouped(0));
		let acknowledged = || session.open_streams()[0].acknowledged;
		assert_eq!(acknowledged(), Some(true), "nothing written is all acknowledged");

		let waiter = kio::Waiter::noop();
		assert!(matches!(
			poll::SendStream::poll_write(&mut stream, &waiter, b"group"),
			Poll::Ready(Ok(5))
		));
		assert_eq!(acknowledged(), Some(false));
		assert_eq!(
			poll::SendStream::acknowledged(&stream),
			Some(false),
			"the writer reads the same"
		);

		let Poll::Ready(Ok(mut recv)) = poll::Session::poll_accept_uni(&b, &waiter) else {
			panic!("the peer accepts the stream");
		};
		assert!(matches!(
			poll::RecvStream::poll_read_chunk(&mut recv, &waiter, 16),
			Poll::Ready(Ok(Some(_)))
		));
		assert_eq!(acknowledged(), Some(true));

		poll::SendStream::reset(&mut stream, 1);
		assert_eq!(poll::SendStream::acknowledged(&stream), Some(false));
	}

	/// A grouped stream ranks by its group's current priority, and follows a move to another
	/// group; a transport that cannot tell reports no acknowledgement.
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
		let read = |session: &Session| session.open_streams()[0];
		let grouped = |session: &Session| match read(session).rank {
			open::Rank::Grouped { group, stream, .. } => (group, stream),
			open::Rank::Ungrouped { .. } => panic!("the stream is in a group"),
		};
		assert_eq!(grouped(&session), (1, 2));
		assert_eq!(read(&session).acknowledged, None);

		poll::SendGroup::set_priority(&mut low, 3);
		assert_eq!(grouped(&session), (3, 2));
		poll::SendStream::set_group(&mut stream, &high);
		assert_eq!(grouped(&session), (5, 2));
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
		let early = accept(&session);
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
