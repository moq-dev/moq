//! The connection: shared state, the driver task, and the session handle.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::task::{Context, Poll};
use std::time::Instant;

use bytes::Bytes;
use quinn_proto::{ConnectionHandle, Dir, StreamId, VarInt};
use rustc_hash::FxHashMap;

use super::super::{Error, SEGMENT};
use super::endpoint;
use crate::{Handle, udp};

/// The state shared by every handle and the driver, single-threaded behind
/// `Rc<RefCell>`.
pub(crate) type Shared = Rc<Inner>;

/// One GSO train is at most 64 segments; stay a hair under the kernel cap.
const TRAIN_SEGMENTS: usize = 63;

/// Everything the handles and the driver share, single-threaded.
///
/// The connection and the bookkeeping live in separate `RefCell`s so a stream
/// operation can mutate the connection, drop that borrow, and then register a
/// waiter without ever holding both.
pub(crate) struct Inner {
	pub(crate) conn: RefCell<quinn_proto::Connection>,
	pub(crate) state: RefCell<State>,
}

pub(crate) struct State {
	/// Wakes the driver; handles kick it after mutating the connection so
	/// fresh egress reaches the wire.
	driver: kio::WaiterList,

	established: bool,
	establish_waiters: kio::WaiterList,

	/// Whoever is waiting for a peer-initiated stream. quinn-proto hands the
	/// ids out in order, so the queue is its own.
	accept_bi_waiters: kio::WaiterList,
	accept_uni_waiters: kio::WaiterList,

	/// Whoever is blocked on the peer's MAX_STREAMS credit.
	open_waiters: kio::WaiterList,

	/// Per-stream read/write parking, keyed by stream id.
	///
	/// FxHash rather than SipHash on all four of these: they sit on the
	/// driver's per-event path, and quinn-proto assigns the ids.
	readable: FxHashMap<StreamId, kio::WaiterList>,
	writable: FxHashMap<StreamId, kio::WaiterList>,
	/// Send streams waiting for their end: a FIN the peer acknowledged, or a
	/// `STOP_SENDING`.
	finishing: FxHashMap<StreamId, kio::WaiterList>,
	/// Every send stream a handle still holds, and how it ended once the
	/// driver has seen that happen. That is what makes a later close mean
	/// "already delivered" rather than "we never found out".
	///
	/// Keyed on the live handles, because the verdict arrives as an event: a
	/// stream finished and then dropped before its FIN was acknowledged would
	/// otherwise leave an entry nobody can ever remove, one per group, for as
	/// long as the connection lives.
	sends: FxHashMap<StreamId, Option<End>>,

	datagram_recv_waiters: kio::WaiterList,
	datagram_send_waiters: kio::WaiterList,

	/// The terminal error, set exactly once; everything fails with it after.
	closed: Option<Error>,
	closed_waiters: kio::WaiterList,

	/// The socket died under us: the driver stops where it stands rather than
	/// waiting out a drain it could never transmit.
	dead: bool,

	/// The close this side asked for, until the driver has put it on the wire.
	/// quinn raises no event for it, so this is what the terminal error is
	/// built from.
	local_close: Option<(u64, String)>,
}

impl State {
	fn new() -> Self {
		Self {
			driver: kio::WaiterList::new(),
			established: false,
			establish_waiters: kio::WaiterList::new(),
			accept_bi_waiters: kio::WaiterList::new(),
			accept_uni_waiters: kio::WaiterList::new(),
			open_waiters: kio::WaiterList::new(),
			readable: FxHashMap::default(),
			writable: FxHashMap::default(),
			finishing: FxHashMap::default(),
			sends: FxHashMap::default(),
			datagram_recv_waiters: kio::WaiterList::new(),
			datagram_send_waiters: kio::WaiterList::new(),
			closed: None,
			closed_waiters: kio::WaiterList::new(),
			dead: false,
			local_close: None,
		}
	}

	/// Drop send stream `id`'s bookkeeping, parking included.
	fn forget_send(&mut self, id: StreamId) {
		self.sends.remove(&id);
		self.finishing.remove(&id);
		self.writable.remove(&id);
	}

	/// The same for the read half. Only its own table: the two halves of a
	/// bidirectional stream are separate handles, and the other one may still
	/// be parked.
	fn forget_recv(&mut self, id: StreamId) {
		self.readable.remove(&id);
	}

	/// Terminate with `err` (the first one wins) and wake absolutely everyone,
	/// the driver included: an externally failed driver has to notice.
	fn fail(&mut self, err: Error) {
		if self.closed.is_none() {
			self.closed = Some(err);
		}
		self.driver.wake();
		self.closed_waiters.wake();
		self.establish_waiters.wake();
		self.accept_bi_waiters.wake();
		self.accept_uni_waiters.wake();
		self.open_waiters.wake();
		self.datagram_recv_waiters.wake();
		self.datagram_send_waiters.wake();
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
}

impl Inner {
	/// The terminal error, if the connection has one.
	pub(crate) fn closed(&self) -> Option<Error> {
		self.state.borrow().closed.clone()
	}

	/// Wake the driver so it flushes what a handle just queued.
	pub(crate) fn kick(&self) {
		self.state.borrow_mut().driver.wake();
	}

	/// Terminate from outside (the endpoint's socket died): everything fails
	/// with `err`, and the driver exits on its next poll.
	pub(crate) fn fail(&self, err: Error) {
		let mut state = self.state.borrow_mut();
		state.dead = true;
		state.fail(err);
	}

	/// Park `waiter` until stream `id` is writable again.
	pub(crate) fn park_writable(&self, id: StreamId, waiter: &kio::Waiter) {
		let mut state = self.state.borrow_mut();
		waiter.register(state.writable.entry(id).or_default());
	}

	/// Park `waiter` until stream `id` is readable again.
	pub(crate) fn park_readable(&self, id: StreamId, waiter: &kio::Waiter) {
		let mut state = self.state.borrow_mut();
		waiter.register(state.readable.entry(id).or_default());
	}

	/// Park `waiter` until send stream `id` reaches its end.
	pub(crate) fn park_finishing(&self, id: StreamId, waiter: &kio::Waiter) {
		let mut state = self.state.borrow_mut();
		waiter.register(state.finishing.entry(id).or_default());
	}

	/// Start tracking send stream `id`, which a handle now owns.
	///
	/// Seeded from quinn rather than empty: a peer can open a bidirectional
	/// stream and stop it before the application ever accepts it, and that
	/// event arrives with no handle to record it against. quinn still knows,
	/// so a `poll_closed` on the fresh handle reports the stop instead of
	/// waiting for an event that has already been and gone.
	pub(crate) fn track(&self, id: StreamId) {
		// Err means quinn has no send half for the id, which is nothing to
		// report either way.
		let stopped = self.conn.borrow_mut().send_stream(id).stopped().ok().flatten();
		self.state
			.borrow_mut()
			.sends
			.insert(id, stopped.map(|code| End::Stopped(code.into_inner())));
	}

	/// How send stream `id` ended, if the driver saw it end while a handle
	/// still held it.
	pub(crate) fn ended(&self, id: StreamId) -> Option<End> {
		self.state.borrow().sends.get(&id).copied().flatten()
	}

	/// Forget send stream `id`'s bookkeeping; called when its handle drops.
	///
	/// The parking table goes with it. A stream reset on the way out is never
	/// reported writable again, so nothing else would ever remove the entry,
	/// and a peer withholding flow control credit could have streams opened
	/// and cancelled against it without bound.
	pub(crate) fn forget_send(&self, id: StreamId) {
		self.state.borrow_mut().forget_send(id);
	}

	/// The same for the read half, which parks on its own table.
	pub(crate) fn forget_recv(&self, id: StreamId) {
		self.state.borrow_mut().forget_recv(id);
	}

	/// Wake anyone parked on stream `id` being readable.
	pub(crate) fn wake_readable(&self, id: StreamId) {
		let mut state = self.state.borrow_mut();
		if let Some(mut waiters) = state.readable.remove(&id) {
			waiters.wake();
		}
	}

	/// Close the connection with a full-width application code.
	///
	/// The [`web_transport_trait::poll::Session::close`] surface narrows codes
	/// to `u32`; the WebTransport layer maps its codes into HTTP/3's error
	/// space, which needs the whole varint range.
	pub(crate) fn close_code(&self, code: u64, reason: &str) {
		self.conn.borrow_mut().close(
			Instant::now(),
			VarInt::from_u64(code).unwrap_or(VarInt::MAX),
			Bytes::copy_from_slice(reason.as_bytes()),
		);
		// quinn raises no event for a close the application asked for, so the
		// terminal error is ours to publish. Not here though: the driver
		// publishes it once it has staged the CONNECTION_CLOSE, so a caller
		// that stops driving the worker the moment `poll_closed` resolves has
		// at least handed the packet over first.
		let mut state = self.state.borrow_mut();
		state.local_close.get_or_insert((code, reason.to_string()));
		state.driver.wake();
	}
}

/// A QUIC connection driven by a [`crate::Worker`], usable as a MoQ transport.
///
/// Created by [`Endpoint`](super::Endpoint) (or its
/// [`client::connect`](crate::quic::client::connect) /
/// [`server::accept`](crate::quic::server::accept) shorthands), already
/// established. Clones share the connection; each carries its own parking so
/// concurrent pending operations don't trample each other's wakeups. Dropping
/// every handle (and every stream) drops the driver's `Rc` peers, but the
/// driver itself keeps the connection alive until it ends; close explicitly
/// with [`close`](web_transport_trait::poll::Session::close) (which moq's
/// session machine does).
///
/// That close only records the code: the driver task is what frames the
/// CONNECTION_CLOSE and hands it to the socket. Drive the worker until
/// [`poll_closed`](web_transport_trait::poll::Session::poll_closed) resolves
/// before stopping it, or the packet is never built and the peer idles out
/// instead.
pub struct Connection {
	shared: Shared,
	// Retains this clone's waiter registrations across polls.
	park: kio::Park,
	/// The negotiated ALPN, cached at establishment so `protocol()` can
	/// borrow from the handle.
	alpn: Option<String>,
}

impl Connection {
	/// Close the connection with a full-width application code.
	///
	/// The [`web_transport_trait::poll::Session::close`] surface narrows codes
	/// to `u32`; the WebTransport layer maps its codes into HTTP/3's error
	/// space, which needs the whole varint range.
	pub(crate) fn close_code(&self, code: u64, reason: &str) {
		self.shared.close_code(code, reason);
	}

	/// The peer's certificate chain in DER, leaf first, or `None` if it
	/// presented none.
	///
	/// A server only sees one when it asked
	/// ([`ClientAuth`](crate::quic::server::ClientAuth)), and TLS already
	/// validated it against the configured roots by the time this connection
	/// exists: an invalid chain fails the handshake instead. So a `Some` here
	/// is an authenticated peer, and the chain is what names it.
	pub fn peer_chain(&self) -> Option<Vec<Vec<u8>>> {
		let conn = self.shared.conn.borrow();
		let identity = conn.crypto_session().peer_identity()?;
		let chain = identity
			.downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>()
			.ok()?;
		Some(chain.iter().map(|cert| cert.to_vec()).collect())
	}
}

impl Clone for Connection {
	fn clone(&self) -> Self {
		Self {
			shared: self.shared.clone(),
			park: kio::Park::default(),
			alpn: self.alpn.clone(),
		}
	}
}

/// Build a connection's shared state and its driver.
///
/// The driver future does timers, event sweeps, and egress; ingress arrives
/// from the endpoint's demux task, which feeds quinn-proto directly and
/// [kicks](Inner::kick) the driver. The caller spawns the future and reclaims
/// the connection's bookkeeping once it resolves.
pub(crate) fn launch(
	handle: &Handle,
	socket: Rc<udp::Socket>,
	endpoint: Weak<endpoint::Inner>,
	key: ConnectionHandle,
	conn: quinn_proto::Connection,
) -> (Shared, impl Future<Output = ()> + use<>) {
	let shared = Rc::new(Inner {
		conn: RefCell::new(conn),
		state: RefCell::new(State::new()),
	});

	let mut driver = Driver {
		shared: shared.clone(),
		socket,
		endpoint,
		key,
		deadline: moq_net::runtime::Deadline::new(handle),
		scratch: Vec::with_capacity(TRAIN_SEGMENTS * SEGMENT),
		blocked: false,
	};
	let future = async move { kio::wait(|waiter| driver.poll(waiter)).await };
	(shared, future)
}

/// Wait out the handshake, yielding the connection's public handle.
pub(crate) async fn establish(shared: Shared) -> Result<Connection, Error> {
	kio::wait(|waiter| {
		let mut state = shared.state.borrow_mut();
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

	let alpn = {
		let conn = shared.conn.borrow();
		conn.crypto_session()
			.handshake_data()
			.and_then(|data| data.downcast::<quinn_proto::crypto::rustls::HandshakeData>().ok())
			.and_then(|data| data.protocol)
			.map(|proto| String::from_utf8_lossy(&proto).into_owned())
	};

	Ok(Connection {
		shared,
		park: kio::Park::default(),
		alpn,
	})
}

impl web_transport_trait::poll::Session for Connection {
	type SendStream = super::SendStream;
	type RecvStream = super::RecvStream;
	type Error = Error;

	fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::RecvStream, Self::Error>> {
		let waiter = self.park.hold(cx);
		if let Some(id) = self.shared.conn.borrow_mut().streams().accept(Dir::Uni) {
			return Poll::Ready(Ok(super::RecvStream::new(self.shared.clone(), id)));
		}
		let mut state = self.shared.state.borrow_mut();
		if let Some(err) = &state.closed {
			return Poll::Ready(Err(err.clone()));
		}
		waiter.register(&mut state.accept_uni_waiters);
		Poll::Pending
	}

	fn poll_accept_bi(
		&mut self,
		cx: &mut Context<'_>,
	) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
		let waiter = self.park.hold(cx);
		// The borrow ends before the handles are built: constructing a send
		// stream reaches back into the connection.
		let accepted = self.shared.conn.borrow_mut().streams().accept(Dir::Bi);
		if let Some(id) = accepted {
			return Poll::Ready(Ok((
				super::SendStream::new(self.shared.clone(), id),
				super::RecvStream::new(self.shared.clone(), id),
			)));
		}
		let mut state = self.shared.state.borrow_mut();
		if let Some(err) = &state.closed {
			return Poll::Ready(Err(err.clone()));
		}
		waiter.register(&mut state.accept_bi_waiters);
		Poll::Pending
	}

	fn poll_open_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::SendStream, Self::Error>> {
		let waiter = self.park.hold(cx);
		if let Some(err) = self.shared.closed() {
			return Poll::Ready(Err(err));
		}
		// The peer's MAX_STREAMS credit is what gates us: `open` hands back
		// nothing while it is spent.
		let opened = self.shared.conn.borrow_mut().streams().open(Dir::Uni);
		match opened {
			Some(id) => Poll::Ready(Ok(super::SendStream::new(self.shared.clone(), id))),
			None => {
				let mut state = self.shared.state.borrow_mut();
				waiter.register(&mut state.open_waiters);
				Poll::Pending
			}
		}
	}

	fn poll_open_bi(
		&mut self,
		cx: &mut Context<'_>,
	) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
		let waiter = self.park.hold(cx);
		if let Some(err) = self.shared.closed() {
			return Poll::Ready(Err(err));
		}
		let opened = self.shared.conn.borrow_mut().streams().open(Dir::Bi);
		match opened {
			Some(id) => Poll::Ready(Ok((
				super::SendStream::new(self.shared.clone(), id),
				super::RecvStream::new(self.shared.clone(), id),
			))),
			None => {
				let mut state = self.shared.state.borrow_mut();
				waiter.register(&mut state.open_waiters);
				Poll::Pending
			}
		}
	}

	fn poll_send_datagram(&mut self, cx: &mut Context<'_>, payload: &[u8]) -> Poll<Result<(), Self::Error>> {
		let waiter = self.park.hold(cx);
		if let Some(err) = self.shared.closed() {
			return Poll::Ready(Err(err));
		}
		let payload = Bytes::copy_from_slice(payload);
		match self.shared.conn.borrow_mut().datagrams().send(payload, false) {
			Ok(()) => {
				self.shared.kick();
				Poll::Ready(Ok(()))
			}
			// The send queue is full; a flush frees space.
			Err(quinn_proto::SendDatagramError::Blocked(_)) => {
				let mut state = self.shared.state.borrow_mut();
				waiter.register(&mut state.datagram_send_waiters);
				Poll::Pending
			}
			Err(err) => Poll::Ready(Err(Error::Quic(err.to_string()))),
		}
	}

	fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>> {
		let waiter = self.park.hold(cx);
		if let Some(datagram) = self.shared.conn.borrow_mut().datagrams().recv() {
			return Poll::Ready(Ok(datagram));
		}
		let mut state = self.shared.state.borrow_mut();
		if let Some(err) = &state.closed {
			return Poll::Ready(Err(err.clone()));
		}
		waiter.register(&mut state.datagram_recv_waiters);
		Poll::Pending
	}

	fn max_datagram_size(&self) -> usize {
		self.shared.conn.borrow_mut().datagrams().max_size().unwrap_or(0)
	}

	fn protocol(&self) -> Option<&str> {
		self.alpn.as_deref()
	}

	fn close(&mut self, code: u32, reason: &str) {
		self.shared.close_code(u64::from(code), reason);
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Self::Error> {
		let waiter = self.park.hold(cx);
		let mut state = self.shared.state.borrow_mut();
		if let Some(err) = &state.closed {
			return Poll::Ready(err.clone());
		}
		waiter.register(&mut state.closed_waiters);
		Poll::Pending
	}

	fn stats(&self) -> impl web_transport_trait::Stats {
		let stats = self.shared.conn.borrow().stats();
		Stats {
			bytes_sent: stats.udp_tx.bytes,
			bytes_received: stats.udp_rx.bytes,
			bytes_lost: stats.path.lost_bytes,
			packets_sent: stats.udp_tx.datagrams,
			packets_received: stats.udp_rx.datagrams,
			packets_lost: stats.path.lost_packets,
			rtt: stats.path.rtt,
		}
	}
}

impl std::fmt::Debug for Connection {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Connection").field("alpn", &self.alpn).finish()
	}
}

/// A snapshot of quinn-proto's counters in [`web_transport_trait::Stats`]
/// shape.
struct Stats {
	bytes_sent: u64,
	bytes_received: u64,
	bytes_lost: u64,
	packets_sent: u64,
	packets_received: u64,
	packets_lost: u64,
	rtt: std::time::Duration,
}

impl web_transport_trait::Stats for Stats {
	fn bytes_sent(&self) -> Option<u64> {
		Some(self.bytes_sent)
	}

	fn bytes_received(&self) -> Option<u64> {
		Some(self.bytes_received)
	}

	fn bytes_lost(&self) -> Option<u64> {
		Some(self.bytes_lost)
	}

	fn packets_sent(&self) -> Option<u64> {
		Some(self.packets_sent)
	}

	fn packets_received(&self) -> Option<u64> {
		Some(self.packets_received)
	}

	fn packets_lost(&self) -> Option<u64> {
		Some(self.packets_lost)
	}

	fn rtt(&self) -> Option<std::time::Duration> {
		Some(self.rtt)
	}

	/// Nothing, because quinn-proto exposes no delivery or pacing rate.
	///
	/// The congestion window over the RTT is not a stand-in for one: BBR
	/// deliberately holds about twice the bandwidth-delay product (nearly
	/// three times it while starting up), so that number is a multiple of
	/// what the path will carry, and moq-net feeds this straight into its
	/// bandwidth allocator and PROBE. A missing sample is a state the model
	/// already handles (`stats.estimated_send_rate` is an `Option`, and no
	/// bandwidth producer is created without one); an inflated one is a rate
	/// an encoder will chase.
	fn estimated_send_rate(&self) -> Option<u64> {
		None
	}
}

/// The per-connection task: endpoint events, application events, timers, and
/// packets out. Packets in come from the endpoint's demux task, which feeds
/// quinn-proto directly and kicks this driver.
struct Driver {
	shared: Shared,
	socket: Rc<udp::Socket>,
	/// The endpoint this connection belongs to, for the events the two of
	/// them trade (fresh connection ids, retirements, and the drain that
	/// frees the slot). Weak, because the endpoint owns us.
	endpoint: Weak<endpoint::Inner>,
	key: ConnectionHandle,
	deadline: moq_net::runtime::Deadline<Handle>,
	/// Egress staging: quinn-proto writes into a `Vec`, so a train is built
	/// here and copied into the socket's registered buffer.
	scratch: Vec<u8>,
	/// The last flush found the transmit pool drained, so nothing it owed the
	/// peer has reached the wire yet.
	blocked: bool,
}

impl Driver {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		{
			let mut state = self.shared.state.borrow_mut();
			// The endpoint fails the connection from outside when its socket
			// dies; there is nothing left to drain it through.
			if state.dead {
				return Poll::Ready(());
			}
			// Register the kick first: a handle mutating the connection after
			// this turn's sweeps still re-polls us.
			waiter.register(&mut state.driver);
		}

		loop {
			self.endpoint_events();
			self.sweep();

			if self.shared.state.borrow().dead {
				return Poll::Ready(());
			}
			// A closed connection still owes the peer its CONNECTION_CLOSE
			// (and a retransmit for each packet that arrives after), so the
			// driver runs until quinn says the drain is over.
			if self.shared.conn.borrow().is_drained() {
				return Poll::Ready(());
			}

			if let Poll::Ready(err) = self.flush(waiter) {
				self.shared.state.borrow_mut().fail(err);
				return Poll::Ready(());
			}
			self.publish_close();

			// Arm, *then* poll: the poll is what registers the waiter, so
			// polling before the set would leave the firing to wake nobody
			// (fatal on a dial nobody answers, where no ingress ever re-polls
			// us).
			self.deadline.set(self.shared.conn.borrow_mut().poll_timeout());
			if self.deadline.poll(waiter).is_pending() {
				return Poll::Pending;
			}
			self.shared.conn.borrow_mut().handle_timeout(Instant::now());
		}
	}

	/// Trade events with the endpoint: connection ids to issue and retire, and
	/// the drain that frees our slot in its table.
	///
	/// The endpoint's borrow is never held across the connection's, and the
	/// demux task borrows them in the same order, so the two cannot deadlock.
	fn endpoint_events(&mut self) {
		let Some(endpoint) = self.endpoint.upgrade() else {
			return;
		};
		loop {
			let event = self.shared.conn.borrow_mut().poll_endpoint_events();
			let Some(event) = event else {
				return;
			};
			if let Some(event) = endpoint.on_connection_event(self.key, event) {
				self.shared.conn.borrow_mut().handle_event(event);
			}
		}
	}

	/// Everything event-shaped: establishment, new and readable streams,
	/// writability, finished sends, received datagrams, and the end.
	fn sweep(&mut self) {
		loop {
			let event = self.shared.conn.borrow_mut().poll();
			let Some(event) = event else {
				return;
			};

			let mut state = self.shared.state.borrow_mut();
			match event {
				quinn_proto::Event::Connected => {
					state.established = true;
					state.establish_waiters.wake();
				}
				quinn_proto::Event::ConnectionLost { reason } => state.fail(reason.into()),
				quinn_proto::Event::DatagramReceived => state.datagram_recv_waiters.wake(),
				quinn_proto::Event::DatagramsUnblocked => state.datagram_send_waiters.wake(),
				quinn_proto::Event::HandshakeDataReady => {}
				quinn_proto::Event::Stream(event) => sweep_stream(&mut state, event),
			}
		}
	}

	/// Publish the terminal error for a close this side asked for.
	///
	/// quinn raises no event for it, so the driver is what reports it, and
	/// only once the flush above has staged the CONNECTION_CLOSE, since an
	/// application is free to stop driving the worker the moment
	/// `poll_closed` resolves. Staged is not delivered: the send is
	/// fire-and-forget, so a worker torn down in the same breath can still
	/// take the packet with it and leave the peer to idle out.
	fn publish_close(&mut self) {
		if self.blocked || !self.shared.conn.borrow().is_closed() {
			return;
		}
		let mut state = self.shared.state.borrow_mut();
		let Some((code, reason)) = state.local_close.take() else {
			return;
		};
		state.fail(Error::App { code, reason });
	}

	/// Stage one GSO train, then yield so another connection sharing the
	/// socket gets a chance at the transmit pool.
	fn flush(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		match self.flush_one(waiter) {
			Poll::Ready(Ok(())) => {}
			Poll::Ready(Err(err)) => return Poll::Ready(err),
			// Backpressure, or nothing left to stage.
			Poll::Pending => return Poll::Pending,
		}
		// Requeue behind the other ready tasks. If quinn is drained, the next
		// poll costs one empty acquire and then parks normally.
		waiter.waker().wake_by_ref();
		Poll::Pending
	}

	/// Fill one transmit buffer and stage it. Ignores quinn's pacing hint;
	/// the congestion controller still bounds each train.
	fn flush_one(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let mut tx = match self.socket.poll_acquire(waiter) {
			Poll::Ready(Ok(tx)) => tx,
			Poll::Ready(Err(err)) => return Poll::Ready(Err(Error::Io(err.to_string()))),
			// Backpressure: a completed send re-polls us.
			Poll::Pending => {
				self.blocked = true;
				return Poll::Pending;
			}
		};
		self.blocked = false;

		let segments = (tx.len() / SEGMENT).min(TRAIN_SEGMENTS);
		if segments == 0 {
			return Poll::Ready(Err(Error::Io(format!(
				"transmit buffer of {} bytes holds no {SEGMENT} byte segment",
				tx.len()
			))));
		}

		self.scratch.clear();
		let transmit = match self
			.shared
			.conn
			.borrow_mut()
			.poll_transmit(Instant::now(), segments, &mut self.scratch)
		{
			Some(transmit) => transmit,
			// Nothing to send; the buffer returns to the pool on drop.
			None => return Poll::Pending,
		};

		tx[..transmit.size].copy_from_slice(&self.scratch[..transmit.size]);
		// A lone datagram is its own segment size, and the socket's GSO
		// stride has to match what quinn actually packed.
		let segment = transmit.segment_size.unwrap_or(transmit.size);
		if let Err(err) = tx.send(transmit.size, transmit.destination, segment) {
			return Poll::Ready(Err(Error::Io(err.to_string())));
		}
		// A flush frees datagram-send queue space.
		self.shared.state.borrow_mut().datagram_send_waiters.wake();
		Poll::Ready(Ok(()))
	}
}

/// How a send stream ended, once the driver has seen it happen.
#[derive(Clone, Copy, Debug)]
pub(crate) enum End {
	/// The peer acknowledged the FIN.
	Delivered,
	/// The peer sent `STOP_SENDING` with this code.
	Stopped(u64),
}

/// Record how a send stream ended, and wake whoever is watching it.
///
/// A stream whose handle has already dropped records nothing: nobody is left
/// to read the verdict, and the entry would outlive every use for it.
fn end(state: &mut State, id: StreamId, end: End) {
	if let Some(slot) = state.sends.get_mut(&id) {
		*slot = Some(end);
	}
	if let Some(mut waiters) = state.finishing.remove(&id) {
		waiters.wake();
	}
}

/// Apply one stream event to the parking tables.
fn sweep_stream(state: &mut State, event: quinn_proto::StreamEvent) {
	// Waking removes the entry: a still-interested poller re-registers on its
	// next poll, so the maps only hold streams somebody is parked on.
	match event {
		quinn_proto::StreamEvent::Opened { dir: Dir::Bi } => state.accept_bi_waiters.wake(),
		quinn_proto::StreamEvent::Opened { dir: Dir::Uni } => state.accept_uni_waiters.wake(),
		quinn_proto::StreamEvent::Available { .. } => state.open_waiters.wake(),
		quinn_proto::StreamEvent::Readable { id } => {
			if let Some(mut waiters) = state.readable.remove(&id) {
				waiters.wake();
			}
		}
		quinn_proto::StreamEvent::Writable { id } => {
			if let Some(mut waiters) = state.writable.remove(&id) {
				waiters.wake();
			}
		}
		quinn_proto::StreamEvent::Finished { id } => end(state, id, End::Delivered),
		quinn_proto::StreamEvent::Stopped { id, error_code } => {
			end(state, id, End::Stopped(error_code.into_inner()));
			// A writer blocked on capacity has to learn it will never come.
			if let Some(mut waiters) = state.writable.remove(&id) {
				waiters.wake();
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use quinn_proto::Side;

	use super::*;

	/// A dropped handle takes its parking with it.
	///
	/// Nothing else can: a stream reset or stopped on the way out is never
	/// reported writable or readable again, so an entry left behind sits
	/// there for the life of the connection, one per cancelled stream.
	#[test]
	fn forgetting_a_handle_clears_its_parking() {
		let mut state = State::new();
		let id = StreamId::new(Side::Client, Dir::Bi, 0);
		state.writable.entry(id).or_default();
		state.readable.entry(id).or_default();
		state.finishing.entry(id).or_default();
		state.sends.insert(id, None);

		state.forget_send(id);
		assert!(state.writable.is_empty(), "the write half's parking");
		assert!(state.finishing.is_empty(), "the finish watch");
		assert!(state.sends.is_empty(), "the end bookkeeping");
		// The read half is a separate handle, which may still be parked.
		assert!(!state.readable.is_empty(), "the read half's parking survives");

		state.forget_recv(id);
		assert!(state.readable.is_empty(), "the read half's parking");
	}
}
