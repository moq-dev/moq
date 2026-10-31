//! A single connection: its streams, datagrams, and why it ends.

use std::{
	any::Any,
	fmt,
	future::Future,
	io,
	net::SocketAddr,
	pin::Pin,
	sync::{
		Arc, OnceLock,
		atomic::{AtomicUsize, Ordering},
	},
	task::{Context, Poll, Waker, ready},
};

use bytes::{Buf, Bytes};
use pin_project_lite::pin_project;
use rustc_hash::FxHashMap;
use tokio::sync::{
	Notify,
	futures::{Notified, OwnedNotified},
	mpsc, oneshot,
};
use tracing::{Instrument, Span, debug_span};

use crate::quic::driver::{
	ConnectionEvent, Duration, Instant, VarInt, mutex::Mutex, recv, runtime::Slot, send, udp_transmit,
};
use moq_congestion::Controller;
pub use moq_quic::connection::{Error, Stats};
use moq_quic::{
	EndpointEvent, StreamEvent, TransportError, TransportErrorCode,
	endpoint::ConnectionHandle,
	stream::{Dir, Id as StreamId},
};

/// In-progress connection attempt future
///
/// Resolves into a [`Connection`] once the TLS handshake completes. In TLS 1.3, peer
/// certificates arrive with handshake completion, so they are only available via
/// [`Connection::peer_identity()`]. [`Connection`] is a lightweight handle, so completing the
/// handshake imposes no extra cost; use [`Connection::close()`] to reject peers that fail
/// custom validation.
#[derive(Debug)]
pub struct Connecting {
	conn: Option<ConnectionRef>,
	connected: Pin<Box<OwnedNotified>>,
	handshake_data_ready: Option<oneshot::Receiver<()>>,
}

impl Connecting {
	pub(crate) fn new(
		handle: ConnectionHandle,
		conn: moq_quic::connection::Connection,
		endpoint_events: mpsc::UnboundedSender<(ConnectionHandle, EndpointEvent)>,
		conn_events: mpsc::UnboundedReceiver<ConnectionEvent>,
		slot: Arc<Slot>,
		runtime: &tokio::runtime::Handle,
	) -> Self {
		let (on_handshake_data_send, on_handshake_data_recv) = oneshot::channel();
		let shared = Shared {
			#[cfg(feature = "net")]
			probe: conn.congestion_state().probes().then(moq_net::transport::Probe::new),
			..Shared::default()
		};

		let conn = ConnectionRef(Arc::new(ConnectionInner {
			state: Mutex::new(State::new(
				conn,
				handle,
				endpoint_events,
				conn_events,
				on_handshake_data_send,
				slot,
			)),
			shared,
		}));
		let connected = Box::pin(conn.shared.connected.clone().notified_owned());

		let driver = ConnectionDriver::new(conn.clone());
		runtime.spawn(
			async {
				if let Err(e) = driver.await {
					tracing::error!("I/O error: {e}");
				}
			}
			.instrument(Span::current()),
		);

		Self {
			conn: Some(conn),
			connected,
			handshake_data_ready: Some(on_handshake_data_recv),
		}
	}

	/// Parameters negotiated during the handshake
	///
	/// The dynamic type returned is determined by the configured
	/// [`Session`](moq_quic::crypto::Session). For the default `rustls` session, the return value can
	/// be [`downcast`](Box::downcast) to a
	/// [`crypto::rustls::HandshakeData`](crate::quic::driver::crypto::rustls::HandshakeData).
	///
	/// Peer certificates are not available at this stage; see [`Connection::peer_identity()`].
	///
	/// This operation is cancel-safe.
	pub async fn handshake_data(&mut self) -> Result<Box<dyn Any>, Error> {
		// Taking &mut self allows us to use a single oneshot channel rather than dealing with
		// potentially many tasks waiting on the same event. It's a bit of a hack, but keeps things
		// simple.
		if let Some(x) = self.handshake_data_ready.as_mut() {
			let _ = x.await;
			// Once the data is ready, apply state changes. This prevents a panic due to
			// inconsistent state when retrying a call to `handshake_data`.
			self.handshake_data_ready = None;
		}
		let conn = self.conn.as_ref().unwrap();
		let inner = conn.state.lock();
		inner
			.inner
			.crypto_session()
			.handshake_data()
			.ok_or_else(|| inner.error.clone().expect("spurious handshake data ready notification"))
	}

	/// The peer's UDP address
	///
	/// Will panic if called after `poll` has returned `Ready`.
	pub fn remote_address(&self) -> SocketAddr {
		let conn_ref: &ConnectionRef = self.conn.as_ref().expect("used after yielding Ready");
		conn_ref.state.lock().inner.remote_address()
	}
}

impl Future for Connecting {
	type Output = Result<Connection, Error>;
	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		Pin::new(&mut self.connected).poll(cx).map(|_| {
			let conn = self.conn.take().unwrap();
			let inner = conn.state.lock();
			if inner.connected {
				drop(inner);
				Ok(Connection(conn))
			} else {
				Err(inner
					.error
					.clone()
					.expect("connected signaled without connection success or error"))
			}
		})
	}
}

/// A future that drives protocol logic for a connection
///
/// This future handles the protocol logic for a single connection, routing events from the
/// `Connection` API object to the `Endpoint` task and the related stream-related interfaces.
/// It also keeps track of outstanding timeouts for the `Connection`.
///
/// If the connection encounters an error condition, this future will yield an error. It will
/// terminate (yielding `Ok(())`) if the connection was closed without error. Unlike other
/// connection-related futures, this waits for the draining period to complete to ensure that
/// packets still in flight from the peer are handled gracefully.
#[must_use = "connection drivers must be spawned for their connections to function"]
#[derive(Debug)]
struct ConnectionDriver {
	conn: ConnectionRef,
	span: Span,
}

impl ConnectionDriver {
	fn new(conn: ConnectionRef) -> Self {
		let span = {
			let conn = &mut conn.state.lock();
			debug_span!("drive", id = conn.handle.0)
		};

		Self { conn, span }
	}
}

impl Future for ConnectionDriver {
	type Output = Result<(), io::Error>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let conn = &mut *self.conn.state.lock();
		let _guard = self.span.enter();

		if let Err(e) = conn.process_conn_events(&self.conn.shared, cx) {
			conn.terminate(e, &self.conn.shared);
			return Poll::Ready(Ok(()));
		}
		// A new target waits for the next poll: probing only resends data in flight, so it has
		// nothing to send until a write or an ACK polls anyway.
		#[cfg(feature = "net")]
		if let Some(probe) = &self.conn.shared.probe {
			conn.inner.set_probe_target(probe.target().as_bps() / 8);
		}
		let mut keep_going = conn.drive_transmit(cx).inspect_err(|_| {
			// Transmit failed, so close the connection and clean state before returning the error.
			if !conn.inner.is_closed() {
				conn.implicit_close(&self.conn.shared);
			}
		})?;
		// If a timer expires, there might be more to transmit. When we transmit something, we
		// might need to reset a timer. Hence, we must loop until neither happens.
		keep_going |= conn.drive_timer(cx);
		conn.forward_endpoint_events();
		conn.forward_app_events(&self.conn.shared);

		// Every datagram sent or received passes through this poll.
		#[cfg(feature = "net")]
		{
			let stats = conn.inner.stats();
			self.conn.shared.wire.set_sent_bytes(stats.udp_tx.bytes);
			self.conn.shared.wire.set_received_bytes(stats.udp_rx.bytes);
		}

		if !conn.inner.is_drained() {
			if keep_going {
				// If the connection hasn't processed all tasks, schedule it again
				cx.waker().wake_by_ref();
			} else {
				match conn.driver.as_mut() {
					Some(old_waker) => old_waker.clone_from(cx.waker()),
					None => conn.driver = Some(cx.waker().clone()),
				};
			}
			return Poll::Pending;
		}
		if conn.error.is_none() {
			unreachable!("drained connections always have an error");
		}
		Poll::Ready(Ok(()))
	}
}

/// A QUIC connection.
///
/// If all references to a connection (including every clone of the `Connection` handle, streams of
/// incoming streams, and the various stream types) have been dropped, then the connection will be
/// automatically closed with an `error_code` of 0 and an empty `reason`. You can also close the
/// connection explicitly by calling [`Connection::close()`].
///
/// Closing the connection immediately abandons efforts to deliver data to the peer.  Upon
/// receiving CONNECTION_CLOSE the peer *may* drop any stream data not yet delivered to the
/// application. [`Connection::close()`] describes in more detail how to gracefully close a
/// connection without losing application data.
///
/// May be cloned to obtain another handle to the same connection.
///
/// [`Connection::close()`]: Connection::close
#[derive(Debug, Clone)]
pub struct Connection(ConnectionRef);

impl Connection {
	/// Initiate a new outgoing unidirectional stream, scheduled by `options`.
	///
	/// Streams are cheap and instantaneous to open unless blocked by flow control. As a
	/// consequence, the peer won't be notified that a stream has been opened until the stream is
	/// actually used.
	///
	/// # Panics
	/// - when `options.group` belongs to another connection
	pub fn open_uni<'a>(
		&'a self,
		options: send::Options<'a>,
	) -> impl Future<Output = Result<send::Stream, Error>> + 'a {
		OpenUni {
			conn: &self.0,
			options,
			notify: self.0.shared.stream_budget_available[Dir::Uni as usize].notified(),
		}
	}

	/// Open a send group at `priority`, for streams and datagrams to join with [`send::Options`]
	/// or [`send::Stream::set_group`].
	pub fn send_group(&self, priority: i64) -> send::Group {
		send::Group::new(self.0.0.clone(), priority)
	}

	/// Initiate a new outgoing bidirectional stream, its sending half scheduled by `options`.
	///
	/// Streams are cheap and instantaneous to open unless blocked by flow control. As a
	/// consequence, the peer won't be notified that a stream has been opened until the stream is
	/// actually used. Calling [`open_bi()`] then waiting on the [`recv::Stream`] without writing
	/// anything to [`send::Stream`] will never succeed.
	///
	/// [`open_bi()`]: crate::quic::driver::connection::Connection::open_bi
	///
	/// # Panics
	/// - when `options.group` belongs to another connection
	pub fn open_bi<'a>(
		&'a self,
		options: send::Options<'a>,
	) -> impl Future<Output = Result<(send::Stream, recv::Stream), Error>> + 'a {
		OpenBi {
			conn: &self.0,
			options,
			notify: self.0.shared.stream_budget_available[Dir::Bi as usize].notified(),
		}
	}

	/// Open a request stream using only the credit already granted by the peer.
	#[cfg(feature = "net")]
	pub(crate) fn open_request(&self) -> Result<Option<(send::Stream, recv::Stream)>, Error> {
		let mut state = self.0.state.lock();
		if let Some(err) = &state.error {
			return Err(err.clone());
		}
		let Some(id) = state.inner.streams().open(Dir::Bi, Default::default()) else {
			return Ok(None);
		};
		drop(state);
		Ok(Some((
			send::Stream::new(self.0.clone(), id),
			recv::Stream::new(self.0.clone(), id),
		)))
	}

	/// Accept the next incoming uni-directional stream
	///
	/// # Cancel safety
	///
	/// This method is cancellation safe. If this does not resolve, no streams were consumed.
	pub fn accept_uni(&self) -> impl Future<Output = Result<recv::Stream, Error>> + '_ {
		AcceptUni {
			conn: &self.0,
			notify: self.0.shared.stream_incoming[Dir::Uni as usize].notified(),
		}
	}

	/// Accept the next incoming bidirectional stream
	///
	/// **Important Note**: The `Connection` that calls [`open_bi()`] must write to its [`send::Stream`]
	/// before the other `Connection` is able to `accept_bi()`. Calling [`open_bi()`] then
	/// waiting on the [`recv::Stream`] without writing anything to [`send::Stream`] will never succeed.
	///
	/// [`accept_bi()`]: crate::quic::driver::connection::Connection::accept_bi
	/// [`open_bi()`]: crate::quic::driver::connection::Connection::open_bi
	///
	/// # Cancel safety
	///
	/// This method is cancellation safe. If this does not resolve, no streams were consumed.
	pub fn accept_bi(&self) -> impl Future<Output = Result<(send::Stream, recv::Stream), Error>> + '_ {
		AcceptBi {
			conn: &self.0,
			notify: self.0.shared.stream_incoming[Dir::Bi as usize].notified(),
		}
	}

	/// Receive an application datagram
	///
	/// # Cancel safety
	///
	/// This method is cancellation safe. If this does not resolve, no datagrams were consumed.
	pub fn read_datagram(&self) -> impl Future<Output = Result<Bytes, Error>> + '_ {
		ReadDatagram {
			conn: &self.0,
			notify: self.0.shared.datagram_received.notified(),
		}
	}

	/// Wait for the connection to be closed for any reason
	///
	/// Despite the return type's name, closed connections are often not an error condition at the
	/// application layer. Cases that might be routine include [`Error::LocallyClosed`]
	/// and [`Error::ApplicationClosed`].
	pub async fn closed(&self) -> Error {
		{
			let conn = self.0.state.lock();
			if let Some(error) = conn.error.as_ref() {
				return error.clone();
			}
			// Construct the future while the lock is held to ensure we can't miss a wakeup if
			// the `Notify` is signaled immediately after we release the lock. `await` it after
			// the lock guard is out of scope.
			self.0.shared.closed.notified()
		}
		.await;
		self.0
			.state
			.lock()
			.error
			.as_ref()
			.expect("closed without an error")
			.clone()
	}

	/// Whether the connection is closed, and why.
	///
	/// The close_reason is always set to `Some(Error)` when a socket is
	/// closed; whether it was closed manually by calling [`Connection::close()`] or due to
	/// an internal error (such as an idle timeout or the peer closing the
	/// connection).
	///
	/// Note: when the connection is closed, `connection.close_reason().is_some()` will always be true.
	pub fn close_reason(&self) -> Option<Error> {
		self.0.state.lock().error.clone()
	}

	/// Close the connection immediately.
	///
	/// Pending operations will fail immediately with [`Error::LocallyClosed`]. No
	/// more data is sent to the peer and the peer may drop buffered data upon receiving
	/// the CONNECTION_CLOSE frame.
	///
	/// `error_code` and `reason` are not interpreted, and are provided directly to the peer.
	///
	/// `reason` will be truncated to fit in a single packet with overhead; to improve odds that it
	/// is preserved in full, it should be kept under 1KiB.
	///
	/// # Gracefully closing a connection
	///
	/// Only the peer last receiving application data can be certain that all data is
	/// delivered. The only reliable action it can then take is to close the connection,
	/// potentially with a custom error code. The delivery of the final CONNECTION_CLOSE
	/// frame is very likely if both endpoints stay online long enough, and
	/// [`Endpoint::close()`](crate::quic::driver::endpoint::Endpoint::close) waits for it. Otherwise, the
	/// remote peer will time out the connection, provided that the idle timeout is not
	/// disabled.
	///
	/// The sending side can not guarantee all stream data is delivered to the remote
	/// application. It only knows the data is delivered to the QUIC stack of the remote
	/// endpoint. Once the local side sends a CONNECTION_CLOSE frame in response to calling
	/// [`close()`] the remote endpoint may drop any data it received but is as yet
	/// undelivered to the application, including data that was acknowledged as received to
	/// the local endpoint.
	///
	/// [`close()`]: Connection::close
	pub fn close(&self, error_code: VarInt, reason: &str) {
		let conn = &mut *self.0.state.lock();
		conn.close(error_code, reason.to_string(), &self.0.shared);
	}

	/// Transmit `data` as an unreliable application datagram, scheduled by `options`
	///
	/// Application datagrams are a low-level primitive. They may be lost or delivered out of order,
	/// and `data` must both fit inside a single QUIC packet and be smaller than the maximum
	/// dictated by the peer.
	///
	/// The datagram is scheduled like one of its group's streams at its priority, or among the
	/// ungrouped streams without a group. Sending never blocks: a full buffer drops the oldest
	/// datagram of the lowest-priority group, see [`moq_quic::Datagrams::send`]. A header chained
	/// in front of a [`Bytes`] payload costs no copy of the payload.
	///
	/// # Panics
	/// - when `options.group` belongs to another connection
	pub fn send_datagram(&self, data: impl Buf, options: send::Options<'_>) -> Result<(), send::Error> {
		let options = options.to_quic(&self.0.0);
		let conn = &mut *self.0.state.lock();
		if let Some(x) = &conn.error {
			return Err(send::Error::Connection(x.clone()));
		}
		match conn.inner.datagrams().send(data, options) {
			Ok(()) => {}
			Err(moq_quic::send::Error::TooLarge) => return Err(send::Error::TooLarge),
			Err(_) => return Err(send::Error::Unsupported),
		}
		conn.wake();
		Ok(())
	}

	/// Compute the maximum size of datagrams that may be passed to [`send_datagram()`].
	///
	/// Returns `None` if datagrams are unsupported by the peer or disabled locally.
	///
	/// This may change over the lifetime of a connection according to variation in the path MTU
	/// estimate. The peer can also enforce an arbitrarily small fixed limit, but if the peer's
	/// limit is large this is guaranteed to be a little over a kilobyte at minimum.
	///
	/// Not necessarily the maximum size of received datagrams.
	///
	/// [`send_datagram()`]: Connection::send_datagram
	pub fn max_datagram_size(&self) -> Option<usize> {
		self.0.state.lock().inner.datagrams().max_size()
	}

	/// Bytes available in the outgoing datagram buffer
	///
	/// When greater than zero, calling [`send_datagram()`](Self::send_datagram) with a datagram of
	/// at most this size is guaranteed not to cause older datagrams to be dropped.
	pub fn datagram_send_buffer_space(&self) -> usize {
		self.0.state.lock().inner.datagrams().send_buffer_space()
	}

	/// The peer's UDP address
	///
	/// If `ServerConfig::migration` is `true`, clients may change addresses at will, e.g. when
	/// switching to a cellular internet connection.
	pub fn remote_address(&self) -> SocketAddr {
		self.0.state.lock().inner.remote_address()
	}

	/// Current best estimate of this connection's latency (round-trip-time)
	pub fn rtt(&self) -> Duration {
		self.0.state.lock().inner.rtt()
	}

	/// Returns connection statistics
	pub fn stats(&self) -> Stats {
		self.0.state.lock().inner.stats()
	}

	/// How many send streams the connection still holds state for
	#[cfg(test)]
	pub(crate) fn send_streams(&self) -> usize {
		self.0.state.lock().inner.streams().send_streams()
	}

	/// How many streams have `send::Stream::stopped` waiters registered, to check none leak
	#[cfg(test)]
	pub(crate) fn stopped_waiters(&self) -> usize {
		self.0.state.lock().stopped.len()
	}

	/// The wire counters the driver updates as it sends and receives.
	#[cfg(feature = "net")]
	pub(crate) fn wire(&self) -> moq_net::transport::Wire {
		self.0.shared.wire.clone()
	}

	/// The target the driver probes up to, or `None` when the congestion controller cannot probe.
	#[cfg(feature = "net")]
	pub(crate) fn probe(&self) -> Option<moq_net::transport::Probe> {
		self.0.shared.probe.clone()
	}

	/// Current state of the congestion control algorithm, for debugging purposes
	pub fn congestion_state(&self) -> Box<dyn Controller> {
		self.0.state.lock().inner.congestion_state().clone_box()
	}

	/// Parameters negotiated during the handshake
	///
	/// Guaranteed to return `Some` on fully established connections or after
	/// [`Connecting::handshake_data()`] succeeds. See that method's documentations for details on
	/// the returned value.
	///
	/// [`Connection::handshake_data()`]: crate::quic::driver::connection::Connecting::handshake_data
	pub fn handshake_data(&self) -> Option<Box<dyn Any>> {
		self.0.state.lock().inner.crypto_session().handshake_data()
	}

	/// Cryptographic identity of the peer
	///
	/// The dynamic type returned is determined by the configured
	/// [`Session`](moq_quic::crypto::Session). For the default `rustls` session, the return value can
	/// be [`downcast`](Box::downcast) to a <code>Vec<[rustls::pki_types::CertificateDer]></code>.
	///
	/// For custom asynchronous validation (e.g. checking a certificate against an external
	/// database), call this after awaiting [`Connecting`] and use [`close()`][Self::close] to
	/// reject untrusted peers.
	pub fn peer_identity(&self) -> Option<Box<dyn Any>> {
		self.0.state.lock().inner.crypto_session().peer_identity()
	}

	/// A stable identifier for this connection
	///
	/// Peer addresses and connection IDs can change, but this value will remain
	/// fixed for the lifetime of the connection.
	pub fn stable_id(&self) -> usize {
		self.0.stable_id()
	}

	/// Derive keying material from this connection's TLS session secrets.
	///
	/// When both peers call this method with the same `label` and `context`
	/// arguments and `output` buffers of equal length, they will get the
	/// same sequence of bytes in `output`. These bytes are cryptographically
	/// strong and pseudorandom, and are suitable for use as keying material.
	///
	/// See [RFC5705](https://tools.ietf.org/html/rfc5705) for more information.
	#[cfg(test)]
	pub(super) fn export_keying_material(
		&self,
		output: &mut [u8],
		label: &[u8],
		context: &[u8],
	) -> Result<(), moq_quic::crypto::ExportKeyingMaterialError> {
		self.0
			.state
			.lock()
			.inner
			.crypto_session()
			.export_keying_material(output, label, context)
	}

	/// See [`moq_quic::connection::Connection::set_limits`]
	pub fn set_limits(&self, limits: moq_quic::Limits) -> Result<(), moq_quic::LimitsError> {
		let mut conn = self.0.state.lock();
		conn.inner.set_limits(limits)?;
		// May need to send MAX_DATA or MAX_STREAMS to make progress
		conn.wake();
		Ok(())
	}
}

pin_project! {
	/// Future produced by [`Connection::open_uni`]
	struct OpenUni<'a> {
		conn: &'a ConnectionRef,
		options: send::Options<'a>,
		#[pin]
		notify: Notified<'a>,
	}
}

impl Future for OpenUni<'_> {
	type Output = Result<send::Stream, Error>;
	fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
		let this = self.project();
		let (conn, id) = ready!(poll_open(ctx, this.conn, *this.options, this.notify, Dir::Uni))?;
		Poll::Ready(Ok(send::Stream::new(conn, id)))
	}
}

pin_project! {
	/// Future produced by [`Connection::open_bi`]
	struct OpenBi<'a> {
		conn: &'a ConnectionRef,
		options: send::Options<'a>,
		#[pin]
		notify: Notified<'a>,
	}
}

impl Future for OpenBi<'_> {
	type Output = Result<(send::Stream, recv::Stream), Error>;
	fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
		let this = self.project();
		let (conn, id) = ready!(poll_open(ctx, this.conn, *this.options, this.notify, Dir::Bi))?;

		Poll::Ready(Ok((send::Stream::new(conn.clone(), id), recv::Stream::new(conn, id))))
	}
}

fn poll_open<'a>(
	ctx: &mut Context<'_>,
	conn: &'a ConnectionRef,
	options: send::Options<'_>,
	mut notify: Pin<&mut Notified<'a>>,
	dir: Dir,
) -> Poll<Result<(ConnectionRef, StreamId), Error>> {
	let options = options.to_quic(&conn.0);
	let mut state = conn.state.lock();
	if let Some(e) = &state.error {
		return Poll::Ready(Err(e.clone()));
	} else if let Some(id) = state.inner.streams().open(dir, options) {
		drop(state); // Release the lock so clone can take it
		return Poll::Ready(Ok((conn.clone(), id)));
	}
	loop {
		match notify.as_mut().poll(ctx) {
			// `state` lock ensures we didn't race with readiness
			Poll::Pending => return Poll::Pending,
			// Spurious wakeup, get a new future
			Poll::Ready(()) => notify.set(conn.shared.stream_budget_available[dir as usize].notified()),
		}
	}
}

pin_project! {
	/// Future produced by [`Connection::accept_uni`]
	struct AcceptUni<'a> {
		conn: &'a ConnectionRef,
		#[pin]
		notify: Notified<'a>,
	}
}

impl Future for AcceptUni<'_> {
	type Output = Result<recv::Stream, Error>;

	fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
		let this = self.project();
		let (conn, id) = ready!(poll_accept(ctx, this.conn, this.notify, Dir::Uni))?;
		Poll::Ready(Ok(recv::Stream::new(conn, id)))
	}
}

pin_project! {
	/// Future produced by [`Connection::accept_bi`]
	struct AcceptBi<'a> {
		conn: &'a ConnectionRef,
		#[pin]
		notify: Notified<'a>,
	}
}

impl Future for AcceptBi<'_> {
	type Output = Result<(send::Stream, recv::Stream), Error>;

	fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
		let this = self.project();
		let (conn, id) = ready!(poll_accept(ctx, this.conn, this.notify, Dir::Bi))?;
		Poll::Ready(Ok((send::Stream::new(conn.clone(), id), recv::Stream::new(conn, id))))
	}
}

fn poll_accept<'a>(
	ctx: &mut Context<'_>,
	conn: &'a ConnectionRef,
	mut notify: Pin<&mut Notified<'a>>,
	dir: Dir,
) -> Poll<Result<(ConnectionRef, StreamId), Error>> {
	let mut state = conn.state.lock();
	// Check for incoming streams before checking `state.error` so that already-received streams,
	// which are necessarily finite, can be drained from a closed connection.
	if let Some(id) = state.inner.streams().accept(dir) {
		state.wake(); // To send additional stream ID credit
		drop(state); // Release the lock so clone can take it
		return Poll::Ready(Ok((conn.clone(), id)));
	} else if let Some(e) = &state.error {
		return Poll::Ready(Err(e.clone()));
	}
	loop {
		match notify.as_mut().poll(ctx) {
			// `state` lock ensures we didn't race with readiness
			Poll::Pending => return Poll::Pending,
			// Spurious wakeup, get a new future
			Poll::Ready(()) => notify.set(conn.shared.stream_incoming[dir as usize].notified()),
		}
	}
}

pin_project! {
	/// Future produced by [`Connection::read_datagram`]
	struct ReadDatagram<'a> {
		conn: &'a ConnectionRef,
		#[pin]
		notify: Notified<'a>,
	}
}

impl Future for ReadDatagram<'_> {
	type Output = Result<Bytes, Error>;
	fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
		let mut this = self.project();
		let mut state = this.conn.state.lock();
		// Check for buffered datagrams before checking `state.error` so that already-received
		// datagrams, which are necessarily finite, can be drained from a closed connection.
		if let Some(x) = state.inner.datagrams().recv() {
			return Poll::Ready(Ok(x));
		} else if let Some(e) = &state.error {
			return Poll::Ready(Err(e.clone()));
		}
		loop {
			match this.notify.as_mut().poll(ctx) {
				// `state` lock ensures we didn't race with readiness
				Poll::Pending => return Poll::Pending,
				// Spurious wakeup, get a new future
				Poll::Ready(()) => this.notify.set(this.conn.shared.datagram_received.notified()),
			}
		}
	}
}

#[derive(Debug)]
pub(crate) struct ConnectionRef(pub(crate) Arc<ConnectionInner>);

impl ConnectionRef {
	pub(crate) fn stable_id(&self) -> usize {
		&*self.0 as *const _ as usize
	}
}

impl Clone for ConnectionRef {
	fn clone(&self) -> Self {
		self.shared.ref_count.fetch_add(1, Ordering::Relaxed);
		Self(self.0.clone())
	}
}

impl Drop for ConnectionRef {
	fn drop(&mut self) {
		if self.shared.ref_count.fetch_sub(1, Ordering::Relaxed) > 1 {
			return;
		}

		let conn = &mut *self.state.lock();

		if !conn.inner.is_closed() {
			// If the driver is alive, it's just it and us, so we'd better shut it down. If it's
			// not, we can't do any harm. If there were any streams being opened, then either
			// the connection will be closed for an unrelated reason or a fresh reference will
			// be constructed for the newly opened stream.
			conn.implicit_close(&self.shared);
		}
	}
}

impl std::ops::Deref for ConnectionRef {
	type Target = ConnectionInner;
	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

#[derive(Debug)]
pub(crate) struct ConnectionInner {
	pub(crate) state: Mutex<State>,
	pub(crate) shared: Shared,
}

#[derive(Debug, Default)]
pub(crate) struct Shared {
	/// Notified when new streams may be locally initiated due to an increase in stream ID flow
	/// control budget
	stream_budget_available: [Notify; 2],
	/// Notified when the peer has initiated a new stream
	stream_incoming: [Notify; 2],
	datagram_received: Notify,
	closed: Notify,
	connected: Arc<Notify>,
	/// Number of live handles that can used to initiate or handle I/O; excludes the driver
	ref_count: AtomicUsize,
	/// Refreshed by every driver poll, so readers never wake the driver to sample.
	///
	/// Only moq-net reads them, so a driver-only build skips the per-poll stats.
	#[cfg(feature = "net")]
	wire: moq_net::transport::Wire,
	/// The session's probe target, read by every driver poll
	#[cfg(feature = "net")]
	probe: Option<moq_net::transport::Probe>,
}

pub(crate) struct State {
	pub(crate) inner: moq_quic::connection::Connection,
	driver: Option<Waker>,
	handle: ConnectionHandle,
	on_handshake_data: Option<oneshot::Sender<()>>,
	connected: bool,
	timer: Option<moq_clock::Sleep>,
	timer_deadline: Option<Instant>,
	conn_events: mpsc::UnboundedReceiver<ConnectionEvent>,
	endpoint_events: mpsc::UnboundedSender<(ConnectionHandle, EndpointEvent)>,
	pub(crate) blocked_writers: FxHashMap<StreamId, Waker>,
	pub(crate) blocked_readers: FxHashMap<StreamId, Waker>,
	pub(crate) stopped: FxHashMap<StreamId, Arc<StopWaiters>>,
	/// Always set to Some before the connection becomes drained
	pub(crate) error: Option<Error>,
	/// The endpoint's socket, sent through without keeping it open
	slot: Arc<Slot>,
	send_buffer: Vec<u8>,
	/// We buffer a transmit when the underlying I/O would block
	buffered_transmit: Option<moq_quic::Transmit>,
}

impl State {
	fn new(
		inner: moq_quic::connection::Connection,
		handle: ConnectionHandle,
		endpoint_events: mpsc::UnboundedSender<(ConnectionHandle, EndpointEvent)>,
		conn_events: mpsc::UnboundedReceiver<ConnectionEvent>,
		on_handshake_data: oneshot::Sender<()>,
		slot: Arc<Slot>,
	) -> Self {
		Self {
			inner,
			driver: None,
			handle,
			on_handshake_data: Some(on_handshake_data),
			connected: false,
			timer: None,
			timer_deadline: None,
			conn_events,
			endpoint_events,
			blocked_writers: FxHashMap::default(),
			blocked_readers: FxHashMap::default(),
			stopped: FxHashMap::default(),
			error: None,
			slot,
			send_buffer: Vec::new(),
			buffered_transmit: None,
		}
	}

	fn drive_transmit(&mut self, cx: &mut Context<'_>) -> io::Result<bool> {
		let now = Instant::now();
		let mut transmits = 0;

		let max_datagrams = self.slot.max_transmit_segments().min(MAX_TRANSMIT_SEGMENTS);

		loop {
			// Retry the last transmit, or get a new one.
			let t = match self.buffered_transmit.take() {
				Some(t) => t,
				None => {
					self.send_buffer.clear();
					self.send_buffer.reserve(self.inner.current_mtu() as usize);
					match self.inner.poll_transmit(now, max_datagrams, &mut self.send_buffer) {
						Some(t) => {
							transmits += match t.segment_size {
								None => 1,
								Some(s) => t.size.div_ceil(s), // round up
							};
							t
						}
						None => break,
					}
				}
			};

			let len = t.size;
			match self.slot.poll_send(&udp_transmit(&t, &self.send_buffer[..len]), cx) {
				Poll::Pending => {
					self.buffered_transmit = Some(t);
					return Ok(false);
				}
				// The datagram is lost, and moq-quic decides whether the connection survives it.
				Poll::Ready(Err(e)) if moq_sock::udp::is_unreachable_err(&e) => {
					tracing::debug!(%e, "no route to the peer");
					self.inner.handle_unreachable();
				}
				Poll::Ready(Err(e)) => return Err(e),
				Poll::Ready(Ok(())) => {}
			}

			if transmits >= MAX_TRANSMIT_DATAGRAMS {
				// TODO: What isn't ideal here yet is that if we don't poll all
				// datagrams that could be sent we don't go into the `app_limited`
				// state and CWND continues to grow until we get here the next time.
				return Ok(true);
			}
		}

		Ok(false)
	}

	fn forward_endpoint_events(&mut self) {
		while let Some(event) = self.inner.poll_endpoint_events() {
			// If the endpoint driver is gone, noop.
			let _ = self.endpoint_events.send((self.handle, event));
		}
	}

	/// If this returns `Err`, the endpoint is dead, so the driver should exit immediately.
	fn process_conn_events(&mut self, shared: &Shared, cx: &mut Context<'_>) -> Result<(), Error> {
		loop {
			match self.conn_events.poll_recv(cx) {
				Poll::Ready(Some(ConnectionEvent::Rebind)) => {
					self.inner.local_address_changed();
				}
				Poll::Ready(Some(ConnectionEvent::Proto(event))) => {
					self.inner.handle_event(event);
				}
				Poll::Ready(Some(ConnectionEvent::Close { reason, error_code })) => {
					self.close(error_code, reason, shared);
				}
				Poll::Ready(None) => {
					return Err(Error::TransportError(TransportError::new(
						TransportErrorCode::INTERNAL_ERROR,
						"endpoint driver future was dropped".to_string(),
					)));
				}
				Poll::Pending => {
					return Ok(());
				}
			}
		}
	}

	fn forward_app_events(&mut self, shared: &Shared) {
		while let Some(event) = self.inner.poll() {
			use moq_quic::Event::*;
			match event {
				HandshakeDataReady => {
					if let Some(x) = self.on_handshake_data.take() {
						let _ = x.send(());
					}
				}
				Connected => {
					self.connected = true;
					shared.connected.notify_waiters();
				}

				ConnectionLost { reason } => {
					self.terminate(reason, shared);
				}
				Stream(StreamEvent::Writable { id }) => wake_stream(id, &mut self.blocked_writers),
				Stream(StreamEvent::Opened { dir: Dir::Uni }) => {
					shared.stream_incoming[Dir::Uni as usize].notify_waiters();
				}
				Stream(StreamEvent::Opened { dir: Dir::Bi }) => {
					shared.stream_incoming[Dir::Bi as usize].notify_waiters();
				}
				DatagramReceived => {
					shared.datagram_received.notify_waiters();
				}
				Stream(StreamEvent::Readable { id }) => wake_stream(id, &mut self.blocked_readers),
				Stream(StreamEvent::Available { dir }) => {
					// Might mean any number of streams are ready, so we wake up everyone
					shared.stream_budget_available[dir as usize].notify_waiters();
				}
				Stream(StreamEvent::Finished { id }) => self.resolve_stopped(id, None),
				Stream(StreamEvent::Stopped { id, error_code }) => {
					self.resolve_stopped(id, Some(error_code));
					wake_stream(id, &mut self.blocked_writers);
				}

				_ => {}
			}
		}
	}

	fn drive_timer(&mut self, cx: &mut Context<'_>) -> bool {
		let Some(deadline) = self.inner.poll_timeout() else {
			self.timer_deadline = None;
			return false;
		};

		// Use the clock rather than the async timer to detect expiry: Sleep::poll
		// respects Tokio's cooperative budget and can return Pending for elapsed
		// deadlines.
		let now = Instant::now();
		if now >= deadline {
			self.inner.handle_timeout(now);
			self.timer_deadline = None;
			return true;
		}

		match &mut self.timer {
			// Avoid resetting the timer when the deadline is unchanged.
			Some(delay) if self.timer_deadline != Some(deadline) => {
				delay.reset(deadline);
			}
			None => {
				self.timer = Some(moq_clock::sleep_until(deadline));
			}
			_ => {}
		}
		self.timer_deadline = Some(deadline);

		let delay = self.timer.as_mut().expect("timer must exist in this state");
		if Pin::new(delay).poll(cx).is_pending() {
			return false;
		}

		// The deadline elapsed in the window between the clock check and poll.
		self.inner.handle_timeout(Instant::now());
		self.timer_deadline = None;
		true
	}

	/// Wake up a blocked `Driver` task to process I/O
	pub(crate) fn wake(&mut self) {
		if let Some(x) = self.driver.take() {
			x.wake();
		}
	}

	/// Used to wake up all blocked futures when the connection becomes closed for any reason
	fn terminate(&mut self, reason: Error, shared: &Shared) {
		self.error = Some(reason.clone());
		if let Some(x) = self.on_handshake_data.take() {
			let _ = x.send(());
		}
		wake_all(&mut self.blocked_writers);
		wake_all(&mut self.blocked_readers);
		shared.stream_budget_available[Dir::Uni as usize].notify_waiters();
		shared.stream_budget_available[Dir::Bi as usize].notify_waiters();
		shared.stream_incoming[Dir::Uni as usize].notify_waiters();
		shared.stream_incoming[Dir::Bi as usize].notify_waiters();
		shared.datagram_received.notify_waiters();
		wake_all_notify(&mut self.stopped);
		shared.closed.notify_waiters();
		shared.connected.notify_waiters();
	}

	fn close(&mut self, error_code: VarInt, reason: String, shared: &Shared) {
		self.inner.close(Instant::now(), error_code, reason);
		// Fails pending operations now, with the close's own code and reason.
		self.forward_app_events(shared);
		self.wake();
	}

	/// Close for a reason other than the application's explicit request
	pub(crate) fn implicit_close(&mut self, shared: &Shared) {
		self.close(0u32.into(), String::new(), shared);
	}

	/// Resolves the stream's `stopped` waiters with the peer's STOP_SENDING code, or `None`
	pub(crate) fn resolve_stopped(&mut self, id: StreamId, outcome: Option<VarInt>) {
		if let Some(waiters) = self.stopped.remove(&id) {
			let _ = waiters.outcome.set(outcome);
			waiters.notify.notify_waiters();
		}
	}
}

impl Drop for State {
	fn drop(&mut self) {
		if !self.inner.is_drained() {
			// Ensure the endpoint can tidy up
			let _ = self.endpoint_events.send((self.handle, EndpointEvent::drained()));
		}
	}
}

impl fmt::Debug for State {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("State").field("inner", &self.inner).finish()
	}
}

fn wake_stream(stream_id: StreamId, wakers: &mut FxHashMap<StreamId, Waker>) {
	if let Some(waker) = wakers.remove(&stream_id) {
		waker.wake();
	}
}

fn wake_all(wakers: &mut FxHashMap<StreamId, Waker>) {
	wakers.drain().for_each(|(_, waker)| waker.wake())
}

/// The `send::Stream::stopped` futures waiting on one stream
#[derive(Debug, Default)]
pub(crate) struct StopWaiters {
	pub(crate) notify: Notify,
	/// What a STOP_SENDING, a delivered finish, or a local reset settled, kept here because the
	/// stream may be freed before a woken waiter polls. Once set, nothing later counts.
	pub(crate) outcome: OnceLock<Option<VarInt>>,
}

fn wake_all_notify(wakers: &mut FxHashMap<StreamId, Arc<StopWaiters>>) {
	wakers.drain().for_each(|(_, waiters)| waiters.notify.notify_waiters())
}

/// The maximum amount of datagrams which will be produced in a single `drive_transmit` call
///
/// This limits the amount of CPU resources consumed by datagram generation,
/// and allows other tasks (like receiving ACKs) to run in between.
const MAX_TRANSMIT_DATAGRAMS: usize = 20;

/// The maximum amount of datagrams that are sent in a single transmit
///
/// This can be lower than the maximum platform capabilities, to avoid excessive
/// memory allocations when calling `poll_transmit()`. Benchmarks have shown
/// that numbers around 10 are a good compromise.
const MAX_TRANSMIT_SEGMENTS: usize = 10;
