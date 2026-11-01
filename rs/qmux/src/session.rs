//! The session: shared state, the reader and writer tasks, and the handles.
//!
//! [`mux::Connection`] owns the streams, flow control, and timers; this module moves its
//! records over the transport. A writer task drains [`mux::Connection::poll_transmit`]
//! into the transport and runs the timers, and a reader task feeds each received record
//! back in. The handles call straight into the connection under its lock and park on the
//! connection's own waiter lists, which it wakes as the tasks feed it. Whatever a handle
//! queues wakes the writer through `poll_transmit` itself.

use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Poll, ready};
use std::time::Duration;

use bytes::{Buf, Bytes};
use kio::clock::Instant;
use moq_net::transport::{self as net, Address, poll};
use moq_quic::stream::{Dir, Id as StreamId};
use moq_quic::{
	Side, TransportError, TransportErrorCode, VarInt, connection::Error as ConnectionError, mux, recv, send,
};

use crate::transport::{Reader, Transport, Writer};
use crate::{Config, Error, SharedSocketStats};

/// How long a closing session keeps the transport open to deliver its close and finish graceful
/// shutdown, like QUIC's draining period. A peer that stopped reading or never finishes its send
/// half cannot hold the transport open past it.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);

/// The most bytes gathered before the writer flushes the transport.
const BATCH_BYTES: usize = 64 * 1024;

/// Everything the handles and the tasks share.
struct Inner {
	state: Mutex<State>,
	/// Updated by the reader and writer, so stats never take the lock.
	wire: net::Wire,
	socket_stats: Option<SharedSocketStats>,
	address: Address,
}

struct State {
	conn: mux::Connection,
	/// Wakes the writer on ingress: the reader fed the connection a record, which may queue
	/// an answer or move a timer. What the handles queue wakes it through `poll_transmit`.
	driver: kio::WaiterList,

	/// The terminal error, set exactly once. Handles report it in place of the connection's
	/// own reason once it is set, since it can say more (the transport's I/O error, say).
	///
	/// The connection's `poll_closed` resolves the moment it is closed, but a close this side
	/// asked for is published here only once the writer delivered it and shut the transport
	/// (see `local_close`), so [`Session`]'s `poll_closed` keeps this latch and its own list.
	/// Only the writer knows when the close frame reached the transport, which the sans-I/O
	/// connection cannot see, and a caller may exit the moment `poll_closed` resolves, so
	/// publishing earlier would leave the frame unwritten.
	closed: Option<Error>,
	closed_waiters: kio::WaiterList,
	/// The error for a close this side asked for, published once the writer has delivered
	/// the close frame, so a caller that exits the moment `poll_closed` resolves already
	/// handed it to the transport.
	local_close: Option<Error>,

	sent_bytes: u64,
	received_bytes: u64,
}

impl State {
	/// Terminate with `err` (the first one wins), waking whoever waits on the session's
	/// `poll_closed`.
	///
	/// Every caller loses the connection under the same lock, which wakes everyone parked on
	/// it, the writer included.
	fn fail(&mut self, err: Error) {
		if self.closed.is_some() {
			return;
		}
		self.closed = Some(err);
		self.closed_waiters.wake();
	}

	/// Publish a loss the connection recorded since the last check: a remote close or a
	/// violation fails the session now, while a close this side asked for waits for the writer
	/// to deliver its frame.
	///
	/// Called after every call that can lose the connection: a record, a timeout, a close.
	fn check_lost(&mut self) {
		if self.closed.is_some() || self.local_close.is_some() {
			return;
		}
		let Some(reason) = self.conn.error() else {
			return;
		};
		let local = matches!(reason, ConnectionError::LocallyClosed { .. });
		let err = Error::lost(reason.clone());
		match local {
			// The writer publishes a local close once its frame is out.
			true => self.local_close = Some(err),
			false => self.fail(err),
		}
	}

	/// What a handle reports once the connection is lost: the session's terminal error, or
	/// why the connection ended while a local close is still being delivered.
	fn lost(&self) -> Error {
		if let Some(err) = &self.closed {
			return err.clone();
		}
		self.conn.error().cloned().map_or(Error::Closed, Error::lost)
	}
}

impl Inner {
	fn lock(&self) -> MutexGuard<'_, State> {
		self.state.lock().unwrap()
	}

	/// Close with an APPLICATION_CLOSE, reporting it once it is out.
	fn close(&self, code: u64, reason: &str) {
		let mut state = self.lock();
		if state.closed.is_some() || state.conn.is_closed() {
			return;
		}
		// Closing wakes everyone parked on the connection, the reader included: a closing
		// connection ignores whatever else the peer sends. The writer hears it too, through
		// `poll_transmit` to deliver the close, or `poll_closed` to start a stalled one's close
		// bound.
		state
			.conn
			.close(VarInt::from_u64(code).unwrap_or(VarInt::MAX), reason.to_string());
		state.check_lost();
	}

	/// Gather the next records to write, running the timers while there are none, or `None`
	/// once the connection has nothing left to say.
	fn poll_batch(&self, waiter: &kio::Waiter, timer: &mut Timer) -> Poll<Option<Vec<Bytes>>> {
		let mut state = self.lock();
		// Ingress may queue an answer or move a timer; what the handles queue wakes us through
		// `poll_transmit`.
		waiter.register(&mut state.driver);
		loop {
			let now = kio::clock::now();
			if state.conn.poll_timeout().is_some_and(|at| now >= at) {
				state.conn.handle_timeout(now);
			}

			let mut batch = Vec::new();
			let mut size = 0;
			while size < BATCH_BYTES {
				let Poll::Ready(record) = state.conn.poll_transmit(waiter, now) else {
					break;
				};
				size += record.len();
				batch.push(record);
			}
			state.check_lost();
			if !batch.is_empty() {
				return Poll::Ready(Some(batch));
			}
			if state.conn.is_drained() {
				return Poll::Ready(None);
			}
			if !timer.poll(waiter, state.conn.poll_timeout()) {
				return Poll::Pending;
			}
		}
	}

	/// Run the timers while a write is stalled, resolving once a closing session has spent
	/// its [`CLOSE_TIMEOUT`] and should drop the transport.
	fn poll_stalled(&self, waiter: &kio::Waiter, timer: &mut Timer) -> Poll<()> {
		let mut state = self.lock();
		// Ingress may move a timer. A close, local or not, starts the close bound; wait for it
		// with `poll_closed` rather than `poll_transmit`, which would take the records the
		// stalled write has no room for.
		waiter.register(&mut state.driver);
		let _ = state.conn.poll_closed(waiter);
		if !timer.stalled {
			// The peer is evidently there but slow, which defers the idle timeout.
			timer.stalled = true;
			state.conn.set_backpressured(true);
		}
		loop {
			let now = kio::clock::now();
			if state.conn.is_closed() && timer.closing.is_none() {
				timer.closing = Some(now + CLOSE_TIMEOUT);
			}
			if timer.closing.is_some_and(|at| now >= at) {
				return Poll::Ready(());
			}
			if state.conn.poll_timeout().is_some_and(|at| now >= at) {
				state.conn.handle_timeout(now);
				state.check_lost();
			}
			let deadline = match (state.conn.poll_timeout(), timer.closing) {
				(Some(a), Some(b)) => Some(a.min(b)),
				(a, b) => a.or(b),
			};
			if !timer.poll(waiter, deadline) {
				return Poll::Pending;
			}
		}
	}

	/// Resolve once the connection ended, which is when the reader stops.
	fn poll_ended(&self, waiter: &kio::Waiter) -> Poll<()> {
		// The session only fails once the connection is lost, so this covers that too.
		self.lock().conn.poll_closed(waiter).map(drop)
	}

	/// The writer stopped: publish a local close, or the failure that stopped it.
	fn writer_done(&self, result: Result<(), Error>) {
		let mut state = self.lock();
		state.check_lost();
		if let Some(err) = state.local_close.take() {
			state.fail(err);
		}
		if let Err(err) = result {
			state.fail(err);
		}
		state.conn.transport_closed();
		// A drained connection ended with a close from either side, already published; this
		// only lands when the transport died under a live one.
		state.fail(Error::Closed);
	}
}

/// The writer's timers.
#[derive(Default)]
struct Timer {
	sleep: Option<kio::clock::Sleep>,
	/// When a closing session gives up on delivering its close frame.
	closing: Option<Instant>,
	/// The current write is stalled on the transport.
	stalled: bool,
}

impl Timer {
	/// Wait for `deadline`, returning whether it already passed.
	fn poll(&mut self, waiter: &kio::Waiter, deadline: Option<Instant>) -> bool {
		let Some(deadline) = deadline else {
			return false;
		};
		if kio::clock::now() >= deadline {
			return true;
		}
		let sleep = match &mut self.sleep {
			Some(sleep) => {
				if sleep.deadline() != deadline {
					sleep.reset(deadline);
				}
				sleep
			}
			None => self.sleep.insert(kio::clock::sleep_until(deadline)),
		};
		waiter.poll_future(Pin::new(sleep)).is_ready()
	}
}

/// Drain the connection into `writer` until it has nothing left to say.
async fn run_writer<W: Writer>(inner: Arc<Inner>, mut writer: W, reader: tokio::task::JoinHandle<()>) {
	let mut timer = Timer::default();
	let result = async {
		loop {
			let batch = tokio::select! {
				biased;
				// Transport-level maintenance, like a WebSocket keep-alive Ping. First, since a
				// writer that always has a batch ready would otherwise never send one; it is
				// ready only once per keep-alive interval.
				result = writer.maintain() => {
					result?;
					continue;
				}
				batch = kio::wait(|waiter| inner.poll_batch(waiter, &mut timer)) => batch,
			};
			let Some(batch) = batch else {
				return Ok(());
			};

			let size: usize = batch.iter().map(Bytes::len).sum();
			let write = async {
				for record in batch {
					writer.feed(record).await?;
				}
				writer.flush().await
			};
			tokio::pin!(write);
			let result = tokio::select! {
				biased;
				result = &mut write => result,
				// Pending while the write runs; resolves once a close has waited long enough.
				() = kio::wait(|waiter| inner.poll_stalled(waiter, &mut timer)) => Err(Error::Closed),
			};
			if std::mem::take(&mut timer.stalled) {
				inner.lock().conn.set_backpressured(false);
			}
			result?;

			let mut state = inner.lock();
			state.sent_bytes += size as u64;
			inner.wire.set_sent_bytes(state.sent_bytes);
		}
	}
	.await;

	// Shut the transport down gracefully, unless it already failed, within what is left of
	// the close bound. The drained connection has already woken the reader; waiting for it to
	// drop its half lets the close drain the peer instead of leaving that to the reader.
	if result.is_ok() {
		let bound = timer
			.closing
			.map_or(CLOSE_TIMEOUT, |at| at.saturating_duration_since(kio::clock::now()));
		let close = async {
			let _ = reader.await;
			writer.close().await
		};
		let _ = kio::clock::timeout(bound, close).await;
	}
	inner.writer_done(result);
}

/// Feed every record from `reader` into the connection until it ends.
async fn run_reader<R: Reader>(inner: Arc<Inner>, mut reader: R) {
	loop {
		let record = tokio::select! {
			biased;
			() = kio::wait(|waiter| inner.poll_ended(waiter)) => return,
			record = reader.recv() => record,
		};

		let mut state = inner.lock();
		let violation = |code, reason: &str| TransportError::new(code, reason.to_string());
		match record {
			Ok(record) => {
				state.received_bytes += record.len() as u64;
				inner.wire.set_received_bytes(state.received_bytes);
				state.conn.handle_record(record, kio::clock::now());
			}
			Err(Error::FrameTooLarge) => state.conn.handle_oversized(),
			Err(Error::FlowControlError) => state.conn.handle_violation(violation(
				TransportErrorCode::FLOW_CONTROL_ERROR,
				"transport buffer overrun",
			)),
			Err(Error::FrameEncoding) => state
				.conn
				.handle_violation(violation(TransportErrorCode::FRAME_ENCODING_ERROR, "transport framing")),
			Err(Error::ProtocolViolation) => state
				.conn
				.handle_violation(violation(TransportErrorCode::PROTOCOL_VIOLATION, "transport framing")),
			Err(err) => {
				// The transport's own error says more than the connection's reset, unless
				// this side is closing, when the peer hanging up is the expected answer and
				// the writer publishes the close it asked for.
				if state.local_close.is_none() {
					state.fail(err);
				}
				state.conn.transport_closed();
			}
		}
		state.check_lost();
		// Ingress: the record may owe the peer an answer or move a timer.
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
	/// before returning.
	///
	/// "Established" means the peer's transport parameters have been received and applied,
	/// so [`protocol`](Self::protocol) returns its final value. Bounded by
	/// [`Config::handshake_timeout`]: if the peer completes the transport handshake but
	/// never sends its parameters, this returns [`Error::HandshakeTimeout`] rather than
	/// hanging.
	pub async fn connect<T: Transport>(transport: T, config: Config) -> Result<Session, Error> {
		Self::new(transport, Side::Client, config)?.established().await
	}

	/// Open a server-side session over the given transport, waiting until it is
	/// established before returning. See [`Session::connect`] for the semantics.
	pub async fn accept<T: Transport>(transport: T, config: Config) -> Result<Session, Error> {
		Self::new(transport, Side::Server, config)?.established().await
	}

	/// Start a session over the transport without waiting for the handshake, for callers
	/// that resolved the protocol out of band (the WebSocket subprotocol, say).
	pub(crate) fn new<T: Transport>(transport: T, side: Side, config: Config) -> Result<Self, Error> {
		let conn = mux::Connection::new(config.to_mux()?, side, kio::clock::now());
		let inner = Arc::new(Inner {
			state: Mutex::new(State {
				conn,
				driver: kio::WaiterList::new(),
				closed: None,
				closed_waiters: kio::WaiterList::new(),
				local_close: None,
				sent_bytes: 0,
				received_bytes: 0,
			}),
			wire: net::Wire::new(),
			socket_stats: transport.socket_stats(),
			address: transport.address(),
		});

		let (writer, reader) = transport.split(&config);
		let reader = tokio::spawn(run_reader(inner.clone(), reader));
		tokio::spawn(run_writer(inner.clone(), writer, reader));

		Ok(Self {
			_guard: Arc::new(Guard(inner.clone())),
			inner,
			protocol: config.negotiated_protocol(),
			#[cfg(feature = "tls")]
			tls: None,
		})
	}

	/// Wait for the peer's transport parameters, then report the negotiated protocol.
	async fn established(mut self) -> Result<Self, Error> {
		let inner = self.inner.clone();
		kio::wait(|waiter| {
			let mut state = inner.lock();
			state.conn.poll_connected(waiter).map_err(|_| state.lost())
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
		state.conn.streams().poll_accept(waiter, dir).map_err(|_| state.lost())
	}

	fn poll_open_dir(
		&self,
		waiter: &kio::Waiter,
		dir: Dir,
		options: poll::SendOptions<'_, SendGroup>,
	) -> Poll<Result<StreamId, Error>> {
		let mut state = self.inner.lock();
		// The peer's MAX_STREAMS credit is what gates us: this parks while it is spent.
		let id = ready!(state.conn.streams().poll_open(waiter, dir, to_quic(options))).map_err(|_| state.lost())?;
		Poll::Ready(Ok(id))
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
		if state.conn.is_closed() {
			return Poll::Ready(Err(state.lost()));
		}
		// A session started on an out-of-band protocol (a WebSocket subprotocol) runs before
		// the peer's transport parameters arrive. Until then its credit is unknown, not spent.
		ready!(state.conn.poll_connected(waiter)).map_err(|_| state.lost())?;
		let id = state.conn.streams().open(Dir::Bi, to_quic(options));
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
		Ok(())
	}

	fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>> {
		let mut state = self.inner.lock();
		state.conn.datagrams().poll_recv(waiter).map_err(|_| state.lost())
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
		}
	}

	fn wire(&self) -> Option<net::Wire> {
		Some(self.inner.wire.clone())
	}
}

/// A snapshot of the session's metrics; its [`net::Wire`] holds the byte counts.
struct Metrics {
	rtt: Option<Duration>,
	rate: Option<u64>,
}

impl net::Metrics for Metrics {
	fn rtt(&self) -> Option<Duration> {
		self.rtt
	}

	fn estimated_send_rate(&self) -> Option<u64> {
		self.rate
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
		// Takes only what the connection accepts, which for a `Bytes` source is no copy at all.
		match ready!(state.conn.send_stream(self.id).poll_write(waiter, buf)) {
			Ok(n) => Poll::Ready(Ok(n)),
			Err(send::Error::Stopped(code)) => Poll::Ready(Err(Error::StreamStop(code.into_inner()))),
			Err(send::Error::ConnectionLost) => Poll::Ready(Err(state.lost())),
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
			// over without our reset, so `poll_closed` reports the stop the connection recorded.
			Err(send::Error::Stopped(code)) => {
				self.fin = true;
				return Err(Error::StreamStop(code.into_inner()));
			}
			// Already finished or reset, so the FIN it wanted is out.
			Err(_) => {}
		}
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
		match ready!(state.conn.send_stream(self.id).poll_end(waiter)) {
			Ok(send::End::Stopped(code)) => Poll::Ready(Err(Error::StreamStop(code.into_inner()))),
			Ok(_) => Poll::Ready(Ok(())),
			// The end never came. If the session died first, the caller cannot read success
			// as "every byte arrived".
			Err(send::Error::ConnectionLost) => Poll::Ready(Err(state.lost())),
			// The ID is only released on drop, so this does not happen.
			Err(_) => Poll::Ready(Err(Error::StreamClosed)),
		}
	}
}

impl Drop for SendStream {
	fn drop(&mut self) {
		let mut state = self.inner.lock();
		// Reset before giving up the ID, which refuses every later call on it.
		if !self.fin && !self.reset {
			let _ = state.conn.send_stream(self.id).reset(VarInt::from_u32(0));
		}
		state.conn.send_stream(self.id).release();
	}
}

impl std::fmt::Debug for SendStream {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("SendStream").field("id", &self.id).finish()
	}
}

/// An incoming stream. Dropping it unfinished sends STOP_SENDING with code 0.
///
/// Reading returns the peer's connection credit as it goes, so incomplete messages are
/// bounded by the largest message each rather than by `receive_window`, and nothing is evicted.
pub struct RecvStream(poll::Buffered<RecvChunks>);

impl RecvStream {
	fn new(inner: Arc<Inner>, id: StreamId) -> Self {
		Self(poll::Buffered::new(RecvChunks {
			inner,
			id,
			finished: false,
			reset: None,
			stopped: false,
		}))
	}
}

impl poll::RecvStream for RecvStream {
	type Error = Error;
	type Lease<'a> = poll::BufferedLease<'a>;

	fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<Self::Lease<'_>, Error>> {
		self.0.poll_lease(waiter, want)
	}

	fn set_priority(&mut self, priority: i64) {
		self.0.set_priority(priority);
	}

	fn stop(&mut self, code: u32) {
		self.0.stop(code);
	}
}

impl std::fmt::Debug for RecvStream {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("RecvStream").field("id", &self.0.get_ref().id).finish()
	}
}

/// Reads a [`RecvStream`] out of the connection's reassembly buffer.
struct RecvChunks {
	inner: Arc<Inner>,
	id: StreamId,
	/// Every byte up to the FIN was read out of the connection.
	finished: bool,
	/// The peer reset the stream with this code. Reading it frees the connection's stream
	/// state, so later reads report it from here.
	reset: Option<u64>,
	/// We stopped the stream; no more reads matter.
	stopped: bool,
}

impl poll::Chunks for RecvChunks {
	type Error = Error;

	fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		if max == 0 {
			return Poll::Ready(Ok(Some(Bytes::new())));
		}
		if let Some(code) = self.reset {
			return Poll::Ready(Err(Error::StreamReset(code)));
		}
		if self.finished {
			return Poll::Ready(Ok(None));
		}
		let mut state = self.inner.lock();
		match ready!(poll_read(&mut state, waiter, self.id, max)) {
			Ok(Some(bytes)) => Poll::Ready(Ok(Some(bytes))),
			Ok(None) => {
				self.finished = true;
				Poll::Ready(Ok(None))
			}
			Err(recv::Error::Reset(code)) => {
				let code = code.into_inner();
				self.reset = Some(code);
				Poll::Ready(Err(Error::StreamReset(code)))
			}
			// Only once everything that arrived before the loss was read.
			Err(recv::Error::ConnectionLost) => Poll::Ready(Err(state.lost())),
			Err(_) => Poll::Ready(Err(Error::StreamClosed)),
		}
	}

	fn stop(&mut self, code: u32) {
		if self.stopped || self.finished || self.reset.is_some() {
			return;
		}
		let mut state = self.inner.lock();
		// Err means the stream is already gone, which is what we wanted.
		let _ = state.conn.recv_stream(self.id).stop(VarInt::from_u32(code));
		self.stopped = true;
	}
}

/// Take up to `max` bytes out of the reassembly buffer, or wait for more, returning `None`
/// once every byte up to the FIN has been handed over.
///
/// Reading is what returns the peer's flow control credit; a read that owes it a frame wakes
/// the writer through the connection.
fn poll_read(
	state: &mut State,
	waiter: &kio::Waiter,
	id: StreamId,
	max: usize,
) -> Poll<Result<Option<Bytes>, recv::Error>> {
	let mut recv = state.conn.recv_stream(id);
	let Ok(mut chunks) = recv.read() else {
		// The stream is gone, so everything it held is already ours.
		return Poll::Ready(Ok(None));
	};
	let read = chunks.poll_next(waiter, max);
	let _ = chunks.finalize();
	read.map_ok(|chunk| chunk.map(|chunk| chunk.bytes))
}

impl Drop for RecvChunks {
	fn drop(&mut self) {
		let mut state = self.inner.lock();
		if !self.finished && !self.stopped && self.reset.is_none() {
			let _ = state.conn.recv_stream(self.id).stop(VarInt::from_u32(0));
		}
	}
}

#[cfg(all(test, any(feature = "tcp", all(unix, feature = "uds"))))]
mod tests;
