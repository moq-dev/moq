//! The endpoint: one UDP socket, many connections.

use std::{
	collections::VecDeque,
	fmt,
	future::Future,
	io::{self, IoSliceMut},
	net::{SocketAddr, SocketAddrV6},
	pin::Pin,
	sync::{
		Arc, Mutex,
		atomic::{AtomicUsize, Ordering},
	},
	task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

use bytes::BytesMut;
use moq_quic::{
	ConfigError, EndpointEvent, client,
	connection::Error as ConnectionError,
	endpoint::{ConnectionHandle, DatagramEvent, Received},
};
use moq_sock::udp::{BATCH_SIZE, RecvMeta, is_msg_size_err};
use pin_project_lite::pin_project;
use rustc_hash::{FxHashMap, FxHashSet};
use tokio::sync::{Notify, futures::Notified, mpsc};
use tracing::{Instrument, Span};

pub use super::incoming::Incoming;
pub use crate::quic::driver::runtime::AsyncUdpSocket;
use crate::quic::driver::{
	ConnectionEvent, IO_LOOP_BOUND, Instant, RECV_TIME_BOUND, VarInt,
	connection::Connecting,
	runtime::{Slot, wrap_udp_socket},
	udp_transmit,
	work_limiter::WorkLimiter,
};
pub use moq_quic::endpoint::{CidGenerator, Config, ConnectError};

/// The UDP socket an [`Endpoint`] runs on: a std [`UdpSocket`](std::net::UdpSocket), or an
/// [`AsyncUdpSocket`] of your own
#[derive(Debug)]
pub struct Socket(SocketKind);

#[derive(Debug)]
enum SocketKind {
	Std(std::net::UdpSocket),
	Custom(Box<dyn AsyncUdpSocket>),
}

impl Socket {
	/// Register the socket with the current tokio runtime
	fn into_async(self) -> io::Result<Box<dyn AsyncUdpSocket>> {
		match self.0 {
			SocketKind::Std(socket) => wrap_udp_socket(socket),
			SocketKind::Custom(socket) => Ok(socket),
		}
	}
}

impl From<std::net::UdpSocket> for Socket {
	fn from(socket: std::net::UdpSocket) -> Self {
		Self(SocketKind::Std(socket))
	}
}

impl From<Box<dyn AsyncUdpSocket>> for Socket {
	fn from(socket: Box<dyn AsyncUdpSocket>) -> Self {
		Self(SocketKind::Custom(socket))
	}
}

/// Why an [`Endpoint`] could not start or switch sockets
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The configuration is invalid.
	#[error("invalid config: {0}")]
	Config(#[from] ConfigError),
	/// The socket could not be registered or read.
	#[error("socket error: {0}")]
	Io(#[from] io::Error),
}

/// A QUIC endpoint.
///
/// An endpoint owns a single UDP socket, may host many connections, and may act as both client
/// and server for different connections. It releases the socket once the last handle drops
/// and its connections finish, or when [`close`](Self::close) completes.
///
/// May be cloned to obtain another handle to the same endpoint.
#[derive(Debug, Clone)]
pub struct Endpoint {
	pub(crate) inner: EndpointRef,
}

impl Endpoint {
	/// Run an endpoint on `socket`, serving incoming connections if [`Config::server`] is set
	///
	/// The endpoint's tasks run on the caller's tokio runtime, else moq-tokio's fallback.
	pub fn new(config: Config, socket: impl Into<Socket>) -> Result<Self, Error> {
		let runtime = crate::runtime::handle();
		let socket = socket.into().into_async()?;
		let addr = socket.local_addr()?;
		let allow_mtud = !socket.may_fragment();
		let max_receive_segments = socket.max_receive_segments();
		let max_udp_payload_size = config.max_udp_payload_size;
		let rc = EndpointRef::new(
			Slot::new(socket),
			moq_quic::endpoint::Endpoint::new(config, allow_mtud)?,
			addr,
			max_udp_payload_size,
			max_receive_segments,
			runtime.clone(),
		);
		let driver = EndpointDriver(rc.clone());
		runtime.spawn(
			async {
				if let Err(e) = driver.await {
					tracing::error!("I/O error: {}", e);
				}
			}
			.instrument(Span::current()),
		);
		Ok(Self { inner: rc })
	}

	/// Get the next incoming connection attempt from a client
	///
	/// Yields [`Incoming`]s, or `None` if the endpoint is [`close`](Self::close)d.
	/// Call [`Incoming::accept`] and await its [`Connecting`] to finish the handshake.
	pub fn accept(&self) -> impl Future<Output = Option<Incoming>> + '_ {
		Accept {
			endpoint: self,
			notify: self.inner.shared.incoming.notified(),
		}
	}

	/// Connect to `addr`, verifying it as `server_name`
	///
	/// `server_name` must be covered by the certificate presented by the server. This prevents a
	/// connection from being intercepted by an attacker with a valid certificate for some other
	/// server.
	///
	/// May fail immediately due to configuration errors, or in the future if the connection could
	/// not be established.
	pub fn connect(
		&self,
		config: &client::Config,
		addr: SocketAddr,
		server_name: &str,
	) -> Result<Connecting, ConnectError> {
		let mut endpoint = self.inner.state.lock().unwrap();
		if endpoint.driver_lost || endpoint.recv_state.connections.close.is_some() {
			return Err(ConnectError::EndpointStopping);
		}
		if addr.is_ipv6() && !endpoint.ipv6 {
			return Err(ConnectError::InvalidRemoteAddress(addr));
		}
		let addr = if endpoint.ipv6 {
			SocketAddr::V6(ensure_ipv6(addr))
		} else {
			addr
		};

		let dial = moq_quic::endpoint::Dial {
			config,
			remote: addr,
			server_name,
		};
		let (ch, conn) = endpoint.inner.connect(Instant::now(), dial)?;
		let slot = self.inner.slot.clone();
		Ok(endpoint
			.recv_state
			.connections
			.insert(ch, conn, slot, false, &self.inner.runtime))
	}

	/// Switch to a new UDP socket
	///
	/// Allows the endpoint's address to be updated live, affecting all active connections. Incoming
	/// connections and connections to servers unreachable from the new address will be lost.
	///
	/// On error, the old UDP socket is retained.
	pub fn rebind(&self, socket: impl Into<Socket>) -> Result<(), Error> {
		let socket = socket.into().into_async()?;
		let addr = socket.local_addr()?;
		let mut inner = self.inner.state.lock().unwrap();
		inner.prev_socket = self.inner.slot.replace(socket);
		inner.local_addr = addr;
		inner.ipv6 = addr.is_ipv6();

		// Update connection socket references
		for sender in inner.recv_state.connections.senders.values() {
			// Ignoring errors from dropped connections
			let _ = sender.send(ConnectionEvent::Rebind);
		}
		if let Some(driver) = inner.driver.take() {
			// Ensure the driver can register for wake-ups from the new socket
			driver.wake();
		}

		Ok(())
	}

	/// Get the local `SocketAddr` the underlying socket is bound to
	pub fn local_addr(&self) -> SocketAddr {
		self.inner.state.lock().unwrap().local_addr
	}

	/// Get the number of connections that are currently open
	#[cfg(test)]
	pub(super) fn open_connections(&self) -> usize {
		self.inner.state.lock().unwrap().inner.open_connections()
	}

	/// Close every connection and refuse new ones at once; await the returned future to wait
	/// until each has sent its close to the peer, then release the socket
	///
	/// Every connection closes with `error_code` and `reason`; see [`Connection::close()`].
	/// The wait does not sit out each connection's closing period, which only repeats the
	/// close to a peer that already has it. Dropping the future without awaiting it still
	/// closes the connections, and the socket is released once they finish.
	///
	/// [`Connection::close()`]: crate::quic::driver::connection::Connection::close
	pub fn close(&self, error_code: VarInt, reason: &str) -> impl Future<Output = ()> + Send + 'static {
		self.close_now(error_code, reason);
		let endpoint = self.clone();
		async move {
			endpoint.wait_all_draining().await;
			endpoint.inner.slot.release();
			// A rebind keeps the old socket until traffic arrives on the new one.
			endpoint.inner.state.lock().unwrap().prev_socket = None;
		}
	}

	/// Close every connection and refuse new ones, without waiting for the closes to go out
	fn close_now(&self, error_code: VarInt, reason: &str) {
		let endpoint = &mut *self.inner.state.lock().unwrap();
		endpoint.recv_state.connections.close = Some((error_code, reason.to_string()));
		for sender in endpoint.recv_state.connections.senders.values() {
			// Ignoring errors from dropped connections
			let _ = sender.send(ConnectionEvent::Close {
				error_code,
				reason: reason.to_string(),
			});
		}
		// Refuse queued attempts now, the same as ones arriving after the close, so the peer
		// hears back before the socket is released.
		let mut response_buffer = Vec::new();
		for incoming in endpoint.recv_state.incoming.drain(..) {
			response_buffer.clear();
			let transmit = endpoint.inner.refuse(incoming, &mut response_buffer);
			respond(transmit, &response_buffer, &self.inner.slot);
		}
		self.inner.shared.incoming.notify_waiters();
		if endpoint.all_draining() {
			self.inner.shared.all_draining.notify_waiters();
		}
	}

	/// Wait for all connections on the endpoint to be cleanly shut down
	#[cfg(test)]
	pub(crate) async fn wait_idle(&self) {
		loop {
			{
				let endpoint = &mut *self.inner.state.lock().unwrap();
				if endpoint.is_idle() {
					break;
				}
				// Construct future while lock is held to avoid race
				self.inner.shared.idle.notified()
			}
			.await;
		}
	}

	/// Wait until no connection on the endpoint is still active
	///
	/// A connection stops being active once its peer's close arrives, at its own close if the
	/// peer never answered the handshake, or once its closing period ends otherwise. An attempt
	/// queued for [`accept()`](Self::accept) or partway through [`Incoming::accept`] still counts
	/// as active.
	pub(crate) async fn wait_all_draining(&self) {
		loop {
			{
				let endpoint = &mut *self.inner.state.lock().unwrap();
				if endpoint.all_draining() {
					break;
				}
				// Construct future while lock is held to avoid race
				self.inner.shared.all_draining.notified()
			}
			.await;
		}
	}

	/// Set the client configuration [`dial`](Self::dial) uses, so tests need not carry it
	#[cfg(test)]
	pub(crate) fn set_default_client_config(&self, config: client::Config) {
		*self.inner.default_client.lock().unwrap() = Some(config);
	}

	/// [`connect`](Self::connect) with the [default client config](Self::set_default_client_config)
	#[cfg(test)]
	pub(crate) fn dial(&self, addr: SocketAddr, server_name: &str) -> Result<Connecting, ConnectError> {
		let config = self.inner.default_client.lock().unwrap().clone();
		self.connect(&config.expect("a default client config"), addr, server_name)
	}

	/// Wait until a connection attempt is queued for [`accept()`](Endpoint::accept)
	#[cfg(test)]
	pub(crate) async fn wait_queued(&self) {
		loop {
			{
				let endpoint = self.inner.state.lock().unwrap();
				if !endpoint.recv_state.incoming.is_empty() {
					break;
				}
				// Construct future while lock is held to avoid race
				self.inner.shared.incoming.notified()
			}
			.await;
		}
	}
}

/// A future that drives IO on an endpoint
///
/// This task functions as the switch point between the UDP socket object and the
/// `Endpoint` responsible for routing datagrams to their owning `Connection`.
/// In order to do so, it also facilitates the exchange of different types of events
/// flowing between the `Endpoint` and the tasks managing `Connection`s. As such,
/// running this task is necessary to keep the endpoint's connections running.
///
/// `EndpointDriver` futures terminate when all clones of the `Endpoint` have been dropped, or when
/// an I/O error occurs.
#[must_use = "endpoint drivers must be spawned for I/O to occur"]
#[derive(Debug)]
pub(crate) struct EndpointDriver(pub(crate) EndpointRef);

impl Future for EndpointDriver {
	type Output = Result<(), io::Error>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let mut endpoint = self.0.state.lock().unwrap();
		match endpoint.driver.as_mut() {
			Some(old_waker) => old_waker.clone_from(cx.waker()),
			None => endpoint.driver = Some(cx.waker().clone()),
		}

		let now = Instant::now();
		let mut keep_going = false;
		keep_going |= endpoint.drive_recv(cx, &self.0.slot, now)?;
		keep_going |= endpoint.handle_events(cx, &self.0.shared);

		if !endpoint.recv_state.incoming.is_empty() {
			self.0.shared.incoming.notify_waiters();
		}

		if self.0.shared.ref_count.load(Ordering::Relaxed) == 0 && endpoint.is_idle() {
			Poll::Ready(Ok(()))
		} else {
			drop(endpoint);
			// If there is more work to do schedule the endpoint task again.
			// `wake_by_ref()` is called outside the lock to minimize
			// lock contention on a multithreaded runtime.
			if keep_going {
				cx.waker().wake_by_ref();
			}
			Poll::Pending
		}
	}
}

impl Drop for EndpointDriver {
	fn drop(&mut self) {
		// Nothing receives on the socket again, so free the port even while connection handles
		// outlive the endpoint.
		self.0.slot.release();
		let endpoint = &mut *self.0.state.lock().unwrap();
		endpoint.prev_socket = None;
		endpoint.driver_lost = true;
		self.0.shared.incoming.notify_waiters();
		// Drop all outgoing channels, signaling the termination of the endpoint to the associated
		// connections.
		endpoint.recv_state.connections.senders.clear();
		endpoint.recv_state.connections.draining.clear();
		// `accept` yields nothing once the driver is lost, so queued attempts would hold off
		// `wait_all_draining` forever.
		for incoming in endpoint.recv_state.incoming.drain(..) {
			endpoint.inner.ignore(incoming);
		}
		self.0.shared.all_draining.notify_waiters();
		// Clearing the senders is what makes the endpoint idle, but a waiter already parked on
		// `idle` only re-reads that after being notified, and with the driver gone nothing else
		// can wake it. Same reason as the equivalent check in `EndpointInner::accept`.
		if endpoint.is_idle() {
			self.0.shared.idle.notify_waiters();
		}
	}
}

#[derive(Debug)]
pub(crate) struct EndpointInner {
	pub(crate) state: Mutex<State>,
	pub(crate) shared: Shared,
	/// The socket, which connections send through without keeping it open
	pub(crate) slot: Arc<Slot>,
	/// Where the endpoint's and its connections' tasks run
	pub(crate) runtime: tokio::runtime::Handle,
	/// What [`Endpoint::dial`] connects with
	#[cfg(test)]
	default_client: Mutex<Option<client::Config>>,
}

impl EndpointInner {
	pub(crate) fn accept(&self, incoming: moq_quic::endpoint::Incoming) -> Result<Connecting, ConnectionError> {
		let mut response_buffer = Vec::new();

		// Phase 1: reserve endpoint state for the connection under the lock.
		let accepting = {
			let mut state = self.state.lock().unwrap();
			let now = Instant::now();
			match state.inner.start_accept(incoming, now, &mut response_buffer) {
				Ok(accepting) => {
					state.pending_accepts += 1;
					accepting
				}
				Err(error) => {
					if let Some(transmit) = error.response {
						respond(transmit, &response_buffer, &self.slot);
					}
					return Err(error.cause);
				}
			}
		};

		// Phase 2: TLS session setup, connection construction, and first-packet handling,
		// without holding the lock.
		let accepted = accepting.accept();

		// Phase 3: register the connection, or release the reservation, under the lock.
		let mut state = self.state.lock().unwrap();
		state.pending_accepts -= 1;
		let result = match state.inner.finish_accept(accepted, &mut response_buffer) {
			Ok((handle, conn)) => {
				let driver_lost = state.driver_lost;
				Ok(state
					.recv_state
					.connections
					.insert(handle, conn, self.slot.clone(), driver_lost, &self.runtime))
			}
			Err(error) => {
				if let Some(transmit) = error.response {
					respond(transmit, &response_buffer, &self.slot);
				}
				Err(error.cause)
			}
		};
		// Failed accepts and accepts completed after driver loss cannot rely on a Drained event
		// being processed to wake idle waiters.
		if state.is_idle() {
			self.shared.idle.notify_waiters();
		}
		// The pending accept held off `wait_all_draining`, and a failed one sends no event.
		if state.all_draining() {
			self.shared.all_draining.notify_waiters();
		}
		result
	}

	pub(crate) fn refuse(&self, incoming: moq_quic::endpoint::Incoming) {
		let mut state = self.state.lock().unwrap();
		let mut response_buffer = Vec::new();
		let transmit = state.inner.refuse(incoming, &mut response_buffer);
		respond(transmit, &response_buffer, &self.slot);
	}

	#[cfg(test)]
	pub(crate) fn retry(&self, incoming: moq_quic::endpoint::Incoming) -> Result<(), moq_quic::endpoint::RetryError> {
		let mut state = self.state.lock().unwrap();
		let mut response_buffer = Vec::new();
		let transmit = state.inner.retry(incoming, &mut response_buffer)?;
		respond(transmit, &response_buffer, &self.slot);
		Ok(())
	}
}

#[derive(Debug)]
pub(crate) struct State {
	/// During an active migration, abandoned_socket receives traffic
	/// until the first packet arrives on the new socket.
	prev_socket: Option<Box<dyn AsyncUdpSocket>>,
	/// The address the current socket is bound to, kept once the socket is released
	local_addr: SocketAddr,
	inner: moq_quic::endpoint::Endpoint,
	recv_state: RecvState,
	driver: Option<Waker>,
	ipv6: bool,
	events: mpsc::UnboundedReceiver<(ConnectionHandle, EndpointEvent)>,
	driver_lost: bool,
	/// Connections in the process of being accepted
	pending_accepts: usize,
}

#[derive(Debug)]
pub(crate) struct Shared {
	incoming: Notify,
	idle: Notify,
	all_draining: Notify,
	/// Number of live handles that can be used to initiate or handle I/O; excludes the driver
	ref_count: AtomicUsize,
}

impl State {
	fn drive_recv(&mut self, cx: &mut Context<'_>, slot: &Slot, now: Instant) -> Result<bool, io::Error> {
		let get_time = Instant::now;
		self.recv_state.recv_limiter.start_cycle(get_time);
		if let Some(socket) = &self.prev_socket {
			// We don't care about the `PollProgress` from old sockets.
			let poll_res = self.recv_state.poll_socket(
				cx,
				&mut self.inner,
				&mut |cx, bufs, meta| socket.poll_recv(cx, bufs, meta),
				slot,
				now,
			);
			if poll_res.is_err() {
				self.prev_socket = None;
			}
		};
		let poll_res = self.recv_state.poll_socket(
			cx,
			&mut self.inner,
			&mut |cx, bufs, meta| slot.poll_recv(cx, bufs, meta),
			slot,
			now,
		);
		self.recv_state.recv_limiter.finish_cycle(get_time);
		let poll_res = poll_res?;
		if poll_res.received_connection_packet {
			// Traffic has arrived on self.socket, therefore there is no need for the abandoned
			// one anymore. TODO: Account for multiple outgoing connections.
			self.prev_socket = None;
		}
		Ok(poll_res.keep_going)
	}

	fn handle_events(&mut self, cx: &mut Context<'_>, shared: &Shared) -> bool {
		for _ in 0..IO_LOOP_BOUND {
			let (ch, event) = match self.events.poll_recv(cx) {
				Poll::Ready(Some(x)) => x,
				Poll::Ready(None) => unreachable!("EndpointInner owns one sender"),
				Poll::Pending => {
					return false;
				}
			};

			let connections = &mut self.recv_state.connections;
			if event.is_draining() && connections.senders.contains_key(&ch) {
				connections.draining.insert(ch);
			}
			if event.is_drained() {
				connections.senders.remove(&ch);
				connections.draining.remove(&ch);
				if self.is_idle() {
					shared.idle.notify_waiters();
				}
			}
			if (event.is_draining() || event.is_drained()) && self.all_draining() {
				shared.all_draining.notify_waiters();
			}
			let Some(event) = self.inner.handle_event(ch, event) else {
				continue;
			};
			// Ignoring errors from dropped connections that haven't yet been cleaned up
			let _ = self
				.recv_state
				.connections
				.senders
				.get_mut(&ch)
				.unwrap()
				.send(ConnectionEvent::Proto(event));
		}

		true
	}

	fn is_idle(&self) -> bool {
		self.recv_state.connections.is_empty() && self.pending_accepts == 0
	}

	/// Whether no connection is still active, nor any attempt left that could become one
	fn all_draining(&self) -> bool {
		self.recv_state.connections.all_draining() && self.pending_accepts == 0 && self.recv_state.incoming.is_empty()
	}
}

impl Drop for State {
	fn drop(&mut self) {
		for incoming in self.recv_state.incoming.drain(..) {
			self.inner.ignore(incoming);
		}
	}
}

fn respond(transmit: moq_quic::Transmit, response_buffer: &[u8], slot: &Slot) {
	// Send if there's kernel buffer space; otherwise, drop it
	//
	// As an endpoint-generated packet, we know this is an
	// immediate, stateless response to an unconnected peer,
	// one of:
	//
	// - A version negotiation response due to an unknown version
	// - A `CLOSE` due to a malformed or unwanted connection attempt
	// - A stateless reset due to an unrecognized connection
	// - A `Retry` packet due to a connection attempt when
	//   `use_retry` is set
	//
	// In each case, a well-behaved peer can be trusted to retry a
	// few times, which is guaranteed to produce the same response
	// from us. Repeated failures might at worst cause a peer's new
	// connection attempt to time out, which is acceptable if we're
	// under such heavy load that there's never room for this code
	// to transmit. This is morally equivalent to the packet getting
	// lost due to congestion further along the link, which
	// similarly relies on peer retries for recovery.

	// Copied from rust 1.85's std::task::Waker::noop() implementation for backwards compatibility
	const NOOP: RawWaker = {
		const VTABLE: RawWakerVTable = RawWakerVTable::new(
			// Cloning just returns a new no-op raw waker
			|_| NOOP,
			// `wake` does nothing
			|_| {},
			// `wake_by_ref` does nothing
			|_| {},
			// Dropping does nothing as we don't allocate anything
			|_| {},
		);
		RawWaker::new(std::ptr::null(), &VTABLE)
	};
	// SAFETY: Copied from rust stdlib, the NOOP waker is thread-safe and doesn't violate the RawWakerVTable contract,
	// it doesn't access the data pointer at all.
	let waker = unsafe { Waker::from_raw(NOOP) };
	let mut cx = Context::from_waker(&waker);
	_ = slot.poll_send(&udp_transmit(&transmit, &response_buffer[..transmit.size]), &mut cx);
}

#[inline]
fn proto_ecn(ecn: moq_sock::udp::EcnCodepoint) -> moq_quic::EcnCodepoint {
	match ecn {
		moq_sock::udp::EcnCodepoint::Ect0 => moq_quic::EcnCodepoint::Ect0,
		moq_sock::udp::EcnCodepoint::Ect1 => moq_quic::EcnCodepoint::Ect1,
		moq_sock::udp::EcnCodepoint::Ce => moq_quic::EcnCodepoint::Ce,
	}
}

#[derive(Debug)]
struct ConnectionSet {
	/// Senders for communicating with the endpoint's connections
	senders: FxHashMap<ConnectionHandle, mpsc::UnboundedSender<ConnectionEvent>>,
	/// The connections in `senders` that are no longer active: closed by the peer, or past
	/// their closing period
	draining: FxHashSet<ConnectionHandle>,
	/// Stored to give out clones to new ConnectionInners
	sender: mpsc::UnboundedSender<(ConnectionHandle, EndpointEvent)>,
	/// Set if the endpoint has been manually closed
	close: Option<(VarInt, String)>,
}

impl ConnectionSet {
	fn insert(
		&mut self,
		handle: ConnectionHandle,
		conn: moq_quic::connection::Connection,
		slot: Arc<Slot>,
		driver_lost: bool,
		runtime: &tokio::runtime::Handle,
	) -> Connecting {
		let (send, recv) = mpsc::unbounded_channel();
		if let Some((error_code, reason)) = &self.close {
			send.send(ConnectionEvent::Close {
				error_code: *error_code,
				reason: reason.clone(),
			})
			.unwrap();
		}
		match driver_lost {
			// Close the event channel before spawning the connection driver, so it cannot
			// transmit a handshake after the endpoint driver has stopped.
			true => drop(send),
			false => {
				self.senders.insert(handle, send);
			}
		}
		Connecting::new(handle, conn, self.sender.clone(), recv, slot, runtime)
	}

	fn is_empty(&self) -> bool {
		self.senders.is_empty()
	}

	/// Whether no connection is still active
	fn all_draining(&self) -> bool {
		self.senders.len() == self.draining.len()
	}
}

fn ensure_ipv6(x: SocketAddr) -> SocketAddrV6 {
	match x {
		SocketAddr::V6(x) => x,
		SocketAddr::V4(x) => SocketAddrV6::new(x.ip().to_ipv6_mapped(), x.port(), 0, 0),
	}
}

pin_project! {
	/// Future produced by [`Endpoint::accept`]
	struct Accept<'a> {
		endpoint: &'a Endpoint,
		#[pin]
		notify: Notified<'a>,
	}
}

impl Future for Accept<'_> {
	type Output = Option<Incoming>;
	fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
		let mut this = self.project();
		let mut endpoint = this.endpoint.inner.state.lock().unwrap();
		if endpoint.driver_lost {
			return Poll::Ready(None);
		}
		if let Some(incoming) = endpoint.recv_state.incoming.pop_front() {
			// A handed-out attempt no longer holds off `wait_all_draining`, and refusing,
			// ignoring, or dropping it sends no event that would wake the waiter later.
			if endpoint.all_draining() {
				this.endpoint.inner.shared.all_draining.notify_waiters();
			}
			// Release the mutex lock on endpoint so cloning it doesn't deadlock
			drop(endpoint);
			let incoming = Incoming::new(incoming, this.endpoint.inner.clone());
			return Poll::Ready(Some(incoming));
		}
		if endpoint.recv_state.connections.close.is_some() {
			return Poll::Ready(None);
		}
		loop {
			match this.notify.as_mut().poll(ctx) {
				// `state` lock ensures we didn't race with readiness
				Poll::Pending => return Poll::Pending,
				// Spurious wakeup, get a new future
				Poll::Ready(()) => this.notify.set(this.endpoint.inner.shared.incoming.notified()),
			}
		}
	}
}

#[derive(Debug)]
pub(crate) struct EndpointRef(Arc<EndpointInner>);

impl EndpointRef {
	pub(crate) fn new(
		slot: Slot,
		inner: moq_quic::endpoint::Endpoint,
		local_addr: SocketAddr,
		max_udp_payload_size: u16,
		max_receive_segments: usize,
		runtime: tokio::runtime::Handle,
	) -> Self {
		let (sender, events) = mpsc::unbounded_channel();
		let recv_state = RecvState::new(sender, max_receive_segments, max_udp_payload_size);
		Self(Arc::new(EndpointInner {
			slot: Arc::new(slot),
			runtime,
			#[cfg(test)]
			default_client: Mutex::new(None),
			shared: Shared {
				incoming: Notify::new(),
				idle: Notify::new(),
				all_draining: Notify::new(),
				ref_count: AtomicUsize::new(0),
			},
			state: Mutex::new(State {
				prev_socket: None,
				local_addr,
				inner,
				ipv6: local_addr.is_ipv6(),
				events,
				driver: None,
				driver_lost: false,
				recv_state,
				pending_accepts: 0,
			}),
		}))
	}
}

impl Clone for EndpointRef {
	fn clone(&self) -> Self {
		self.0.shared.ref_count.fetch_add(1, Ordering::Relaxed);
		Self(self.0.clone())
	}
}

impl Drop for EndpointRef {
	fn drop(&mut self) {
		if self.shared.ref_count.fetch_sub(1, Ordering::Relaxed) > 1 {
			return;
		}

		let endpoint = &mut *self.0.state.lock().unwrap();
		// If the driver is about to be on its own, ensure it can shut down if the last
		// connection is gone.
		if let Some(task) = endpoint.driver.take() {
			task.wake();
		}
	}
}

impl std::ops::Deref for EndpointRef {
	type Target = EndpointInner;
	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

/// A receive from one of the endpoint's sockets, as [`AsyncUdpSocket::poll_recv`]
type PollRecv<'a> = dyn FnMut(&mut Context<'_>, &mut [IoSliceMut<'_>], &mut [RecvMeta]) -> Poll<io::Result<usize>> + 'a;

/// State directly involved in handling incoming packets
struct RecvState {
	incoming: VecDeque<moq_quic::endpoint::Incoming>,
	connections: ConnectionSet,
	recv_buf: Box<[u8]>,
	/// Reused buffer that received datagrams are copied into and split into owned slices.
	datagrams: BytesMut,
	recv_limiter: WorkLimiter,
}

impl RecvState {
	fn new(
		sender: mpsc::UnboundedSender<(ConnectionHandle, EndpointEvent)>,
		max_receive_segments: usize,
		max_udp_payload_size: u16,
	) -> Self {
		let datagram_capacity = usize::from(max_udp_payload_size) * max_receive_segments;
		let recv_buf = vec![0; datagram_capacity * BATCH_SIZE];
		Self {
			connections: ConnectionSet {
				senders: FxHashMap::default(),
				draining: FxHashSet::default(),
				sender,
				close: None,
			},
			incoming: VecDeque::new(),
			recv_buf: recv_buf.into(),
			datagrams: BytesMut::with_capacity(datagram_capacity),
			recv_limiter: WorkLimiter::new(RECV_TIME_BOUND),
		}
	}

	fn poll_socket(
		&mut self,
		cx: &mut Context<'_>,
		endpoint: &mut moq_quic::endpoint::Endpoint,
		recv: &mut PollRecv<'_>,
		slot: &Slot,
		now: Instant,
	) -> Result<PollProgress, io::Error> {
		let mut received_connection_packet = false;
		let mut metas = [RecvMeta::default(); BATCH_SIZE];
		let mut iovs: [IoSliceMut<'_>; BATCH_SIZE] = {
			let mut bufs = self
				.recv_buf
				.chunks_mut(self.recv_buf.len() / BATCH_SIZE)
				.map(IoSliceMut::new);

			// expect() safe as self.recv_buf is chunked into BATCH_SIZE items
			// and iovs will be of size BATCH_SIZE, thus from_fn is called
			// exactly BATCH_SIZE times.
			std::array::from_fn(|_| bufs.next().expect("BATCH_SIZE elements"))
		};
		loop {
			match recv(cx, &mut iovs, &mut metas) {
				Poll::Ready(Ok(msgs)) => {
					self.recv_limiter.record_work(msgs);
					// Copy the received batch into `datagrams` and split each datagram off as
					// an owned slice. We can't read directly into `datagrams` and drop
					// `recv_buf`: once a slice has been handed to a connection the allocation
					// is shared, so the next `reserve` must allocate fresh backing memory.
					// Reading in place would therefore mean reserving the worst-case iovec
					// envelope before every poll and reallocating it whenever a prior datagram
					// is still in flight. `recv_buf` stays an allocate-once scratch buffer, and
					// we reserve only the bytes actually received.
					let batch_len = metas.iter().take(msgs).map(|meta| meta.len).sum();
					self.datagrams.reserve(batch_len);
					for (meta, buf) in metas.iter().zip(iovs.iter()).take(msgs) {
						self.datagrams.extend_from_slice(&buf[..meta.len]);
					}
					for meta in metas.iter().take(msgs) {
						let mut data = self.datagrams.split_to(meta.len);
						while !data.is_empty() {
							let buf = data.split_to(meta.stride.min(data.len()));
							let mut response_buffer = Vec::new();
							let received = Received {
								remote: meta.addr,
								local_ip: meta.dst_ip,
								ecn: meta.ecn.map(proto_ecn),
								data: buf,
							};
							match endpoint.handle(now, received, &mut response_buffer) {
								Some(DatagramEvent::NewConnection(incoming)) => {
									if self.connections.close.is_none() {
										self.incoming.push_back(incoming);
									} else {
										let transmit = endpoint.refuse(incoming, &mut response_buffer);
										respond(transmit, &response_buffer, slot);
									}
								}
								Some(DatagramEvent::ConnectionEvent(handle, event)) => {
									// Ignoring errors from dropped connections that haven't yet been cleaned up
									received_connection_packet = true;
									let _ = self
										.connections
										.senders
										.get_mut(&handle)
										.unwrap()
										.send(ConnectionEvent::Proto(event));
								}
								Some(DatagramEvent::Response(transmit)) => {
									respond(transmit, &response_buffer, slot);
								}
								None => {}
							}
						}
					}
				}
				Poll::Pending => {
					return Ok(PollProgress {
						received_connection_packet,
						keep_going: false,
					});
				}
				// Ignore ECONNRESET as it's undefined in QUIC and may be injected by an
				// attacker
				Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::ConnectionReset => {
					self.recv_limiter.record_work(1);
				}
				// Ignore EMSGSIZE as we're currently not handling ICMPv4 Fragmentation Needed
				// and ICMPv6 Packet Too Big (PTB) messages since they cannot be authenticated,
				// and Datagram Packetization Layer Path MTU Discovery (DPLPMTUD) works without
				// it anyways.
				Poll::Ready(Err(e)) if is_msg_size_err(&e) => {
					self.recv_limiter.record_work(1);
				}
				Poll::Ready(Err(e)) => {
					return Err(e);
				}
			}
			if !self.recv_limiter.allow_work(Instant::now) {
				return Ok(PollProgress {
					received_connection_packet,
					keep_going: true,
				});
			}
		}
	}
}

impl fmt::Debug for RecvState {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("RecvState")
			.field("incoming", &self.incoming)
			.field("connections", &self.connections)
			// recv_buf too large
			.field("recv_limiter", &self.recv_limiter)
			.finish_non_exhaustive()
	}
}

#[derive(Default)]
struct PollProgress {
	/// Whether a datagram was routed to an existing connection
	received_connection_packet: bool,
	/// Whether datagram handling was interrupted early by the work limiter for fairness
	keep_going: bool,
}

#[cfg(all(test, any(unix, windows), any(feature = "ring", feature = "aws-lc-rs")))]
mod tests {
	use std::net::UdpSocket;

	use moq_sock::udp::Transmit;

	use super::*;

	#[test]
	fn ignored_receive_errors_respect_work_budget() {
		let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
		let size_error = socket
			.send_to(&vec![0; 65_536], socket.local_addr().unwrap())
			.unwrap_err();
		assert!(is_msg_size_err(&size_error));

		for error in [io::ErrorKind::ConnectionReset.into(), size_error] {
			let mut endpoint = moq_quic::endpoint::Endpoint::new(Config::default(), true).unwrap();
			let (tx, _rx) = mpsc::unbounded_channel();
			let mut state = RecvState::new(tx, 1, Config::default().max_udp_payload_size);
			let now = Instant::now();

			// Calibrate the next receive cycles to allow two work items each.
			state.recv_limiter.start_cycle(|| now);
			state.recv_limiter.record_work(2);
			state.recv_limiter.finish_cycle(|| now + RECV_TIME_BOUND);

			let socket = ErrorSocket {
				error,
				remaining: AtomicUsize::new(5),
			};
			let slot = Slot::new(Box::new(UnusedSocket));
			let mut cx = Context::from_waker(Waker::noop());
			for (remaining, keep_going) in [(3, true), (1, true), (0, false)] {
				state.recv_limiter.start_cycle(|| now);
				let progress = state
					.poll_socket(
						&mut cx,
						&mut endpoint,
						&mut |cx, bufs, meta| socket.poll_recv(cx, bufs, meta),
						&slot,
						now,
					)
					.unwrap();
				state.recv_limiter.finish_cycle(|| now);
				assert_eq!(socket.remaining.load(Ordering::Relaxed), remaining);
				assert_eq!(progress.keep_going, keep_going);
				assert!(!progress.received_connection_packet);
			}
		}
	}

	#[derive(Debug)]
	struct ErrorSocket {
		error: io::Error,
		remaining: AtomicUsize,
	}

	impl ErrorSocket {
		fn poll_recv(
			&self,
			_cx: &mut Context<'_>,
			_bufs: &mut [IoSliceMut<'_>],
			_meta: &mut [RecvMeta],
		) -> Poll<io::Result<usize>> {
			if self.remaining.load(Ordering::Relaxed) == 0 {
				return Poll::Pending;
			}
			self.remaining.fetch_sub(1, Ordering::Relaxed);
			Poll::Ready(Err(match self.error.raw_os_error() {
				Some(code) => io::Error::from_raw_os_error(code),
				None => self.error.kind().into(),
			}))
		}
	}

	/// A socket the test never sends a response through
	#[derive(Debug)]
	struct UnusedSocket;

	impl AsyncUdpSocket for UnusedSocket {
		fn poll_recv(
			&self,
			_cx: &mut Context<'_>,
			_bufs: &mut [IoSliceMut<'_>],
			_meta: &mut [RecvMeta],
		) -> Poll<io::Result<usize>> {
			Poll::Pending
		}

		fn try_send(&self, _transmit: &Transmit<'_>) -> io::Result<()> {
			unreachable!("ignored receive errors must not produce responses")
		}

		fn poll_writable(&self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
			unreachable!("ignored receive errors must not produce responses")
		}

		fn local_addr(&self) -> io::Result<SocketAddr> {
			Ok("127.0.0.1:0".parse().unwrap())
		}
	}
}
