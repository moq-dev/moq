//! The connection: shared state, the driver task, and the session handle.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::rc::Rc;
use std::task::{Context, Poll};

use bytes::Bytes;

use super::{Error, SEGMENT};
use crate::{Handle, udp};

/// The state shared by every handle and the driver, single-threaded behind
/// `Rc<RefCell>`.
pub(crate) type Shared = Rc<Inner>;

/// One GSO train is at most 64 segments; stay a hair under the kernel cap.
const TRAIN_SEGMENTS: usize = 63;
/// How many received datagrams to hold for the application before dropping
/// the oldest.
const DGRAM_QUEUE: usize = 64;
/// The urgency a locally opened stream starts with, until the application
/// sets one: quiche's own default, the middle of the range.
const DEFAULT_URGENCY: u8 = 127;

/// Everything the handles and the driver share, single-threaded.
///
/// The connection and the bookkeeping live in separate `RefCell`s so a stream
/// operation can mutate the connection, drop that borrow, and then register a
/// waiter without ever holding both.
pub(crate) struct Inner {
	pub(crate) conn: RefCell<quiche::Connection>,
	pub(crate) state: RefCell<State>,
}

pub(crate) struct State {
	/// Wakes the driver; handles kick it after mutating the connection so
	/// fresh egress reaches the wire.
	driver: kio::WaiterList,

	/// The endpoint fed us packets since the driver's last sweep.
	ingested: bool,

	established: bool,
	establish_waiters: kio::WaiterList,

	/// Peer-initiated streams not yet handed out, and the next ids expected.
	/// QUIC creates every lower-numbered stream of a type implicitly, so a
	/// readable id queues everything up to it.
	accept_bi: VecDeque<u64>,
	accept_uni: VecDeque<u64>,
	next_accept_bi: u64,
	next_accept_uni: u64,
	accept_bi_waiters: kio::WaiterList,
	accept_uni_waiters: kio::WaiterList,

	/// The next locally initiated ids, and whoever is blocked on the peer's
	/// MAX_STREAMS credit.
	next_open_bi: u64,
	next_open_uni: u64,
	open_waiters: kio::WaiterList,

	/// Per-stream read/write parking, keyed by stream id.
	readable: HashMap<u64, kio::WaiterList>,
	writable: HashMap<u64, kio::WaiterList>,
	/// Send streams waiting for their end (FIN acknowledged, a STOP, or a
	/// reset); the driver probes these each turn since quiche has no event
	/// for stream collection.
	finishing: HashMap<u64, kio::WaiterList>,
	/// How each send stream ended, for the ones the driver saw collected while
	/// the connection was still up: `None` for a FIN the peer acknowledged,
	/// `Some(code)` for a `STOP_SENDING` the probe consumed on our behalf.
	/// That is what makes a later close mean "already delivered" rather than
	/// "we never found out". Cleared when the stream drops.
	pub(crate) collected: HashMap<u64, Option<u64>>,

	/// Received datagrams, oldest first; over [`DGRAM_QUEUE`] the oldest drop.
	datagrams: VecDeque<Bytes>,
	datagram_recv_waiters: kio::WaiterList,
	datagram_send_waiters: kio::WaiterList,

	/// The terminal error, set exactly once; everything fails with it after.
	closed: Option<Error>,
	closed_waiters: kio::WaiterList,
}

impl State {
	fn new(is_server: bool) -> Self {
		// Stream ids: bit 0 is the initiator (0 = client), bit 1 the
		// directionality (set = uni).
		let (next_accept_bi, next_accept_uni, next_open_bi, next_open_uni) = if is_server {
			(0b00, 0b10, 0b01, 0b11)
		} else {
			(0b01, 0b11, 0b00, 0b10)
		};
		Self {
			driver: kio::WaiterList::new(),
			ingested: false,
			established: false,
			establish_waiters: kio::WaiterList::new(),
			accept_bi: VecDeque::new(),
			accept_uni: VecDeque::new(),
			next_accept_bi,
			next_accept_uni,
			accept_bi_waiters: kio::WaiterList::new(),
			accept_uni_waiters: kio::WaiterList::new(),
			next_open_bi,
			next_open_uni,
			open_waiters: kio::WaiterList::new(),
			readable: HashMap::new(),
			writable: HashMap::new(),
			finishing: HashMap::new(),
			collected: HashMap::new(),
			datagrams: VecDeque::new(),
			datagram_recv_waiters: kio::WaiterList::new(),
			datagram_send_waiters: kio::WaiterList::new(),
			closed: None,
			closed_waiters: kio::WaiterList::new(),
		}
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

	/// The endpoint fed packets into the connection; the driver's next sweep
	/// treats them as ingress.
	pub(crate) fn mark_ingested(&self) {
		self.state.borrow_mut().ingested = true;
	}

	/// Terminate from outside (the endpoint's socket died): everything fails
	/// with `err`, and the driver exits on its next poll.
	pub(crate) fn fail(&self, err: Error) {
		self.state.borrow_mut().fail(err);
	}

	/// Park `waiter` until stream `id` is writable again.
	pub(crate) fn park_writable(&self, id: u64, waiter: &kio::Waiter) {
		let mut state = self.state.borrow_mut();
		waiter.register(state.writable.entry(id).or_default());
	}

	/// Park `waiter` until stream `id` is readable again.
	pub(crate) fn park_readable(&self, id: u64, waiter: &kio::Waiter) {
		let mut state = self.state.borrow_mut();
		waiter.register(state.readable.entry(id).or_default());
	}

	/// Park `waiter` until send stream `id` reaches its end.
	pub(crate) fn park_finishing(&self, id: u64, waiter: &kio::Waiter) {
		let mut state = self.state.borrow_mut();
		waiter.register(state.finishing.entry(id).or_default());
	}

	/// How send stream `id` ended, if the driver saw it collected on a live
	/// connection: `Some(None)` for an acknowledged FIN, `Some(Some(code))`
	/// for a `STOP_SENDING`.
	pub(crate) fn collected(&self, id: u64) -> Option<Option<u64>> {
		self.state.borrow().collected.get(&id).copied()
	}

	/// Forget stream `id`'s bookkeeping; called when a handle drops.
	pub(crate) fn forget(&self, id: u64) {
		let mut state = self.state.borrow_mut();
		state.collected.remove(&id);
		state.finishing.remove(&id);
	}

	/// Wake anyone parked on stream `id` being readable.
	pub(crate) fn wake_readable(&self, id: u64) {
		let mut state = self.state.borrow_mut();
		if let Some(mut waiters) = state.readable.remove(&id) {
			waiters.wake();
		}
	}
}

/// A QUIC connection driven by a [`crate::Worker`], usable as a MoQ transport.
///
/// Created by [`Endpoint`](super::Endpoint) (or its
/// [`client::connect`](super::client::connect) /
/// [`server::accept`](super::server::accept) shorthands), already
/// established. Clones share the connection; each carries its own parking so
/// concurrent pending operations don't trample each other's wakeups. Dropping
/// every handle (and every stream) drops the driver's `Rc` peers, but the
/// driver itself keeps the connection alive until it ends; close explicitly
/// with [`close`](web_transport_trait::poll::Session::close) (which moq's
/// session machine does).
pub struct Connection {
	shared: Shared,
	// Retains this clone's waiter registrations across polls.
	park: kio::Park,
	/// The negotiated ALPN, cached at establishment so `protocol()` can
	/// borrow from the handle.
	alpn: Option<String>,
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
/// from the endpoint's demux task, which feeds quiche directly and
/// [kicks](Inner::kick) the driver. The caller spawns the future and reclaims
/// the connection's routes once it resolves.
pub(crate) fn launch(
	handle: &Handle,
	socket: Rc<udp::Socket>,
	conn: quiche::Connection,
) -> (Shared, impl Future<Output = ()> + use<>) {
	let is_server = conn.is_server();
	let shared = Rc::new(Inner {
		conn: RefCell::new(conn),
		state: RefCell::new(State::new(is_server)),
	});

	let mut driver = Driver {
		shared: shared.clone(),
		socket,
		deadline: moq_net::runtime::Deadline::new(handle),
		scratch: vec![0u8; 65535],
		carry: None,
	};
	let future = async move { kio::wait(|waiter| driver.poll(waiter)).await };
	(shared, future)
}

/// Wait out the handshake, yielding the connection's public handle.
pub(crate) async fn establish(shared: Shared) -> Result<Connection, Error> {
	kio::wait(|waiter| {
		let mut state = shared.state.borrow_mut();
		if let Some(err) = &state.closed {
			return Poll::Ready(Err(err.clone()));
		}
		if state.established {
			return Poll::Ready(Ok(()));
		}
		waiter.register(&mut state.establish_waiters);
		Poll::Pending
	})
	.await?;

	let alpn = {
		let conn = shared.conn.borrow();
		let proto = conn.application_proto();
		(!proto.is_empty()).then(|| String::from_utf8_lossy(proto).into_owned())
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
		let mut state = self.shared.state.borrow_mut();
		if let Some(id) = state.accept_uni.pop_front() {
			return Poll::Ready(Ok(super::RecvStream::new(self.shared.clone(), id)));
		}
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
		let mut state = self.shared.state.borrow_mut();
		if let Some(id) = state.accept_bi.pop_front() {
			return Poll::Ready(Ok((
				super::SendStream::new(self.shared.clone(), id),
				super::RecvStream::new(self.shared.clone(), id),
			)));
		}
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
		let id = self.shared.state.borrow().next_open_uni;
		// Materialize the stream in quiche so the peer's MAX_STREAMS credit is
		// what gates us, not our own bookkeeping.
		match self
			.shared
			.conn
			.borrow_mut()
			.stream_priority(id, DEFAULT_URGENCY, false)
		{
			Ok(()) => {
				self.shared.state.borrow_mut().next_open_uni += 4;
				Poll::Ready(Ok(super::SendStream::new(self.shared.clone(), id)))
			}
			Err(quiche::Error::StreamLimit) => {
				let mut state = self.shared.state.borrow_mut();
				waiter.register(&mut state.open_waiters);
				Poll::Pending
			}
			Err(err) => Poll::Ready(Err(err.into())),
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
		let id = self.shared.state.borrow().next_open_bi;
		match self
			.shared
			.conn
			.borrow_mut()
			.stream_priority(id, DEFAULT_URGENCY, false)
		{
			Ok(()) => {
				self.shared.state.borrow_mut().next_open_bi += 4;
				Poll::Ready(Ok((
					super::SendStream::new(self.shared.clone(), id),
					super::RecvStream::new(self.shared.clone(), id),
				)))
			}
			Err(quiche::Error::StreamLimit) => {
				let mut state = self.shared.state.borrow_mut();
				waiter.register(&mut state.open_waiters);
				Poll::Pending
			}
			Err(err) => Poll::Ready(Err(err.into())),
		}
	}

	fn poll_send_datagram(&mut self, cx: &mut Context<'_>, payload: &[u8]) -> Poll<Result<(), Self::Error>> {
		let waiter = self.park.hold(cx);
		if let Some(err) = self.shared.closed() {
			return Poll::Ready(Err(err));
		}
		match self.shared.conn.borrow_mut().dgram_send(payload) {
			Ok(()) => {
				self.shared.kick();
				Poll::Ready(Ok(()))
			}
			// The send queue is full; a flush frees space.
			Err(quiche::Error::Done) => {
				let mut state = self.shared.state.borrow_mut();
				waiter.register(&mut state.datagram_send_waiters);
				Poll::Pending
			}
			Err(err) => Poll::Ready(Err(err.into())),
		}
	}

	fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>> {
		let waiter = self.park.hold(cx);
		let mut state = self.shared.state.borrow_mut();
		if let Some(datagram) = state.datagrams.pop_front() {
			return Poll::Ready(Ok(datagram));
		}
		if let Some(err) = &state.closed {
			return Poll::Ready(Err(err.clone()));
		}
		waiter.register(&mut state.datagram_recv_waiters);
		Poll::Pending
	}

	fn max_datagram_size(&self) -> usize {
		self.shared.conn.borrow().dgram_max_writable_len().unwrap_or(0)
	}

	fn protocol(&self) -> Option<&str> {
		self.alpn.as_deref()
	}

	fn close(&mut self, code: u32, reason: &str) {
		// Err(Done) means already closed, which is what the caller wanted.
		let _ = self
			.shared
			.conn
			.borrow_mut()
			.close(true, u64::from(code), reason.as_bytes());
		self.shared.kick();
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
		let conn = self.shared.conn.borrow();
		let stats = conn.stats();
		let path = conn.path_stats().next();
		Stats {
			bytes_sent: stats.sent_bytes,
			bytes_received: stats.recv_bytes,
			bytes_lost: stats.lost_bytes,
			packets_sent: stats.sent as u64,
			packets_received: stats.recv as u64,
			packets_lost: stats.lost as u64,
			rtt: path.as_ref().map(|path| path.rtt),
			// Bytes per second on the wire; the trait wants bits.
			send_rate: path.as_ref().map(|path| path.delivery_rate.saturating_mul(8)),
		}
	}
}

impl std::fmt::Debug for Connection {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Connection").field("alpn", &self.alpn).finish()
	}
}

/// A snapshot of quiche's counters in [`web_transport_trait::Stats`] shape.
struct Stats {
	bytes_sent: u64,
	bytes_received: u64,
	bytes_lost: u64,
	packets_sent: u64,
	packets_received: u64,
	packets_lost: u64,
	rtt: Option<std::time::Duration>,
	send_rate: Option<u64>,
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
		self.rtt
	}

	fn estimated_send_rate(&self) -> Option<u64> {
		self.send_rate
	}
}

/// The per-connection task: timers, event sweeps, and packets out. Packets in
/// come from the endpoint's demux task, which feeds quiche directly and kicks
/// this driver.
struct Driver {
	shared: Shared,
	socket: Rc<udp::Socket>,
	deadline: moq_net::runtime::Deadline<Handle>,
	/// Datagram receive scratch.
	scratch: Vec<u8>,
	/// A packet quiche handed us for a different path than the train being
	/// packed; it opens the next one.
	carry: Option<(SocketAddr, Vec<u8>)>,
}

impl Driver {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		// The endpoint fails the connection from outside when its socket dies.
		let mut ingested = {
			let mut state = self.shared.state.borrow_mut();
			if state.closed.is_some() {
				return Poll::Ready(());
			}
			// Register the kick first: a handle mutating the connection after
			// this turn's sweeps still re-polls us.
			waiter.register(&mut state.driver);
			std::mem::take(&mut state.ingested)
		};

		loop {
			self.sweep(ingested);
			ingested = false;

			if let Poll::Ready(err) = self.flush(waiter) {
				self.fail(err);
				return Poll::Ready(());
			}

			// A flush frees datagram-send queue space.
			self.shared.state.borrow_mut().datagram_send_waiters.wake();

			{
				let conn = self.shared.conn.borrow();
				self.deadline.set(conn.timeout_instant());
				if conn.is_closed() {
					let err = terminal(&conn);
					drop(conn);
					self.fail(err);
					return Poll::Ready(());
				}
			}

			// Arm, *then* poll: the poll is what registers the waiter, so
			// polling before the set would leave the firing to wake nobody
			// (fatal on a dial nobody answers, where no ingress ever re-polls
			// us). Elapsed means quiche's timeout is due right now; restart
			// the turn so its effects flush and the next deadline arms.
			if self.deadline.poll(waiter).is_pending() {
				return Poll::Pending;
			}
			self.shared.conn.borrow_mut().on_timeout();
		}
	}

	/// Everything event-shaped: establishment, new and readable streams,
	/// writability, finished sends, received datagrams.
	fn sweep(&mut self, ingested: bool) {
		let mut conn = self.shared.conn.borrow_mut();
		let mut state = self.shared.state.borrow_mut();

		if !state.established && conn.is_established() {
			state.established = true;
			state.establish_waiters.wake();
		}

		// Waking removes the entry: a still-interested poller re-registers on
		// its next poll, so the maps only hold streams somebody is parked on.
		for id in conn.readable() {
			queue_accept(&mut state, id);
			if let Some(mut waiters) = state.readable.remove(&id) {
				waiters.wake();
			}
		}
		for id in conn.writable() {
			if let Some(mut waiters) = state.writable.remove(&id) {
				waiters.wake();
			}
		}

		// MAX_STREAMS (and everything else) only arrives with ingress.
		if ingested {
			state.open_waiters.wake();
		}

		// quiche has no stream-collected event, so probe: a send stream whose
		// capacity query errors has ended one way or another.
		let mut collected = Vec::new();
		state.finishing.retain(|id, waiters| match conn.stream_capacity(*id) {
			Ok(_) => true,
			// This probe is the only reader of the stop code, so record it
			// rather than letting the collection look like a clean delivery.
			Err(quiche::Error::StreamStopped(code)) => {
				collected.push((*id, Some(code)));
				waiters.wake();
				false
			}
			Err(_) => {
				collected.push((*id, None));
				waiters.wake();
				false
			}
		});
		// Recorded before any terminal error is published this turn, so a
		// close racing the acknowledgement cannot turn a delivered FIN into a
		// reported failure.
		state.collected.extend(collected);

		let mut received = false;
		while let Ok(len) = conn.dgram_recv(&mut self.scratch) {
			if state.datagrams.len() >= DGRAM_QUEUE {
				state.datagrams.pop_front();
			}
			state.datagrams.push_back(Bytes::copy_from_slice(&self.scratch[..len]));
			received = true;
		}
		if received {
			state.datagram_recv_waiters.wake();
		}
	}

	/// Stage at most one GSO train, then yield so another connection sharing the
	/// socket gets a chance at the transmit pool. Ignores quiche's pacing hint;
	/// the congestion controller still bounds each train.
	fn flush(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		let mut tx = match self.socket.poll_acquire(waiter) {
			Poll::Ready(Ok(tx)) => tx,
			Poll::Ready(Err(err)) => return Poll::Ready(Error::Io(err.to_string())),
			// Backpressure: a completed send re-polls us.
			Poll::Pending => return Poll::Pending,
		};

		let mut filled = 0;
		let mut dest = None;
		// One train, one destination, so a packet left over from the last one
		// leads this one.
		if let Some((to, packet)) = self.carry.take() {
			tx[..packet.len()].copy_from_slice(&packet);
			filled = packet.len();
			dest = Some(to);
		}
		// A short packet must ride last in a GSO send, so a short carry goes
		// out alone.
		if filled == 0 || filled == SEGMENT {
			let mut conn = self.shared.conn.borrow_mut();
			// Pack uniform SEGMENT-sized packets back to back; a short packet
			// ends the train (it must ride last in a GSO send).
			while filled + SEGMENT <= tx.len() && filled / SEGMENT < TRAIN_SEGMENTS {
				match conn.send(&mut tx[filled..filled + SEGMENT]) {
					Ok((n, info)) => {
						// Path validation and NAT rebinding make quiche alternate
						// destinations; this packet must lead another train.
						if dest.is_some_and(|to| to != info.to) {
							self.carry = Some((info.to, tx[filled..filled + n].to_vec()));
							break;
						}
						dest = Some(info.to);
						filled += n;
						if n < SEGMENT {
							break;
						}
					}
					Err(quiche::Error::Done) => break,
					Err(err) => return Poll::Ready(err.into()),
				}
			}
		}

		let Some(to) = dest else {
			// Nothing to send; the buffer returns to the pool on drop.
			return Poll::Pending;
		};
		if let Err(err) = tx.send(filled, to, SEGMENT) {
			return Poll::Ready(Error::Io(err.to_string()));
		}
		// Requeue behind the other ready tasks. If quiche is drained, the next
		// poll costs one empty acquire and then parks normally.
		waiter.waker().wake_by_ref();
		Poll::Pending
	}

	fn fail(&mut self, err: Error) {
		self.shared.state.borrow_mut().fail(err);
	}
}

/// Queue a peer-initiated stream (and every implicitly created lower one).
fn queue_accept(state: &mut State, id: u64) {
	let uni = id & 0b10 == 0b10;
	let (next, queue, waiters) = if uni {
		(
			&mut state.next_accept_uni,
			&mut state.accept_uni,
			&mut state.accept_uni_waiters,
		)
	} else {
		(
			&mut state.next_accept_bi,
			&mut state.accept_bi,
			&mut state.accept_bi_waiters,
		)
	};
	// Locally initiated ids (a different low-bit pattern) never match `next`'s
	// parity walk, so this also filters our own streams out.
	if id & 0b11 != *next & 0b11 || id < *next {
		return;
	}
	let mut queued = false;
	while *next <= id {
		queue.push_back(*next);
		*next += 4;
		queued = true;
	}
	if queued {
		waiters.wake();
	}
}

/// The terminal error of a closed connection.
fn terminal(conn: &quiche::Connection) -> Error {
	let err = conn.peer_error().or_else(|| conn.local_error());
	match err {
		Some(err) if err.is_app => Error::App {
			code: err.error_code,
			reason: String::from_utf8_lossy(&err.reason).into_owned(),
		},
		Some(err) => Error::Transport {
			code: err.error_code,
			reason: String::from_utf8_lossy(&err.reason).into_owned(),
		},
		None => Error::TimedOut,
	}
}
