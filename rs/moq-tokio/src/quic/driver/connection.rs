//! A single connection: its streams, datagrams, and why it ends.

use std::{
	any::Any,
	fmt,
	future::Future,
	io,
	net::SocketAddr,
	pin::Pin,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
	task::{Context, Poll, ready},
};

use bytes::{Buf, Bytes};
use tokio::sync::mpsc;
use tracing::{Instrument, Span, debug_span};

use crate::quic::driver::{
	ConnectionEvent, Duration, Instant, VarInt, mutex::Mutex, recv, runtime::Slot, send, udp_transmit,
};
use moq_congestion::Controller;
pub use moq_quic::connection::{Error, Stats};
use moq_quic::{
	EndpointEvent, TransportError, TransportErrorCode,
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
	/// Holds the waiter of [`Future::poll`] across polls
	park: kio::Park,
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
		let shared = Shared {
			#[cfg(feature = "net")]
			probe: conn.congestion_state().probes().then(moq_net::transport::Probe::new),
			..Shared::default()
		};

		let conn = ConnectionRef(Arc::new(ConnectionInner {
			state: Mutex::new(State::new(conn, handle, endpoint_events, conn_events, slot)),
			shared,
		}));

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
			park: kio::Park::default(),
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
		let conn = self.conn.as_ref().unwrap();
		kio::wait(|waiter| {
			let mut state = conn.state.lock();
			if let Err(err) = ready!(state.inner.poll_handshake_data(waiter)) {
				return Poll::Ready(Err(state.lost_as(err)));
			}
			let data = state.inner.crypto_session().handshake_data();
			Poll::Ready(data.ok_or_else(|| state.lost()))
		})
		.await
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
		let this = &mut *self;
		let waiter = this.park.hold(cx);
		let conn = this.conn.as_ref().expect("used after yielding Ready");
		let mut state = conn.state.lock();
		let connected = ready!(state.inner.poll_connected(waiter)).map_err(|err| state.lost_as(err));
		drop(state);
		let conn = this.conn.take().unwrap();
		Poll::Ready(connected.map(|()| Connection(conn)))
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
	/// Holds the waiter `poll_transmit` parks on, which the application's calls wake
	park: kio::Park,
}

impl ConnectionDriver {
	fn new(conn: ConnectionRef) -> Self {
		let span = {
			let conn = &mut conn.state.lock();
			debug_span!("drive", id = conn.handle.0)
		};

		Self {
			conn,
			span,
			park: kio::Park::default(),
		}
	}
}

impl Future for ConnectionDriver {
	type Output = Result<(), io::Error>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let this = self.get_mut();
		let waiter = this.park.hold(cx);
		let conn = &mut *this.conn.state.lock();
		let _guard = this.span.enter();

		if let Err(e) = conn.process_conn_events(cx) {
			conn.terminate(e);
			return Poll::Ready(Ok(()));
		}
		// A new target waits for the next poll: probing only resends data in flight, so it has
		// nothing to send until a write or an ACK polls anyway.
		#[cfg(feature = "net")]
		if let Some(probe) = &this.conn.shared.probe {
			conn.inner.set_probe_target(probe.target().as_bps() / 8);
		}
		let mut keep_going = conn.drive_transmit(waiter, cx).inspect_err(|_| {
			// Transmit failed, so close the connection and clean state before returning the error.
			if !conn.inner.is_closed() {
				conn.implicit_close();
			}
		})?;
		// If a timer expires, there might be more to transmit. When we transmit something, we
		// might need to reset a timer. Hence, we must loop until neither happens.
		keep_going |= conn.drive_timer(cx);
		conn.forward_endpoint_events();
		conn.drain_events();

		// Every datagram sent or received passes through this poll.
		#[cfg(feature = "net")]
		{
			let stats = conn.inner.stats();
			this.conn.shared.wire.set_sent_bytes(stats.udp_tx.bytes);
			this.conn.shared.wire.set_received_bytes(stats.udp_rx.bytes);
		}

		if !conn.inner.is_drained() {
			// A local close still reaches a driver whose socket holds output back, which skips
			// `poll_transmit` and so parks nothing there, to re-arm the timer for its closing
			// period. Waiting on the close consumes no transmit.
			let _ = conn.inner.poll_closed(waiter);
			// Otherwise `poll_transmit` parked the waiter for the application's next call, and the
			// ingress channel, the timer, and the socket hold the context.
			if keep_going {
				// If the connection hasn't processed all tasks, schedule it again
				cx.waker().wake_by_ref();
			}
			return Poll::Pending;
		}
		if !conn.is_lost() {
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
	pub async fn open_uni(&self, options: send::Options<'_>) -> Result<send::Stream, Error> {
		let (conn, id) = kio::wait(|waiter| poll_open(waiter, &self.0, options, Dir::Uni)).await?;
		Ok(send::Stream::new(conn, id))
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
	pub async fn open_bi(&self, options: send::Options<'_>) -> Result<(send::Stream, recv::Stream), Error> {
		let (conn, id) = kio::wait(|waiter| poll_open(waiter, &self.0, options, Dir::Bi)).await?;
		Ok((send::Stream::new(conn.clone(), id), recv::Stream::new(conn, id)))
	}

	/// Open a request stream using only the credit already granted by the peer.
	#[cfg(feature = "net")]
	pub(crate) fn open_request(&self) -> Result<Option<(send::Stream, recv::Stream)>, Error> {
		let mut state = self.0.state.lock();
		if let Some(err) = state.error() {
			return Err(err);
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
	pub async fn accept_uni(&self) -> Result<recv::Stream, Error> {
		let (conn, id) = kio::wait(|waiter| poll_accept(waiter, &self.0, Dir::Uni)).await?;
		Ok(recv::Stream::new(conn, id))
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
	pub async fn accept_bi(&self) -> Result<(send::Stream, recv::Stream), Error> {
		let (conn, id) = kio::wait(|waiter| poll_accept(waiter, &self.0, Dir::Bi)).await?;
		Ok((send::Stream::new(conn.clone(), id), recv::Stream::new(conn, id)))
	}

	/// Receive an application datagram
	///
	/// # Cancel safety
	///
	/// This method is cancellation safe. If this does not resolve, no datagrams were consumed.
	pub fn read_datagram(&self) -> impl Future<Output = Result<Bytes, Error>> + '_ {
		kio::wait(|waiter| {
			let mut state = self.0.state.lock();
			// moq-quic yields the datagrams received before checking for the connection error,
			// so they, which are necessarily finite, drain from a closed connection.
			let datagram = ready!(state.inner.datagrams().poll_recv(waiter));
			Poll::Ready(datagram.map_err(|err| state.lost_as(err)))
		})
	}

	/// Wait for the connection to be closed for any reason
	///
	/// Despite the return type's name, closed connections are often not an error condition at the
	/// application layer. Cases that might be routine include [`Error::LocallyClosed`]
	/// and [`Error::ApplicationClosed`].
	pub async fn closed(&self) -> Error {
		kio::wait(|waiter| {
			let mut state = self.0.state.lock();
			let err = ready!(state.inner.poll_closed(waiter));
			Poll::Ready(state.lost_as(err))
		})
		.await
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
		self.0.state.lock().error()
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
		conn.close(error_code, reason.to_string());
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
		if let Some(x) = conn.error() {
			return Err(send::Error::Connection(x));
		}
		match conn.inner.datagrams().send(data, options) {
			Ok(()) => Ok(()),
			Err(moq_quic::send::Error::TooLarge) => Err(send::Error::TooLarge),
			Err(_) => Err(send::Error::Unsupported),
		}
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

	/// Whether send stream `id` was released, so moq-quic no longer keeps how it ended
	#[cfg(test)]
	pub(crate) fn send_released(&self, id: StreamId) -> bool {
		let mut state = self.0.state.lock();
		matches!(state.inner.send_stream(id).end(), Err(moq_quic::send::Error::Closed))
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
		self.0.state.lock().inner.set_limits(limits)
	}
}

/// Open a stream in `dir`, or wait for the peer to allow another
fn poll_open(
	waiter: &kio::Waiter,
	conn: &ConnectionRef,
	options: send::Options<'_>,
	dir: Dir,
) -> Poll<Result<(ConnectionRef, StreamId), Error>> {
	let options = options.to_quic(&conn.0);
	let mut state = conn.state.lock();
	let id = match ready!(state.inner.streams().poll_open(waiter, dir, options)) {
		Ok(id) => id,
		Err(err) => return Poll::Ready(Err(state.lost_as(err))),
	};
	drop(state); // Release the lock so clone can take it
	Poll::Ready(Ok((conn.clone(), id)))
}

/// Accept the peer's next stream in `dir`, or wait for one
fn poll_accept(waiter: &kio::Waiter, conn: &ConnectionRef, dir: Dir) -> Poll<Result<(ConnectionRef, StreamId), Error>> {
	let mut state = conn.state.lock();
	// moq-quic yields the streams received before checking for the connection error, so they,
	// which are necessarily finite, drain from a closed connection.
	let id = match ready!(state.inner.streams().poll_accept(waiter, dir)) {
		Ok(id) => id,
		Err(err) => return Poll::Ready(Err(state.lost_as(err))),
	};
	drop(state); // Release the lock so clone can take it
	Poll::Ready(Ok((conn.clone(), id)))
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
			conn.implicit_close();
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
	handle: ConnectionHandle,
	timer: Option<kio::clock::Sleep>,
	timer_deadline: Option<Instant>,
	conn_events: mpsc::UnboundedReceiver<ConnectionEvent>,
	endpoint_events: mpsc::UnboundedSender<(ConnectionHandle, EndpointEvent)>,
	/// Why the connection failed, when moq-quic did not see it fail: the endpoint driver went away
	///
	/// moq-quic takes no foreign error, so this wins over its reason when handles report the loss.
	external: Option<Error>,
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
		slot: Arc<Slot>,
	) -> Self {
		Self {
			inner,
			handle,
			timer: None,
			timer_deadline: None,
			conn_events,
			endpoint_events,
			external: None,
			slot,
			send_buffer: Vec::new(),
			buffered_transmit: None,
		}
	}

	/// Send what moq-quic has, parking `waiter` until the application queues more once it has
	/// nothing, or `cx` on the socket once it is full
	fn drive_transmit(&mut self, waiter: &kio::Waiter, cx: &mut Context<'_>) -> io::Result<bool> {
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
					match self
						.inner
						.poll_transmit(waiter, now, max_datagrams, &mut self.send_buffer)
					{
						Poll::Ready(t) => {
							transmits += match t.segment_size {
								None => 1,
								Some(s) => t.size.div_ceil(s), // round up
							};
							t
						}
						Poll::Pending => break,
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
	fn process_conn_events(&mut self, cx: &mut Context<'_>) -> Result<(), Error> {
		loop {
			match self.conn_events.poll_recv(cx) {
				Poll::Ready(Some(ConnectionEvent::Rebind)) => {
					self.inner.local_address_changed();
				}
				Poll::Ready(Some(ConnectionEvent::Proto(event))) => {
					self.inner.handle_event(event);
				}
				Poll::Ready(Some(ConnectionEvent::Close { reason, error_code })) => {
					self.close(error_code, reason);
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

	/// Drain moq-quic's events, which no handle waits on: moq-quic wakes its waiters itself.
	fn drain_events(&mut self) {
		while self.inner.poll().is_some() {}
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
				self.timer = Some(kio::clock::sleep_until(deadline));
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

	/// Why the connection was lost, if it was: the driver's external reason, or moq-quic's
	pub(crate) fn error(&self) -> Option<Error> {
		self.external.clone().or_else(|| self.inner.error().cloned())
	}

	/// Whether the connection was lost, without cloning why
	pub(crate) fn is_lost(&self) -> bool {
		self.external.is_some() || self.inner.error().is_some()
	}

	/// The error handles report for a loss moq-quic reported as `err`
	fn lost_as(&self, err: Error) -> Error {
		self.external.clone().unwrap_or(err)
	}

	/// The error handles report for a loss moq-quic reported without a reason, as a stream's
	/// `ConnectionLost`
	pub(crate) fn lost(&self) -> Error {
		self.error().expect("moq-quic reported a loss without a reason")
	}

	/// Fail the connection for `reason`, which moq-quic did not see: the endpoint driver went away
	///
	/// The reason is kept to win over moq-quic's, and a local close makes moq-quic wake every
	/// waiter. The driver exits right after, so the close is never sent.
	fn terminate(&mut self, reason: Error) {
		let message = reason.to_string();
		self.external = Some(reason);
		self.inner.close(Instant::now(), 0u32.into(), message);
	}

	fn close(&mut self, error_code: VarInt, reason: String) {
		// moq-quic fails pending operations now, with the close's own code and reason.
		self.inner.close(Instant::now(), error_code, reason);
	}

	/// Close for a reason other than the application's explicit request
	pub(crate) fn implicit_close(&mut self) {
		self.close(0u32.into(), String::new());
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
