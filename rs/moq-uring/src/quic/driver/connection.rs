//! The connection: shared state, the driver task, and the session handle.

use moq_clock::Instant;
use std::cell::RefCell;
use std::net::SocketAddr;
use std::rc::{Rc, Weak};
use std::task::{Context, Poll};

use bytes::{Buf, Bytes};
use moq_quic::endpoint::ConnectionHandle;
use moq_quic::stream::{Dir, Id as StreamId};
use rustc_hash::FxHashMap;

use super::super::{Error, VarInt};
use super::{endpoint, recv, send};
use crate::udp;
use crate::worker::Owner;

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
	pub(crate) conn: RefCell<moq_quic::connection::Connection>,
	/// Updated by packet ingress and egress, so readers never wake the worker to sample.
	pub(crate) wire: moq_net::transport::Wire,
	/// The session's probe target, read before every egress, or `None` when the congestion
	/// controller cannot probe
	pub(crate) probe: Option<moq_net::transport::Probe>,
	pub(crate) state: RefCell<State>,
	/// The worker driving this connection, which anything layered on it
	/// (the WebTransport handshake, say) runs on too.
	pub(crate) owner: Owner,
}

pub(crate) struct State {
	pub(crate) active: Option<moq_net::server::Active>,
	/// Wakes the driver; handles kick it after mutating the connection so
	/// fresh egress reaches the wire.
	driver: kio::WaiterList,

	established: bool,
	establish_waiters: kio::WaiterList,

	/// Whoever is waiting for a peer-initiated stream. moq-quic hands the
	/// ids out in order, so the queue is its own.
	accept_bi_waiters: kio::WaiterList,
	accept_uni_waiters: kio::WaiterList,

	/// Whoever is blocked on the peer's MAX_STREAMS credit.
	open_waiters: kio::WaiterList,

	/// Per-stream read/write parking, keyed by stream id.
	///
	/// FxHash rather than SipHash on all four of these: they sit on the
	/// driver's per-event path, and moq-quic assigns the ids.
	readable: FxHashMap<StreamId, kio::WaiterList>,
	writable: FxHashMap<StreamId, kio::WaiterList>,
	/// Send streams waiting for their end: a FIN the peer acknowledged, or a
	/// `STOP_SENDING`.
	finishing: FxHashMap<StreamId, kio::WaiterList>,

	datagram_recv_waiters: kio::WaiterList,

	/// The terminal error, set exactly once; everything fails with it after.
	closed: Option<Error>,
	closed_waiters: kio::WaiterList,

	/// The socket died under us: the driver stops where it stands rather than
	/// waiting out a drain it could never transmit.
	dead: bool,

	/// The close this side asked for, as moq-quic reported it, until the driver
	/// has put it on the wire.
	local_close: Option<Error>,
}

impl State {
	fn new() -> Self {
		Self {
			active: None,
			driver: kio::WaiterList::new(),
			established: false,
			establish_waiters: kio::WaiterList::new(),
			accept_bi_waiters: kio::WaiterList::new(),
			accept_uni_waiters: kio::WaiterList::new(),
			open_waiters: kio::WaiterList::new(),
			readable: FxHashMap::default(),
			writable: FxHashMap::default(),
			finishing: FxHashMap::default(),
			datagram_recv_waiters: kio::WaiterList::new(),
			closed: None,
			closed_waiters: kio::WaiterList::new(),
			dead: false,
			local_close: None,
		}
	}

	/// Drop send stream `id`'s bookkeeping, parking included.
	fn forget_send(&mut self, id: StreamId) {
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
		self.active.take();
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

	/// Close the connection with a full-width application code.
	///
	/// The [`web_transport_trait::poll::Session::close`] surface narrows codes
	/// to `u32`; the WebTransport layer maps its codes into HTTP/3's error
	/// space, which needs the whole varint range.
	pub(crate) fn close_code(&self, code: u64, reason: &str) {
		self.conn.borrow_mut().close(
			self.owner.now(),
			VarInt::from_u64(code).unwrap_or(VarInt::MAX),
			reason.to_string(),
		);
		self.state.borrow_mut().driver.wake();
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
/// with [`close`](web_transport_trait::poll::Session::close), or wrap it in
/// [`crate::quic::web::Session`], whose last handle closes it.
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
	/// The SNI the client presented, cached likewise.
	server_name: Option<String>,
	/// The peer's address when the handshake completed.
	remote: SocketAddr,
	handshake: crate::quic::handshake::Info,
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

	/// The worker driving this connection.
	pub(crate) fn owner(&self) -> &Owner {
		&self.shared.owner
	}

	/// Open a send group at `priority`, for streams to join with
	/// [`send::Stream::set_group`] and datagrams with [`send::Options`].
	pub fn send_group(&self, priority: i64) -> send::Group {
		send::Group::new(self.shared.clone(), priority)
	}

	/// Send a datagram scheduled by `options`, like one of its group's streams at its priority,
	/// or among the ungrouped streams without a group.
	///
	/// Never blocks: a full buffer drops the oldest datagram of the lowest-priority group,
	/// see [`moq_quic::Datagrams::send`]. A [`Bytes`] payload, or a chain of them, is queued
	/// without copying.
	///
	/// # Panics
	/// - when `options.group` belongs to another connection
	pub fn send_datagram(&self, data: impl Buf, options: send::Options<'_>) -> Result<(), send::Error> {
		if let Some(err) = self.shared.closed() {
			return Err(send::Error::Connection(err));
		}
		let sent = self.shared.conn.borrow_mut().datagrams().send(data, options.to_quic());
		match sent {
			Ok(()) => {}
			Err(moq_quic::send::Error::TooLarge) => return Err(send::Error::TooLarge),
			Err(_) => return Err(send::Error::Unsupported),
		}
		self.shared.kick();
		Ok(())
	}

	/// The largest datagram [`Self::send_datagram`] takes, or `None` if datagrams are
	/// unsupported by the peer or disabled locally.
	pub fn max_datagram_size(&self) -> Option<usize> {
		self.shared.conn.borrow_mut().datagrams().max_size()
	}

	/// The negotiated ALPN protocol, if any.
	pub fn protocol(&self) -> Option<&str> {
		self.alpn.as_deref()
	}

	/// Close the connection immediately with `error_code` and `reason`.
	///
	/// Only records the close: the driver task frames the CONNECTION_CLOSE, so keep the worker
	/// running until [`Self::closed`] resolves.
	pub fn close(&self, error_code: VarInt, reason: &[u8]) {
		self.shared
			.close_code(error_code.into_inner(), &String::from_utf8_lossy(reason));
	}

	/// Wait for the connection to close, returning why.
	pub async fn closed(&self) -> Error {
		kio::wait(|waiter| poll_closed(&self.shared, waiter)).await
	}

	/// The connection's statistics.
	pub fn stats(&self) -> moq_quic::connection::Stats {
		self.shared.conn.borrow().stats()
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

	/// The peer's address as of the handshake; a peer that migrates later
	/// keeps its connection but not this value.
	pub fn remote_addr(&self) -> SocketAddr {
		self.remote
	}

	/// The SNI the client presented, or `None` if it sent none. Always `None`
	/// on a dialed connection.
	pub fn server_name(&self) -> Option<&str> {
		self.server_name.as_deref()
	}

	/// The handshake this connection went through, and when all of it must be
	/// done.
	pub fn handshake(&self) -> &crate::quic::handshake::Info {
		&self.handshake
	}

	/// Change the flow-control limits `limits` sets; a raise reaches the peer
	/// on the next packet.
	///
	/// Errors, changing nothing, on limits [`Limits::validate`](crate::quic::Limits::validate)
	/// refuses.
	pub fn set_limits(&self, limits: crate::quic::Limits) -> Result<(), Error> {
		self.shared.conn.borrow_mut().set_limits(limits)?;
		self.shared.kick();
		Ok(())
	}

	/// The wire counters this connection updates as it sends and receives.
	pub(crate) fn wire(&self) -> moq_net::transport::Wire {
		self.shared.wire.clone()
	}

	/// The target this connection probes up to, or `None` when its congestion controller cannot probe.
	pub(crate) fn probe(&self) -> Option<moq_net::transport::Probe> {
		self.shared.probe.clone()
	}

	/// Receive streams evicted to free a stuck receive window.
	pub(crate) fn streams_evicted(&self) -> u64 {
		self.shared.conn.borrow().stats().credit.evicted
	}
}

impl Clone for Connection {
	fn clone(&self) -> Self {
		Self {
			shared: self.shared.clone(),
			park: kio::Park::default(),
			alpn: self.alpn.clone(),
			server_name: self.server_name.clone(),
			remote: self.remote,
			handshake: self.handshake,
		}
	}
}

/// Build a connection's shared state and its driver.
///
/// The driver future does timers, event sweeps, and egress; ingress arrives
/// from the endpoint's demux task, which feeds moq-quic directly and
/// [kicks](Inner::kick) the driver. The caller spawns the future and reclaims
/// the connection's bookkeeping once it resolves.
pub(crate) fn launch(
	owner: &Owner,
	socket: Rc<udp::Socket>,
	endpoint: Weak<endpoint::Inner>,
	key: ConnectionHandle,
	conn: moq_quic::connection::Connection,
) -> (Shared, impl Future<Output = ()> + use<>) {
	// The handshake's first datagrams arrived before this driver existed.
	let stats = conn.stats();
	let wire = moq_net::transport::Wire::new();
	wire.set_sent_bytes(stats.udp_tx.bytes);
	wire.set_received_bytes(stats.udp_rx.bytes);
	let probe = conn.congestion_state().probes().then(moq_net::transport::Probe::new);
	let shared = Rc::new(Inner {
		wire,
		probe,
		conn: RefCell::new(conn),
		state: RefCell::new(State::new()),
		owner: owner.clone(),
	});

	let mut driver = Driver {
		shared: shared.clone(),
		socket,
		endpoint,
		key,
		deadline: owner.timer(),
		close_staged: false,
	};
	let future = async move { kio::wait(|waiter| driver.poll(waiter)).await };
	(shared, future)
}

/// Own a connection until its handshake is handed to a public handle.
///
/// The driver and endpoint keep their own references, so dropping a pending
/// establishment future without closing it leaves both alive until timeout.
struct EstablishGuard {
	shared: Option<Shared>,
}

impl Drop for EstablishGuard {
	fn drop(&mut self) {
		if let Some(shared) = self.shared.take() {
			shared.close_code(0, "QUIC handshake abandoned");
		}
	}
}

/// Wait out the handshake, yielding the connection's public handle, or close
/// the connection if it is still handshaking at `deadline`.
pub(crate) async fn establish(shared: Shared, deadline: Option<Instant>) -> Result<Connection, Error> {
	let mut guard = EstablishGuard {
		shared: Some(shared.clone()),
	};
	let mut timer = deadline.map(|at| {
		let mut timer = shared.owner.timer();
		timer.set(Some(at));
		timer
	});
	let mut expired = false;
	let result = kio::wait(|waiter| {
		// The deadline goes first, so a handshake that completed in the same
		// turn it expired is still refused. The close resolves this wait once
		// the driver has staged it.
		if timer.as_mut().is_some_and(|timer| timer.poll(waiter).is_ready()) {
			timer = None;
			expired = true;
			shared.close_code(0, "QUIC handshake timed out");
		}
		let mut state = shared.state.borrow_mut();
		if state.established && !expired {
			return Poll::Ready(Ok(()));
		}
		if let Some(err) = &state.closed {
			return Poll::Ready(Err(err.clone()));
		}
		waiter.register(&mut state.establish_waiters);
		Poll::Pending
	})
	.await;
	if result.is_err() {
		// The driver already reached a terminal state.
		guard.shared = None;
	}
	result?;

	let (alpn, server_name, remote) = {
		let conn = shared.conn.borrow();
		let handshake = conn
			.crypto_session()
			.handshake_data()
			.and_then(|data| data.downcast::<moq_quic::crypto::rustls::HandshakeData>().ok());
		let (alpn, server_name) = match handshake {
			Some(data) => (
				data.protocol.map(|proto| String::from_utf8_lossy(&proto).into_owned()),
				data.server_name,
			),
			None => (None, None),
		};
		let remote = conn.remote_address();
		(alpn, server_name, remote)
	};

	let conn = Connection {
		shared,
		park: kio::Park::default(),
		alpn,
		server_name,
		remote,
		handshake: crate::quic::handshake::Info { deadline },
	};
	guard.shared = None;
	Ok(conn)
}

impl Connection {
	pub(crate) fn open_request(&self) -> Result<Option<(send::Stream, recv::Stream)>, Error> {
		if let Some(err) = self.shared.closed() {
			return Err(err);
		}
		let id = self
			.shared
			.conn
			.borrow_mut()
			.streams()
			.open(Dir::Bi, Default::default());
		Ok(id.map(|id| {
			(
				send::Stream::new(self.shared.clone(), id),
				recv::Stream::new(self.shared.clone(), id),
			)
		}))
	}
}

impl Connection {
	/// Accept the next unidirectional stream the peer opens.
	pub async fn accept_uni(&self) -> Result<recv::Stream, Error> {
		kio::wait(|waiter| accept_uni(&self.shared, waiter)).await
	}

	/// Accept the next bidirectional stream the peer opens.
	pub async fn accept_bi(&self) -> Result<(send::Stream, recv::Stream), Error> {
		kio::wait(|waiter| accept_bi(&self.shared, waiter)).await
	}

	/// Open a unidirectional stream scheduled by `options`, once the peer's stream credit
	/// allows.
	///
	/// # Panics
	/// - when `options.group` belongs to another connection
	pub async fn open_uni(&self, options: send::Options<'_>) -> Result<send::Stream, Error> {
		kio::wait(|waiter| open(&self.shared, Dir::Uni, options, waiter))
			.await
			.map(|(send, _)| send)
	}

	/// Open a bidirectional stream scheduled by `options`, once the peer's stream credit
	/// allows.
	///
	/// # Panics
	/// - when `options.group` belongs to another connection
	pub async fn open_bi(&self, options: send::Options<'_>) -> Result<(send::Stream, recv::Stream), Error> {
		kio::wait(|waiter| open(&self.shared, Dir::Bi, options, waiter))
			.await
			.map(|(send, recv)| (send, recv.expect("bidirectional")))
	}

	/// Receive the next datagram.
	pub async fn read_datagram(&self) -> Result<Bytes, Error> {
		kio::wait(|waiter| recv_datagram(&self.shared, waiter)).await
	}

	/// [`Self::accept_uni`], as a poll.
	pub(crate) fn poll_accept_uni(&mut self, cx: &Context<'_>) -> Poll<Result<recv::Stream, Error>> {
		accept_uni(&self.shared, self.park.hold(cx))
	}

	/// [`Self::accept_bi`], as a poll.
	pub(crate) fn poll_accept_bi(&mut self, cx: &Context<'_>) -> Poll<Result<(send::Stream, recv::Stream), Error>> {
		accept_bi(&self.shared, self.park.hold(cx))
	}

	/// [`Self::open_uni`], as a poll.
	pub(crate) fn poll_open_uni(
		&mut self,
		cx: &Context<'_>,
		options: send::Options<'_>,
	) -> Poll<Result<send::Stream, Error>> {
		open(&self.shared, Dir::Uni, options, self.park.hold(cx)).map_ok(|(send, _)| send)
	}

	/// [`Self::open_bi`], as a poll.
	pub(crate) fn poll_open_bi(
		&mut self,
		cx: &Context<'_>,
		options: send::Options<'_>,
	) -> Poll<Result<(send::Stream, recv::Stream), Error>> {
		open(&self.shared, Dir::Bi, options, self.park.hold(cx))
			.map_ok(|(send, recv)| (send, recv.expect("bidirectional")))
	}

	/// [`Self::read_datagram`], as a poll.
	pub(crate) fn poll_recv_datagram(&mut self, cx: &Context<'_>) -> Poll<Result<Bytes, Error>> {
		recv_datagram(&self.shared, self.park.hold(cx))
	}

	/// [`Self::closed`], as a poll.
	pub(crate) fn poll_closed(&mut self, cx: &Context<'_>) -> Poll<Error> {
		poll_closed(&self.shared, self.park.hold(cx))
	}

	/// The connection's counters in [`web_transport_trait::Stats`] shape.
	pub(crate) fn transport_stats(&self) -> impl web_transport_trait::Stats + use<> {
		let stats = self.stats();
		Stats {
			bytes_sent: stats.udp_tx.bytes,
			bytes_received: stats.udp_rx.bytes,
			bytes_lost: stats.path.lost_bytes,
			packets_sent: stats.udp_tx.datagrams,
			packets_received: stats.udp_rx.datagrams,
			packets_lost: stats.path.lost_packets,
			rtt: stats.path.rtt,
			bandwidth: stats.path.bandwidth_estimate,
		}
	}
}

fn accept_uni(shared: &Shared, waiter: &kio::Waiter) -> Poll<Result<recv::Stream, Error>> {
	if let Some(id) = shared.conn.borrow_mut().streams().accept(Dir::Uni) {
		return Poll::Ready(Ok(recv::Stream::new(shared.clone(), id)));
	}
	let mut state = shared.state.borrow_mut();
	if let Some(err) = &state.closed {
		return Poll::Ready(Err(err.clone()));
	}
	waiter.register(&mut state.accept_uni_waiters);
	Poll::Pending
}

fn accept_bi(shared: &Shared, waiter: &kio::Waiter) -> Poll<Result<(send::Stream, recv::Stream), Error>> {
	let accepted = shared.conn.borrow_mut().streams().accept(Dir::Bi);
	if let Some(id) = accepted {
		return Poll::Ready(Ok((
			send::Stream::new(shared.clone(), id),
			recv::Stream::new(shared.clone(), id),
		)));
	}
	let mut state = shared.state.borrow_mut();
	if let Some(err) = &state.closed {
		return Poll::Ready(Err(err.clone()));
	}
	waiter.register(&mut state.accept_bi_waiters);
	Poll::Pending
}

/// Open a stream in `dir`, with its receive half when bidirectional.
fn open(
	shared: &Shared,
	dir: Dir,
	options: send::Options<'_>,
	waiter: &kio::Waiter,
) -> Poll<Result<(send::Stream, Option<recv::Stream>), Error>> {
	if let Some(err) = shared.closed() {
		return Poll::Ready(Err(err));
	}
	// The peer's MAX_STREAMS credit is what gates us: `open` hands back nothing while it is
	// spent.
	let opened = shared.conn.borrow_mut().streams().open(dir, options.to_quic());
	let Some(id) = opened else {
		waiter.register(&mut shared.state.borrow_mut().open_waiters);
		return Poll::Pending;
	};
	let recv = (dir == Dir::Bi).then(|| recv::Stream::new(shared.clone(), id));
	Poll::Ready(Ok((send::Stream::new(shared.clone(), id), recv)))
}

fn recv_datagram(shared: &Shared, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>> {
	if let Some(datagram) = shared.conn.borrow_mut().datagrams().recv() {
		return Poll::Ready(Ok(datagram));
	}
	let mut state = shared.state.borrow_mut();
	if let Some(err) = &state.closed {
		return Poll::Ready(Err(err.clone()));
	}
	waiter.register(&mut state.datagram_recv_waiters);
	Poll::Pending
}

fn poll_closed(shared: &Shared, waiter: &kio::Waiter) -> Poll<Error> {
	let mut state = shared.state.borrow_mut();
	if let Some(err) = &state.closed {
		return Poll::Ready(err.clone());
	}
	waiter.register(&mut state.closed_waiters);
	Poll::Pending
}

impl std::fmt::Debug for Connection {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Connection").field("alpn", &self.alpn).finish()
	}
}

/// A snapshot of moq-quic's counters in [`web_transport_trait::Stats`]
/// shape.
struct Stats {
	bytes_sent: u64,
	bytes_received: u64,
	bytes_lost: u64,
	packets_sent: u64,
	packets_received: u64,
	packets_lost: u64,
	rtt: std::time::Duration,
	/// The congestion controller's bandwidth estimate in bytes per second.
	bandwidth: Option<u64>,
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

	/// The congestion controller's bandwidth estimate, in bits per second.
	///
	/// BBR estimates the delivery rate; CUBIC offers none. The congestion
	/// window over the RTT is not a stand-in: BBR deliberately holds about
	/// twice the bandwidth-delay product, so that number is a multiple of what
	/// the path will carry, and moq-net feeds this straight into its bandwidth
	/// allocator and PROBE. A missing sample is a state the model already
	/// handles; an inflated one is a rate an encoder will chase.
	fn estimated_send_rate(&self) -> Option<u64> {
		self.bandwidth.map(|bytes| bytes.saturating_mul(8))
	}
}

/// The per-connection task: endpoint events, application events, timers, and
/// packets out. Packets in come from the endpoint's demux task, which feeds
/// moq-quic directly and kicks this driver.
struct Driver {
	shared: Shared,
	socket: Rc<udp::Socket>,
	/// The endpoint this connection belongs to, for the events the two of
	/// them trade (fresh connection ids, retirements, and the drain that
	/// frees the slot). Weak, because the endpoint owns us.
	endpoint: Weak<endpoint::Inner>,
	key: ConnectionHandle,
	deadline: crate::Timer,
	/// A CONNECTION_CLOSE has been handed to the socket. Not implied by a
	/// flush that came back empty: moq-quic paces the close like any other packet,
	/// so it can sit behind a pacing timer after the application asked for it.
	close_staged: bool,
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
			// driver runs until moq-quic says the drain is over.
			if self.shared.conn.borrow().is_drained() {
				// A close moq-quic never managed to send (the server's
				// anti-amplification limit can withhold it) ends here too.
				self.publish_close();
				return Poll::Ready(());
			}

			if let Poll::Ready(err) = self.flush(waiter) {
				self.shared.state.borrow_mut().fail(err);
				return Poll::Ready(());
			}
			if self.close_staged {
				self.publish_close();
			}

			// Arm, *then* poll: the poll is what registers the waiter, so
			// polling before the set would leave the firing to wake nobody
			// (fatal on a dial nobody answers, where no ingress ever re-polls
			// us).
			self.deadline.set(self.shared.conn.borrow_mut().poll_timeout());
			if self.deadline.poll(waiter).is_pending() {
				return Poll::Pending;
			}
			self.shared.conn.borrow_mut().handle_timeout(self.shared.owner.now());
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
				moq_quic::Event::Connected => {
					state.established = true;
					state.establish_waiters.wake();
				}
				// The driver publishes a local close once it has staged the
				// CONNECTION_CLOSE, so a caller that stops driving the worker the
				// moment `poll_closed` resolves has at least handed the packet over.
				moq_quic::Event::ConnectionLost {
					reason: reason @ moq_quic::connection::Error::LocallyClosed { .. },
				} => state.local_close = Some(reason.into()),
				moq_quic::Event::ConnectionLost { reason } => state.fail(reason.into()),
				moq_quic::Event::DatagramReceived => state.datagram_recv_waiters.wake(),
				moq_quic::Event::HandshakeDataReady => {}
				moq_quic::Event::Stream(event) => sweep_stream(&mut state, event),
				// Non-exhaustive upstream; nothing else needs a wakeup.
				_ => {}
			}
		}
	}

	/// Publish the terminal error for a close this side asked for.
	///
	/// Only once the CONNECTION_CLOSE is staged (or the drain is over), since
	/// an application is free to stop driving the worker the moment
	/// `poll_closed` resolves. Staged is not delivered: the send is
	/// fire-and-forget, so a worker torn down in the same breath can still
	/// take the packet with it and leave the peer to idle out.
	fn publish_close(&mut self) {
		let mut state = self.shared.state.borrow_mut();
		if let Some(err) = state.local_close.take() {
			state.fail(err);
		}
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
		// Requeue behind the other ready tasks. If moq-quic is drained, the next
		// poll costs one empty acquire and then parks normally.
		waiter.waker().wake_by_ref();
		Poll::Pending
	}

	/// Fill one transmit buffer and stage it.
	///
	/// Pacing happens inside `poll_transmit`: a paced path returns nothing
	/// and arms a timer that `poll_timeout` reports, and the driver loop
	/// flushes again once it fires.
	fn flush_one(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let mut tx = match self.socket.poll_acquire(waiter) {
			Poll::Ready(Ok(tx)) => tx,
			Poll::Ready(Err(err)) => return Poll::Ready(Err(Error::Io(err.to_string()))),
			// Backpressure: a completed send re-polls us.
			Poll::Pending => return Poll::Pending,
		};

		let (transmit, closing) = {
			let mut conn = self.shared.conn.borrow_mut();
			// moq-quic packs every full datagram at the current MTU, which
			// discovery can raise, so a train sized for any smaller one would
			// outgrow the buffer.
			let mtu = usize::from(conn.current_mtu());
			let segments = (tx.capacity() / mtu).min(TRAIN_SEGMENTS);
			if segments == 0 {
				return Poll::Ready(Err(Error::Io(format!(
					"transmit buffer of {} bytes holds no {mtu} byte datagram",
					tx.capacity()
				))));
			}
			// A new target waits for the next egress: probing only resends data in flight, so it
			// has nothing to send until a write or an ACK kicks the driver anyway.
			if let Some(probe) = &self.shared.probe {
				conn.set_probe_target(probe.target().as_bps() / 8);
			}
			// moq-quic encrypts the train straight into the socket's buffer.
			// Nothing to send, or paced; the buffer returns to the pool on drop.
			let Some(transmit) = conn.poll_transmit(self.shared.owner.now(), segments, &mut tx) else {
				return Poll::Pending;
			};
			self.shared.wire.set_sent_bytes(conn.stats().udp_tx.bytes);
			// Once closed, moq-quic transmits nothing but the CONNECTION_CLOSE.
			(transmit, conn.is_closed())
		};
		debug_assert_eq!(transmit.size, tx.len());

		// A lone datagram is its own segment size, and the socket's GSO
		// stride has to match what moq-quic actually packed.
		let transmit = udp::Transmit {
			to: transmit.remote,
			segment: transmit.segment_size.unwrap_or(transmit.size),
			ecn: transmit.ecn.map(super::ecn_from_quic),
			src_ip: transmit.local_ip,
		};
		if let Err(err) = tx.send(transmit) {
			return Poll::Ready(Err(Error::Io(err.to_string())));
		}
		self.close_staged |= closing;
		Poll::Ready(Ok(()))
	}
}

/// Wake whoever waits for send stream `id` to end.
fn ended(state: &mut State, id: StreamId) {
	if let Some(mut waiters) = state.finishing.remove(&id) {
		waiters.wake();
	}
}

/// Apply one stream event to the parking tables.
fn sweep_stream(state: &mut State, event: moq_quic::StreamEvent) {
	// Waking removes the entry: a still-interested poller re-registers on its
	// next poll, so the maps only hold streams somebody is parked on.
	match event {
		moq_quic::StreamEvent::Opened { dir: Dir::Bi } => state.accept_bi_waiters.wake(),
		moq_quic::StreamEvent::Opened { dir: Dir::Uni } => state.accept_uni_waiters.wake(),
		moq_quic::StreamEvent::Available { .. } => state.open_waiters.wake(),
		moq_quic::StreamEvent::Readable { id } => {
			if let Some(mut waiters) = state.readable.remove(&id) {
				waiters.wake();
			}
		}
		moq_quic::StreamEvent::Writable { id } => {
			if let Some(mut waiters) = state.writable.remove(&id) {
				waiters.wake();
			}
		}
		moq_quic::StreamEvent::Finished { id } => ended(state, id),
		moq_quic::StreamEvent::Stopped { id, .. } => {
			ended(state, id);
			// A writer blocked on capacity has to learn it will never come.
			if let Some(mut waiters) = state.writable.remove(&id) {
				waiters.wake();
			}
		}
		// Nothing waits on a send stream's acknowledgment yet.
		moq_quic::StreamEvent::Acked { .. } => {}
	}
}

#[cfg(test)]
mod tests {
	use moq_quic::Side;

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

		state.forget_send(id);
		assert!(state.writable.is_empty(), "the write half's parking");
		assert!(state.finishing.is_empty(), "the finish watch");
		// The read half is a separate handle, which may still be parked.
		assert!(!state.readable.is_empty(), "the read half's parking survives");

		state.forget_recv(id);
		assert!(state.readable.is_empty(), "the read half's parking");
	}
}
