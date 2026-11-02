//! The session: shared state and the handles.
//!
//! [`mux::Connection`] owns the streams, flow control, and timers, and the
//! [`Driver`](crate::Driver) moves its records over the transport. The handles call straight
//! into the connection under its lock and park on the per-stream waiter lists the driver
//! wakes, the way the QUIC drivers do.

use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Poll, ready};
use std::time::Duration;

use bytes::{Buf, Bytes};
use moq_net::transport::{self as net, Address, poll};
use moq_quic::stream::{Dir, Id as StreamId};
use moq_quic::{Side, VarInt, connection::Error as ConnectionError, mux, recv, send};
use rustc_hash::FxHashMap;

use crate::transport::Transport;
use crate::{Config, Driver, Error, SharedSocketStats};

/// Everything the handles and the driver share.
pub(crate) struct Inner {
	state: Mutex<State>,
	/// Updated by the driver, so stats never take the lock.
	pub(crate) wire: net::Wire,
	socket_stats: Option<SharedSocketStats>,
	address: Address,
}

pub(crate) struct State {
	pub(crate) conn: mux::Connection,
	/// Wakes the driver; handles kick it after mutating the connection so fresh egress
	/// reaches the transport.
	pub(crate) driver: kio::WaiterList,

	established: bool,
	establish_waiters: kio::WaiterList,
	accept_bi_waiters: kio::WaiterList,
	accept_uni_waiters: kio::WaiterList,
	/// Whoever is blocked on the peer's MAX_STREAMS credit.
	open_waiters: kio::WaiterList,

	/// Per-stream read/write parking, keyed by stream id.
	readable: FxHashMap<StreamId, kio::WaiterList>,
	writable: FxHashMap<StreamId, kio::WaiterList>,
	/// Send streams waiting for their end: a FIN serialized, or a `STOP_SENDING`.
	finishing: FxHashMap<StreamId, kio::WaiterList>,

	datagram_waiters: kio::WaiterList,

	/// The terminal error, set exactly once; everything fails with it after.
	closed: Option<Error>,
	closed_waiters: kio::WaiterList,
	/// The error for a close this side asked for, published once the driver has delivered
	/// the close frame, so a caller that exits the moment `poll_closed` resolves already
	/// handed it to the transport.
	pub(crate) local_close: Option<Error>,
}

impl State {
	/// Terminate with `err` (the first one wins) and wake absolutely everyone.
	pub(crate) fn fail(&mut self, err: Error) {
		if self.closed.is_some() {
			return;
		}
		self.closed = Some(err);
		self.driver.wake();
		self.closed_waiters.wake();
		self.establish_waiters.wake();
		self.accept_bi_waiters.wake();
		self.accept_uni_waiters.wake();
		self.open_waiters.wake();
		self.datagram_waiters.wake();
		for waiters in self.readable.values_mut() {
			waiters.wake();
		}
		for waiters in self.writable.values_mut() {
			waiters.wake();
		}
		for waiters in self.finishing.values_mut() {
			waiters.wake();
		}
	}

	/// Apply every pending connection event to the parking tables.
	pub(crate) fn sweep(&mut self) {
		while let Some(event) = self.conn.poll() {
			match event {
				mux::Event::Connected => {
					self.established = true;
					self.establish_waiters.wake();
				}
				mux::Event::Stream(event) => self.sweep_stream(event),
				mux::Event::DatagramReceived => self.datagram_waiters.wake(),
				mux::Event::ConnectionLost { reason } => {
					let local = matches!(reason, ConnectionError::LocallyClosed { .. });
					let err = Error::lost(reason);
					match local {
						// The driver publishes a local close once its frame is out.
						true => self.local_close = Some(err),
						false => self.fail(err),
					}
				}
			}
		}
	}

	fn sweep_stream(&mut self, event: moq_quic::StreamEvent) {
		use moq_quic::StreamEvent;

		// Waking removes the entry: a still-interested poller re-registers on its next poll,
		// so the maps only hold streams somebody is parked on.
		match event {
			StreamEvent::Opened { dir: Dir::Bi } => self.accept_bi_waiters.wake(),
			StreamEvent::Opened { dir: Dir::Uni } => self.accept_uni_waiters.wake(),
			StreamEvent::Available { .. } => self.open_waiters.wake(),
			StreamEvent::Readable { id } => wake(&mut self.readable, id),
			StreamEvent::Writable { id } => wake(&mut self.writable, id),
			StreamEvent::Finished { id } => wake(&mut self.finishing, id),
			StreamEvent::Stopped { id, .. } => {
				wake(&mut self.finishing, id);
				// A writer blocked on capacity has to learn it will never come.
				wake(&mut self.writable, id);
			}
			// Nothing waits on a send stream's acknowledgment yet.
			StreamEvent::Acked { .. } => {}
		}
	}

	/// Drop send stream `id`'s bookkeeping, parking and its end included.
	fn forget_send(&mut self, id: StreamId) {
		self.finishing.remove(&id);
		self.writable.remove(&id);
		self.conn.send_stream(id).release();
	}
}

fn wake(lists: &mut FxHashMap<StreamId, kio::WaiterList>, id: StreamId) {
	if let Some(mut waiters) = lists.remove(&id) {
		waiters.wake();
	}
}

impl Inner {
	pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
		self.state.lock().unwrap()
	}

	/// Close with an APPLICATION_CLOSE, reporting it once it is out.
	fn close(&self, code: u64, reason: &str) {
		let mut state = self.lock();
		if state.closed.is_some() || state.conn.is_closed() {
			return;
		}
		state
			.conn
			.close(VarInt::from_u64(code).unwrap_or(VarInt::MAX), reason.to_string());
		state.sweep();
		state.driver.wake();
	}
}

/// Closes the session once the last [`Session`] handle drops.
struct Guard(Arc<Inner>);

impl Drop for Guard {
	fn drop(&mut self) {
		self.0.close(0, "");
	}
}

/// A multiplexed session over a reliable transport.
///
/// Clones share the session, and every operation parks the caller's own waiter. Dropping the
/// last clone closes the session with an APPLICATION_CLOSE of code 0; streams outliving it
/// fail.
#[derive(Clone)]
pub struct Session {
	inner: Arc<Inner>,
	// Held for its drop, which closes the session once the last clone goes.
	_guard: Arc<Guard>,
	/// The application protocol, cached at establishment so `protocol()` can borrow it.
	protocol: Option<String>,
	/// What the TLS handshake learned about the peer, on a session over TLS.
	#[cfg(feature = "tls")]
	pub(crate) tls: Option<Arc<crate::tls::Handshake>>,
}

impl Session {
	/// Open a client-side session over the given transport, waiting until it is established
	/// before returning it with the [`Driver`] that runs it.
	///
	/// "Established" means the peer's transport parameters have been received and applied,
	/// so [`protocol`](Self::protocol) returns its final value. The handshake polls the driver
	/// itself; spawn the driver once this returns. Bounded by [`Config::handshake_timeout`]:
	/// if the peer completes the transport handshake but never sends its parameters, this
	/// returns [`Error::HandshakeTimeout`] rather than hanging.
	pub async fn connect<T: Transport>(transport: T, config: Config) -> Result<(Session, Driver<T>), Error> {
		let (session, mut driver) = Self::new(transport, Side::Client, config)?;
		let session = session.established(&mut driver).await?;
		Ok((session, driver))
	}

	/// Open a server-side session over the given transport, waiting until it is
	/// established. See [`Session::connect`] for the semantics.
	pub async fn accept<T: Transport>(transport: T, config: Config) -> Result<(Session, Driver<T>), Error> {
		let (session, mut driver) = Self::new(transport, Side::Server, config)?;
		let session = session.established(&mut driver).await?;
		Ok((session, driver))
	}

	/// Start a session over the transport without waiting for the handshake, for callers
	/// that resolved the protocol out of band (the WebSocket subprotocol, say).
	pub(crate) fn new<T: Transport>(mut transport: T, side: Side, config: Config) -> Result<(Self, Driver<T>), Error> {
		let conn = mux::Connection::new(config.to_mux()?, side, kio::clock::now());
		transport.bind(&config);
		let inner = Arc::new(Inner {
			state: Mutex::new(State {
				conn,
				driver: kio::WaiterList::new(),
				established: false,
				establish_waiters: kio::WaiterList::new(),
				accept_bi_waiters: kio::WaiterList::new(),
				accept_uni_waiters: kio::WaiterList::new(),
				open_waiters: kio::WaiterList::new(),
				readable: FxHashMap::default(),
				writable: FxHashMap::default(),
				finishing: FxHashMap::default(),
				datagram_waiters: kio::WaiterList::new(),
				closed: None,
				closed_waiters: kio::WaiterList::new(),
				local_close: None,
			}),
			wire: net::Wire::new(),
			socket_stats: transport.socket_stats(),
			address: transport.address(),
		});
		let session = Self {
			_guard: Arc::new(Guard(inner.clone())),
			inner: inner.clone(),
			protocol: config.negotiated_protocol(),
			#[cfg(feature = "tls")]
			tls: None,
		};
		Ok((session, Driver::new(inner, transport)))
	}

	/// Wait for the peer's transport parameters, then report the negotiated protocol.
	///
	/// Polls `driver` meanwhile, since nobody has spawned it yet.
	async fn established<T: Transport>(mut self, driver: &mut Driver<T>) -> Result<Self, Error> {
		let inner = self.inner.clone();
		kio::wait(|waiter| {
			// A finished driver already published why, so only the state needs checking.
			let _ = waiter.poll_future(std::pin::Pin::new(&mut *driver));
			let mut state = inner.lock();
			if state.established {
				return Poll::Ready(Ok(()));
			}
			if let Some(err) = &state.closed {
				return Poll::Ready(Err(err.clone()));
			}
			waiter.register(&mut state.establish_waiters);
			Poll::Pending
		})
		.await?;
		if self.protocol.is_none() {
			self.protocol = inner.lock().conn.protocol().map(str::to_owned);
		}
		Ok(self)
	}

	/// The application protocol: negotiated in-band, or out of band before the session
	/// started.
	pub fn protocol(&self) -> Option<&str> {
		self.protocol.as_deref()
	}

	/// The wire format this session speaks.
	pub fn version(&self) -> crate::Version {
		self.inner.lock().conn.version()
	}

	/// Change the windows and stream counts `limits` sets, leaving the others as they are, such
	/// as to widen a session once its peer is known.
	///
	/// Refuses, changing nothing, a rate (QMux never paces), a receive window below moq-net's
	/// [`MIN_RECEIVE_WINDOW`](net::MIN_RECEIVE_WINDOW), and limits a peer would refuse. A window
	/// only grows at once: credit already granted cannot be taken back.
	pub fn set_limits(&self, limits: moq_quic::Limits) -> Result<(), Error> {
		crate::config::check_limits(&limits)?;
		if limits.receive_rate.is_some() || limits.send_rate.is_some() {
			return Err(Error::InvalidConfig("qmux does not pace, so it takes no rate"));
		}
		let mut state = self.inner.lock();
		state.conn.set_limits(limits)?;
		state.driver.wake();
		Ok(())
	}

	/// Open a send group at `priority`, for streams to join with
	/// [`SendStream::set_group`](poll::SendStream::set_group) and datagrams with
	/// [`SendOptions`](poll::SendOptions).
	fn send_group(&self, priority: i64) -> SendGroup {
		let id = self.inner.lock().conn.send_groups().open(priority);
		SendGroup(Arc::new(GroupInner {
			inner: self.inner.clone(),
			id: Some(id),
		}))
	}

	fn poll_accept_dir(&self, waiter: &kio::Waiter, dir: Dir) -> Poll<Result<StreamId, Error>> {
		let mut state = self.inner.lock();
		if let Some(id) = state.conn.streams().accept(dir) {
			return Poll::Ready(Ok(id));
		}
		if let Some(err) = &state.closed {
			return Poll::Ready(Err(err.clone()));
		}
		match dir {
			Dir::Bi => waiter.register(&mut state.accept_bi_waiters),
			Dir::Uni => waiter.register(&mut state.accept_uni_waiters),
		}
		Poll::Pending
	}

	fn poll_open_dir(
		&self,
		waiter: &kio::Waiter,
		dir: Dir,
		options: poll::SendOptions<'_, SendGroup>,
	) -> Poll<Result<StreamId, Error>> {
		let mut state = self.inner.lock();
		if let Some(err) = &state.closed {
			return Poll::Ready(Err(err.clone()));
		}
		// The peer's MAX_STREAMS credit is what gates us: `open` hands back nothing while it
		// is spent.
		if let Some(id) = state.conn.streams().open(dir, to_quic(options)) {
			state.driver.wake();
			return Poll::Ready(Ok(id));
		}
		waiter.register(&mut state.open_waiters);
		Poll::Pending
	}
}

impl std::fmt::Debug for Session {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Session")
			.field("protocol", &self.protocol)
			.finish_non_exhaustive()
	}
}

/// The options as the connection knows them.
fn to_quic<'a>(options: poll::SendOptions<'a, SendGroup>) -> send::Options<'a> {
	send::Options {
		group: options.group.map(SendGroup::id),
		priority: options.priority,
	}
}

impl poll::Session for Session {
	type SendStream = SendStream;
	type RecvStream = RecvStream;
	type Error = Error;

	fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<RecvStream, Error>> {
		self.poll_accept_dir(waiter, Dir::Uni)
			.map_ok(|id| RecvStream::new(self.inner.clone(), id))
	}

	fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<(SendStream, RecvStream), Error>> {
		self.poll_accept_dir(waiter, Dir::Bi).map_ok(|id| {
			(
				SendStream::new(self.inner.clone(), id),
				RecvStream::new(self.inner.clone(), id),
			)
		})
	}

	fn poll_open_uni(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, SendGroup>,
	) -> Poll<Result<SendStream, Error>> {
		self.poll_open_dir(waiter, Dir::Uni, options)
			.map_ok(|id| SendStream::new(self.inner.clone(), id))
	}

	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, SendGroup>,
	) -> Poll<Result<(SendStream, RecvStream), Error>> {
		self.poll_open_dir(waiter, Dir::Bi, options).map_ok(|id| {
			(
				SendStream::new(self.inner.clone(), id),
				RecvStream::new(self.inner.clone(), id),
			)
		})
	}

	fn poll_open_request(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, SendGroup>,
	) -> Poll<Result<Option<poll::BiStreams<Self>>, Error>> {
		let mut state = self.inner.lock();
		if let Some(err) = &state.closed {
			return Poll::Ready(Err(err.clone()));
		}
		// A session started on an out-of-band protocol (a WebSocket subprotocol) runs before
		// the peer's transport parameters arrive. Until then its credit is unknown, not spent.
		if !state.established {
			waiter.register(&mut state.establish_waiters);
			return Poll::Pending;
		}
		let id = state.conn.streams().open(Dir::Bi, to_quic(options));
		if id.is_some() {
			state.driver.wake();
		}
		drop(state);
		Poll::Ready(Ok(id.map(|id| {
			(
				SendStream::new(self.inner.clone(), id),
				RecvStream::new(self.inner.clone(), id),
			)
		})))
	}

	fn send_group(&self, priority: i64) -> Option<SendGroup> {
		Some(Session::send_group(self, priority))
	}

	fn send_datagram(&self, payload: Bytes, options: poll::SendOptions<'_, SendGroup>) -> Result<(), Error> {
		let mut state = self.inner.lock();
		if let Some(err) = &state.closed {
			return Err(err.clone());
		}
		match state.conn.datagrams().send(payload, to_quic(options)) {
			Ok(()) => {}
			Err(send::Error::TooLarge) => return Err(Error::DatagramTooLarge),
			Err(_) => return Err(Error::DatagramsUnsupported),
		}
		state.driver.wake();
		Ok(())
	}

	fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>> {
		let mut state = self.inner.lock();
		if let Some(datagram) = state.conn.datagrams().recv() {
			return Poll::Ready(Ok(datagram));
		}
		if let Some(err) = &state.closed {
			return Poll::Ready(Err(err.clone()));
		}
		waiter.register(&mut state.datagram_waiters);
		Poll::Pending
	}

	fn max_datagram_size(&self) -> usize {
		self.inner.lock().conn.datagrams().max_size().unwrap_or(0)
	}

	fn protocol(&self) -> Option<&str> {
		Session::protocol(self)
	}

	fn address(&self) -> Address {
		self.inner.address
	}

	fn close(&self, code: u32, reason: &str) {
		self.inner.close(code.into(), reason);
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		let mut state = self.inner.lock();
		if let Some(err) = &state.closed {
			return Poll::Ready(err.clone());
		}
		waiter.register(&mut state.closed_waiters);
		Poll::Pending
	}

	fn metrics(&self) -> impl net::Metrics {
		let state = self.inner.lock();
		// The kernel's view of the socket underneath beats a QX_PING sample: it is there
		// from the first byte and costs no traffic.
		let socket = self.inner.socket_stats.as_ref();
		Metrics {
			rtt: socket.and_then(|stats| stats.rtt()).or(state.conn.rtt()),
			rate: socket.and_then(|stats| stats.estimated_send_rate()),
			evicted: state.conn.credit_stats().evicted,
		}
	}

	fn wire(&self) -> Option<net::Wire> {
		Some(self.inner.wire.clone())
	}

	fn receive_window(&self) -> Option<net::ReceiveWindow> {
		let state = self.inner.lock();
		Some(net::ReceiveWindow::new(
			state.conn.receive_window(),
			state.conn.stream_receive_window(),
		))
	}
}

/// A snapshot of the session's metrics; its [`net::Wire`] holds the byte counts.
struct Metrics {
	rtt: Option<Duration>,
	rate: Option<u64>,
	evicted: u64,
}

impl net::Metrics for Metrics {
	fn rtt(&self) -> Option<Duration> {
		self.rtt
	}

	fn estimated_send_rate(&self) -> Option<u64> {
		self.rate
	}

	fn streams_evicted(&self) -> Option<u64> {
		Some(self.evicted)
	}
}

/// A send group on a [`Session`].
///
/// Streams that join a group share bytes with the session's other groups: a higher
/// priority group preempts every lower one, and groups of equal priority get an equal share
/// of bytes however many streams each has queued. Clones share one group, released once the
/// last clone drops and its streams finish.
#[derive(Clone)]
pub struct SendGroup(Arc<GroupInner>);

struct GroupInner {
	inner: Arc<Inner>,
	/// Taken only on drop.
	id: Option<send::Group>,
}

impl SendGroup {
	fn id(&self) -> &send::Group {
		self.0.id.as_ref().expect("only taken on drop")
	}
}

impl poll::SendGroup for SendGroup {
	fn set_priority(&mut self, priority: i64) {
		let mut state = self.0.inner.lock();
		state.conn.send_groups().set_priority(self.id(), priority);
		state.driver.wake();
	}
}

impl Drop for GroupInner {
	fn drop(&mut self) {
		if let Some(id) = self.id.take() {
			self.inner.lock().conn.send_groups().close(id);
		}
	}
}

impl std::fmt::Debug for SendGroup {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("SendGroup").finish_non_exhaustive()
	}
}

/// An outgoing stream. Dropping it unfinished resets it with code 0.
pub struct SendStream {
	inner: Arc<Inner>,
	id: StreamId,
	/// The FIN was queued; further writes are refused and `poll_closed` waits for it to go
	/// out.
	fin: bool,
	/// We reset the stream; it is as closed as it will ever be.
	reset: bool,
}

impl SendStream {
	fn new(inner: Arc<Inner>, id: StreamId) -> Self {
		Self {
			inner,
			id,
			fin: false,
			reset: false,
		}
	}

	fn reset_with(&mut self, code: u32) {
		if self.reset {
			return;
		}
		let code = VarInt::from_u32(code);
		let mut state = self.inner.lock();
		// Err means the stream is already gone, which is what we wanted.
		let _ = state.conn.send_stream(self.id).reset(code);
		state.driver.wake();
		self.reset = true;
	}
}

impl poll::SendStream for SendStream {
	type Error = Error;
	type Group = SendGroup;

	fn poll_write(&mut self, waiter: &kio::Waiter, mut buf: &[u8]) -> Poll<Result<usize, Error>> {
		self.poll_write_buf(waiter, &mut buf)
	}

	fn poll_write_buf<B: Buf>(&mut self, waiter: &kio::Waiter, buf: &mut B) -> Poll<Result<usize, Error>> {
		if self.fin || self.reset {
			return Poll::Ready(Err(Error::StreamClosed));
		}
		let mut state = self.inner.lock();
		if let Some(err) = &state.closed {
			return Poll::Ready(Err(err.clone()));
		}
		// Takes only what the connection accepts, which for a `Bytes` source is no copy at all.
		match state.conn.send_stream(self.id).write(buf) {
			Ok(n) => {
				state.driver.wake();
				Poll::Ready(Ok(n))
			}
			Err(send::Error::Blocked) => {
				waiter.register(state.writable.entry(self.id).or_default());
				Poll::Pending
			}
			Err(send::Error::Stopped(code)) => Poll::Ready(Err(Error::StreamStop(code.into_inner()))),
			Err(_) => Poll::Ready(Err(Error::StreamClosed)),
		}
	}

	fn set_priority(&mut self, priority: i64) {
		let mut state = self.inner.lock();
		let _ = state.conn.send_stream(self.id).set_priority(priority);
	}

	fn set_group(&mut self, group: &SendGroup) {
		let mut state = self.inner.lock();
		let _ = state.conn.send_stream(self.id).set_group(Some(group.id()));
	}

	fn finish(&mut self) -> Result<(), Error> {
		if self.fin || self.reset {
			return Ok(());
		}
		let mut state = self.inner.lock();
		match state.conn.send_stream(self.id).finish() {
			Ok(()) => {}
			// A STOP_SENDING beat us here; carry its code like `poll_write` does. The stream is
			// over without our reset, so `poll_closed` reports the stop the sweep recorded.
			Err(send::Error::Stopped(code)) => {
				self.fin = true;
				return Err(Error::StreamStop(code.into_inner()));
			}
			// Already finished or reset, so the FIN it wanted is out.
			Err(_) => {}
		}
		state.driver.wake();
		self.fin = true;
		Ok(())
	}

	fn reset(&mut self, code: u32) {
		self.reset_with(code);
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		if self.reset {
			return Poll::Ready(Ok(()));
		}
		let mut state = self.inner.lock();
		// A finish is acknowledged once its FIN goes to the transport, which on a reliable
		// transport is delivery.
		match state.conn.send_stream(self.id).end() {
			Ok(Some(send::End::Stopped(code))) => Poll::Ready(Err(Error::StreamStop(code.into_inner()))),
			Ok(Some(_)) => Poll::Ready(Ok(())),
			// The end never came. If the session died first, the caller cannot read success
			// as "every byte arrived".
			Ok(None) | Err(_) => match &state.closed {
				Some(err) => Poll::Ready(Err(err.clone())),
				None => {
					waiter.register(state.finishing.entry(self.id).or_default());
					Poll::Pending
				}
			},
		}
	}
}

impl Drop for SendStream {
	fn drop(&mut self) {
		let mut state = self.inner.lock();
		// Reset before giving up the ID, which refuses every later call on it.
		if !self.fin && !self.reset {
			let _ = state.conn.send_stream(self.id).reset(VarInt::from_u32(0));
			state.driver.wake();
		}
		state.forget_send(self.id);
	}
}

impl std::fmt::Debug for SendStream {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("SendStream").field("id", &self.id).finish()
	}
}

/// An incoming stream. Dropping it sends STOP_SENDING with code 0, which also frees its state.
///
/// Leased bytes keep their connection credit until released, so `receive_window` bounds what
/// every stream's incomplete messages hold together. When they fill it and no reader can make
/// progress, the stream ranked lowest by [`set_priority`](poll::RecvStream::set_priority) is
/// evicted: its next lease fails with [`Error::Evicted`].
pub struct RecvStream {
	inner: Arc<Inner>,
	id: StreamId,
	/// The peer reset the stream with this code. Observing it frees the connection's stream
	/// state, so later calls report it from here.
	reset: Option<u64>,
	/// We stopped the stream; it is gone.
	stopped: bool,
}

impl RecvStream {
	fn new(inner: Arc<Inner>, id: StreamId) -> Self {
		Self {
			inner,
			id,
			reset: None,
			stopped: false,
		}
	}

	/// Take the connection's lease, which [`RecvLease`] wraps once this returns: the borrow of
	/// `self` it holds cannot start inside a poll that may return `Pending`.
	fn poll_take(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<moq_quic::recv::Lease, Error>> {
		if let Some(code) = self.reset {
			return Poll::Ready(Err(Error::StreamReset(code)));
		}
		if self.stopped {
			return Poll::Ready(Err(Error::StreamClosed));
		}
		let mut state = self.inner.lock();
		let (result, transmit) = state.conn.recv_stream(self.id).lease(want);
		// Freed credit, or an eviction whose reader the driver's sweep wakes.
		if transmit.should_transmit() {
			state.driver.wake();
		}
		match result {
			Ok(lease) => Poll::Ready(Ok(lease)),
			Err(recv::Error::Blocked) => {
				if let Some(err) = &state.closed {
					return Poll::Ready(Err(err.clone()));
				}
				waiter.register(state.readable.entry(self.id).or_default());
				Poll::Pending
			}
			Err(recv::Error::Reset(code)) => {
				self.reset = Some(code.into_inner());
				Poll::Ready(Err(Error::StreamReset(code.into_inner())))
			}
			Err(recv::Error::Evicted) => Poll::Ready(Err(Error::Evicted)),
			Err(recv::Error::TooLarge) => Poll::Ready(Err(Error::LeaseTooLarge)),
			// The stream is gone, which only a lost connection does to one we hold.
			Err(_) => Poll::Ready(Err(state.closed.clone().unwrap_or(Error::Closed))),
		}
	}
}

impl poll::RecvStream for RecvStream {
	type Error = Error;
	type Lease<'a> = RecvLease<'a>;

	fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<RecvLease<'_>, Error>> {
		let lease = ready!(self.poll_take(waiter, want))?;
		Poll::Ready(Ok(RecvLease {
			stream: self,
			lease: Some(lease),
		}))
	}

	fn set_priority(&mut self, priority: i64) {
		if self.reset.is_some() || self.stopped {
			return;
		}
		self.inner.lock().conn.recv_stream(self.id).set_priority(priority);
	}

	fn stop(&mut self, code: u32) {
		if self.stopped || self.reset.is_some() {
			return;
		}
		let mut state = self.inner.lock();
		state.readable.remove(&self.id);
		// Err means the stream is already gone, which is what we wanted.
		let _ = state.conn.recv_stream(self.id).stop(VarInt::from_u32(code));
		state.driver.wake();
		self.stopped = true;
	}

	fn poll_reset(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		if let Some(code) = self.reset {
			return Poll::Ready(Error::StreamReset(code));
		}
		if self.stopped {
			return Poll::Ready(Error::StreamClosed);
		}
		let mut state = self.inner.lock();
		match state.conn.recv_stream(self.id).received_reset() {
			// Every byte arrived, so nothing is left to reset.
			Err(_) => Poll::Pending,
			Ok(Some(code)) => {
				// Observing it freed the stream, which may owe the peer credit.
				self.reset = Some(code.into_inner());
				state.driver.wake();
				Poll::Ready(Error::StreamReset(code.into_inner()))
			}
			Ok(None) => {
				if let Some(err) = &state.closed {
					return Poll::Ready(err.clone());
				}
				// A reset notifies readers, since it is an immediate read error.
				waiter.register(state.readable.entry(self.id).or_default());
				Poll::Pending
			}
		}
	}
}

impl Drop for RecvStream {
	fn drop(&mut self) {
		if self.stopped || self.reset.is_some() {
			return;
		}
		let mut state = self.inner.lock();
		state.readable.remove(&self.id);
		// A leased stream keeps its state until stopped, even once it ended; Err means it is
		// already gone.
		let _ = state.conn.recv_stream(self.id).stop(VarInt::from_u32(0));
		state.driver.wake();
	}
}

impl std::fmt::Debug for RecvStream {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("RecvStream").field("id", &self.id).finish()
	}
}

/// Unreleased bytes of a [`RecvStream`], read as a [`Buf`].
///
/// It owns the bytes, so the session carries on while it is held. [`release`](poll::Lease::release)
/// returns the credit of the bytes read; dropping it keeps their credit, as for a message read
/// only in part. Either way the bytes not read are leased again next time.
pub struct RecvLease<'a> {
	stream: &'a mut RecvStream,
	/// Taken only to hand it back.
	lease: Option<moq_quic::recv::Lease>,
}

impl RecvLease<'_> {
	fn lease(&self) -> &moq_quic::recv::Lease {
		self.lease.as_ref().expect("only taken to hand it back")
	}

	fn lease_mut(&mut self) -> &mut moq_quic::recv::Lease {
		self.lease.as_mut().expect("only taken to hand it back")
	}
}

impl Buf for RecvLease<'_> {
	fn remaining(&self) -> usize {
		self.lease().remaining()
	}

	fn chunk(&self) -> &[u8] {
		self.lease().chunk()
	}

	fn advance(&mut self, n: usize) {
		self.lease_mut().advance(n);
	}

	fn copy_to_bytes(&mut self, n: usize) -> Bytes {
		self.lease_mut().copy_to_bytes(n)
	}
}

impl poll::Lease for RecvLease<'_> {
	fn chunks(&self) -> impl Iterator<Item = &[u8]> {
		self.lease().chunks()
	}

	fn is_fin(&self) -> bool {
		self.lease().is_fin()
	}

	fn release(mut self) {
		let lease = self.lease.take().expect("only taken to hand it back");
		let mut state = self.stream.inner.lock();
		if state.conn.recv_stream(self.stream.id).release(lease).should_transmit() {
			state.driver.wake();
		}
	}
}

impl Drop for RecvLease<'_> {
	fn drop(&mut self) {
		let Some(lease) = self.lease.take() else {
			return;
		};
		let mut state = self.stream.inner.lock();
		if state.conn.recv_stream(self.stream.id).restore(lease).should_transmit() {
			state.driver.wake();
		}
	}
}

impl std::fmt::Debug for RecvLease<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("RecvLease")
			.field("id", &self.stream.id)
			.field("remaining", &self.remaining())
			.finish()
	}
}

#[cfg(all(test, any(feature = "tcp", all(unix, feature = "uds"))))]
mod tests;
