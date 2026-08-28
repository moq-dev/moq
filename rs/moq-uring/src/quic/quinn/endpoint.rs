//! The quinn endpoint: one socket, many quinn-proto connections.
//!
//! quinn-proto's own [`quinn_proto::Endpoint`] is the routing table: it parses
//! each datagram, hands it to the connection its destination id names, mints
//! and retires ids as peers consume them, and answers an unsupported version
//! itself. What is left for us is the socket, the accept backlog, and a driver
//! task per connection. Everything policy-shaped (the backlog, the shard
//! steering, the id length) lives in [`super::super::endpoint`], which is
//! shared with the other backend.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::task::Poll;
use std::time::Instant;

use bytes::BytesMut;
use quinn_proto::{ConnectionHandle, DatagramEvent, Incoming, Transmit};
use rustc_hash::FxHashMap;

use super::super::{Error, endpoint::Config};
use super::connection;
use crate::quic::Connection;
use crate::{Handle, udp};

/// The accept side: connections whose handshake finished and nobody has
/// claimed yet.
struct Accepting {
	queue: VecDeque<Connection>,
}

/// State shared by the handles, the demux task, and the per-connection
/// teardown tasks.
pub(crate) struct Inner {
	handle: Handle,
	socket: Rc<udp::Socket>,
	local: SocketAddr,
	/// The routing table, and the server configuration it accepts with.
	endpoint: RefCell<quinn_proto::Endpoint>,
	accepting: RefCell<Option<Accepting>>,
	/// Every live connection, looked up per received datagram.
	///
	/// FxHash rather than SipHash: quinn-proto hands out the handle, and it
	/// owns connection-id routing (and its hasher choices) itself, so nothing
	/// a peer picks reaches this map.
	conns: RefCell<FxHashMap<ConnectionHandle, connection::Shared>>,
	/// Incoming handshakes in flight. Together with the accept queue this is
	/// bounded by [`Config::backlog`].
	pending: Cell<usize>,
	backlog: usize,
	/// Live [`Endpoint`] handles; at zero the endpoint winds down (no new
	/// accepts or dials, existing connections served until they end).
	handles: Cell<usize>,
	/// The socket's terminal error, if it died under us.
	closed: RefCell<Option<Error>>,
	/// Wakes the demux task to re-check its exit condition.
	task_waiters: RefCell<kio::WaiterList>,
	accept_waiters: RefCell<kio::WaiterList>,
}

/// A QUIC endpoint on one worker socket: accept, dial, or both.
///
/// Created by [`new`](Endpoint::new); clones share the endpoint. Dropping the
/// last handle stops new accepts and dials, but connections already
/// established (or still handshaking) are served until they end; the socket
/// closes when the last one does.
pub struct Endpoint {
	inner: Rc<Inner>,
}

impl Endpoint {
	/// Serve `socket` on the worker behind `handle`.
	pub fn new(handle: &Handle, socket: udp::Socket, config: Config) -> Result<Self, Error> {
		let local = socket.local_addr().map_err(|err| Error::Io(err.to_string()))?;
		let server = match &config.server {
			Some(server) => {
				let mut server = super::server_config(server)?;
				// quinn buffers half-open handshakes itself, so it enforces
				// the same bound the accept queue does.
				server.max_incoming(config.backlog);
				Some(Arc::new(server))
			}
			None => None,
		};
		let accepting = server.is_some().then(|| Accepting { queue: VecDeque::new() });
		// MTU discovery is off (the GSO pool sends fixed SEGMENT datagrams),
		// so the endpoint has no reason to allow it either.
		let endpoint = quinn_proto::Endpoint::new(super::endpoint_config(config.shard)?, server, false, None);

		let inner = Rc::new(Inner {
			handle: handle.clone(),
			socket: Rc::new(socket),
			local,
			endpoint: RefCell::new(endpoint),
			accepting: RefCell::new(accepting),
			conns: RefCell::new(FxHashMap::default()),
			pending: Cell::new(0),
			backlog: config.backlog,
			handles: Cell::new(1),
			closed: RefCell::new(None),
			task_waiters: RefCell::new(kio::WaiterList::new()),
			accept_waiters: RefCell::new(kio::WaiterList::new()),
		});

		let task = inner.clone();
		handle.spawn(async move { kio::wait(|waiter| task.poll_run(waiter)).await });

		Ok(Self { inner })
	}

	/// The socket's bound local address.
	pub fn local_addr(&self) -> SocketAddr {
		self.inner.local
	}

	/// The next handshaken incoming connection.
	///
	/// Fails immediately on an endpoint built without a
	/// [`server`](Config::server) configuration, and with the socket's error
	/// once the endpoint has died.
	pub async fn accept(&self) -> Result<Connection, Error> {
		kio::wait(|waiter| {
			if let Some(err) = &*self.inner.closed.borrow() {
				return Poll::Ready(Err(err.clone()));
			}
			let mut accepting = self.inner.accepting.borrow_mut();
			let Some(accepting) = accepting.as_mut() else {
				return Poll::Ready(Err(Error::NotServer));
			};
			if let Some(conn) = accepting.queue.pop_front() {
				return Poll::Ready(Ok(conn));
			}
			waiter.register(&mut self.inner.accept_waiters.borrow_mut());
			Poll::Pending
		})
		.await
	}

	/// Dial [`Config::peer`](crate::quic::client::Config::peer) through this
	/// endpoint's socket, driving the handshake to completion.
	pub async fn connect(&self, config: &crate::quic::client::Config) -> Result<Connection, Error> {
		if let Some(err) = &*self.inner.closed.borrow() {
			return Err(err.clone());
		}
		let client = super::client_config(config)?;
		let (key, conn) = self
			.inner
			.endpoint
			.borrow_mut()
			.connect(Instant::now(), client, config.peer, &config.server_name)
			.map_err(|err| Error::Quic(err.to_string()))?;
		let shared = self.inner.launch(key, conn);
		// Flush the Initial flight; the demux task takes it from here.
		shared.kick();
		connection::establish(shared).await
	}
}

impl Clone for Endpoint {
	fn clone(&self) -> Self {
		self.inner.handles.set(self.inner.handles.get() + 1);
		Self {
			inner: self.inner.clone(),
		}
	}
}

impl Drop for Endpoint {
	fn drop(&mut self) {
		let handles = self.inner.handles.get() - 1;
		self.inner.handles.set(handles);
		if handles > 0 {
			return;
		}
		// Nobody will ever claim the queued connections; close them rather
		// than leaving their drivers to idle out.
		if let Some(accepting) = self.inner.accepting.borrow_mut().as_mut() {
			for mut conn in accepting.queue.drain(..) {
				web_transport_trait::poll::Session::close(&mut conn, 0, "endpoint closed");
			}
		}
		self.inner.task_waiters.borrow_mut().wake();
	}
}

impl std::fmt::Debug for Endpoint {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Endpoint")
			.field("local", &self.inner.local)
			.field("conns", &self.inner.conns.borrow().len())
			.finish()
	}
}

impl Inner {
	/// The demux task: receive, route, and stop once nothing needs us.
	fn poll_run(self: &Rc<Self>, waiter: &kio::Waiter) -> Poll<()> {
		// Handle drops and connection teardowns re-check the exit condition.
		waiter.register(&mut self.task_waiters.borrow_mut());

		loop {
			if self.handles.get() == 0 && self.conns.borrow().is_empty() {
				return Poll::Ready(());
			}
			match self.socket.poll_recv(waiter) {
				Poll::Ready(Ok(mut packet)) => self.demux(&mut packet),
				Poll::Ready(Err(err)) => {
					self.fail(Error::Io(err.to_string()));
					return Poll::Ready(());
				}
				Poll::Pending => return Poll::Pending,
			}
		}
	}

	/// Route every datagram in one receive, then kick the connections fed.
	fn demux(self: &Rc<Self>, packet: &mut udp::Packet) {
		let from = packet.from();
		// Where the endpoint writes its own answers (version negotiation,
		// retry, a refusal), reused across the whole receive.
		let mut buf = Vec::new();

		// The connections this packet fed; almost always exactly one (a GRO
		// coalesce is a single source, and ids rarely change mid-burst).
		let mut fed: Vec<ConnectionHandle> = Vec::new();

		for segment in packet.segments() {
			buf.clear();
			// The socket does not report which local address a datagram
			// arrived on, so the four-tuple stays half known, exactly as it
			// does on a platform without `IP_PKTINFO`.
			let event = self.endpoint.borrow_mut().handle(
				Instant::now(),
				from,
				None,
				None,
				BytesMut::from(&segment[..]),
				&mut buf,
			);
			match event {
				Some(DatagramEvent::ConnectionEvent(key, event)) => {
					let conn = self.conns.borrow().get(&key).cloned();
					// A connection whose driver has already exited is not one
					// we can feed.
					if let Some(conn) = conn {
						conn.conn.borrow_mut().handle_event(event);
						if !fed.contains(&key) {
							fed.push(key);
						}
					}
				}
				Some(DatagramEvent::NewConnection(incoming)) => {
					if let Some(key) = self.greet(incoming, &mut buf)
						&& !fed.contains(&key)
					{
						fed.push(key);
					}
				}
				// A dial-only endpoint owns no inbound policy, so it spends
				// nothing on strangers: no version negotiation, and no
				// stateless reset either.
				Some(DatagramEvent::Response(transmit)) if self.accepting.borrow().is_some() => {
					self.respond(&transmit, &buf)
				}
				Some(DatagramEvent::Response(_)) => {}
				None => {}
			}
		}

		for key in fed {
			let conn = self.conns.borrow().get(&key).cloned();
			if let Some(conn) = conn {
				conn.kick();
			}
		}
	}

	/// A datagram starting a handshake: accept it, or drop it and let the
	/// peer retransmit.
	fn greet(self: &Rc<Self>, incoming: Incoming, buf: &mut Vec<u8>) -> Option<ConnectionHandle> {
		// Nobody left to claim the connection. This also suppresses the
		// stateless responses of a socket nobody is serving from.
		if self.handles.get() == 0 {
			self.endpoint.borrow_mut().ignore(incoming);
			return None;
		}
		// Bound every connection the application has not claimed, whether its
		// handshake is still pending or it is already queued for accept.
		let queued = self
			.accepting
			.borrow()
			.as_ref()
			.map(|accepting| accepting.queue.len())
			.unwrap_or(0);
		if self.pending.get() + queued >= self.backlog {
			tracing::debug!(from = %incoming.remote_address(), "dropping a handshake over the backlog");
			// Dropped rather than refused: a real peer retransmits and lands
			// once the application drains the backlog.
			self.endpoint.borrow_mut().ignore(incoming);
			return None;
		}

		buf.clear();
		let accepted = self.endpoint.borrow_mut().accept(incoming, Instant::now(), buf, None);
		let (key, conn) = match accepted {
			Ok(accepted) => accepted,
			Err(err) => {
				tracing::debug!(err = %err.cause, "failed to accept a connection");
				if let Some(transmit) = err.response {
					self.respond(&transmit, buf);
				}
				return None;
			}
		};

		let shared = self.launch(key, conn);

		// Hand the connection over once (if) its handshake completes.
		self.pending.set(self.pending.get() + 1);
		let inner = self.clone();
		self.handle.spawn(async move {
			let outcome = connection::establish(shared).await;
			inner.pending.set(inner.pending.get() - 1);
			let mut conn = match outcome {
				Ok(conn) => conn,
				Err(err) => {
					tracing::debug!(%err, "incoming handshake failed");
					return;
				}
			};
			// The last handle may have left while we handshook; see Drop.
			if inner.handles.get() == 0 {
				web_transport_trait::poll::Session::close(&mut conn, 0, "endpoint closed");
				return;
			}
			{
				let mut accepting = inner.accepting.borrow_mut();
				let accepting = accepting.as_mut().expect("accepted without a server config");
				accepting.queue.push_back(conn);
			}
			inner.accept_waiters.borrow_mut().wake();
		});

		Some(key)
	}

	/// Send one packet the endpoint itself produced, from `buf`.
	///
	/// Best effort: under send backpressure the packet is dropped and the
	/// peer's retransmit tries again.
	fn respond(&self, transmit: &Transmit, buf: &[u8]) {
		let Poll::Ready(Ok(mut tx)) = self.socket.poll_acquire(&kio::Waiter::noop()) else {
			return;
		};
		if transmit.size > tx.len() {
			tracing::debug!(size = transmit.size, "dropping an oversized endpoint response");
			return;
		}
		tx[..transmit.size].copy_from_slice(&buf[..transmit.size]);
		let segment = transmit.segment_size.unwrap_or(transmit.size);
		if let Err(err) = tx.send(transmit.size, transmit.destination, segment) {
			tracing::debug!(%err, "failed to send an endpoint response");
		}
	}

	/// Register `conn`, spawn its driver, and arrange its teardown.
	fn launch(self: &Rc<Self>, key: ConnectionHandle, conn: quinn_proto::Connection) -> connection::Shared {
		let (shared, driver) = connection::launch(&self.handle, self.socket.clone(), Rc::downgrade(self), key, conn);
		self.conns.borrow_mut().insert(key, shared.clone());

		let inner = self.clone();
		self.handle.spawn(async move {
			driver.await;
			inner.release(key);
		});
		shared
	}

	/// The events a connection and the endpoint trade: fresh connection ids,
	/// retirements, and the drain that frees the connection's slot.
	pub(crate) fn on_connection_event(
		&self,
		key: ConnectionHandle,
		event: quinn_proto::EndpointEvent,
	) -> Option<quinn_proto::ConnectionEvent> {
		self.endpoint.borrow_mut().handle_event(key, event)
	}

	/// A connection's driver ended; forget it.
	fn release(&self, key: ConnectionHandle) {
		self.conns.borrow_mut().remove(&key);
		// The demux task may be waiting on us to exit.
		self.task_waiters.borrow_mut().wake();
	}

	/// The socket died: everything on it fails with `err`.
	fn fail(&self, err: Error) {
		*self.closed.borrow_mut() = Some(err.clone());
		let conns: Vec<connection::Shared> = self.conns.borrow().values().cloned().collect();
		for conn in conns {
			conn.fail(err.clone());
		}
		self.accept_waiters.borrow_mut().wake();
	}
}
