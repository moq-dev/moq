//! The multi-connection endpoint: one socket, many QUIC connections.
//!
//! An [`Endpoint`] owns a [`udp::Socket`] and routes every received datagram
//! to the connection its destination connection id names: dials share the
//! socket with accepted connections, which is what lets one worker socket
//! carry a relay's inbound sessions and its upstream cluster dials at once.
//! A single demux task receives; each connection keeps a driver task of its
//! own for timers and egress.
//!
//! Every connection id this endpoint issues is [`CID_LEN`] bytes, which is
//! what lets a short header (which does not encode the id's length) be
//! parsed at all. Ids rotate as peers consume them (`NEW_CONNECTION_ID`), so
//! a migrating client stays routable; an Initial for an unsupported version
//! gets a version negotiation packet back.
//!
//! An endpoint whose socket is a member of a steered `SO_REUSEPORT` group
//! (one worker per core on one port) sets [`Config::shard`], and every id it
//! issues then carries the [`moq_sock::shard::cid_prefix`] steering byte, so
//! the kernel keeps delivering a connection's packets to the worker that owns
//! it. Dials through the endpoint carry it too, which is what steers a
//! cluster peer's responses back to the dialing worker.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::rc::Rc;
use std::task::Poll;

use super::{Connection, Error, connection, server};
use crate::{Handle, udp};

/// Every connection id this endpoint issues is this long: long enough to be
/// unguessable per socket, and fixed because a short header does not encode
/// the id's length, so parsing one assumes it.
pub const CID_LEN: usize = 8;

/// What an endpoint serves, beyond dialing.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// Accept incoming connections with this configuration. Without it the
	/// endpoint only dials, and inbound handshakes are dropped.
	pub server: Option<server::Config>,

	/// How many incoming connections may await acceptance at once (default 1024).
	///
	/// This bounds both handshakes in flight and completed connections queued
	/// for [`Endpoint::accept`]. An Initial past the cap is dropped; a real
	/// peer retransmits and lands once the application drains the backlog or
	/// a handshake fails.
	pub backlog: usize,

	/// This socket's slot in a steered `SO_REUSEPORT` group, if it is in one.
	///
	/// Every connection id the endpoint issues then leads with the slot's
	/// [`cid_prefix`](moq_sock::shard::cid_prefix) byte, which is what the
	/// group's filter selects on. Set it if and only if the socket was bound
	/// into a group with this shard; a lone socket leaves it `None` and keeps
	/// the whole id random.
	pub shard: Option<moq_sock::shard::Shard>,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			server: None,
			backlog: 1024,
			shard: None,
		}
	}
}

impl Config {
	/// Accept incoming connections with `server`, on top of dialing.
	pub fn with_server(mut self, server: server::Config) -> Self {
		self.server = Some(server);
		self
	}

	/// Issue connection ids steering to `shard`'s slot of a reuseport group.
	pub fn with_shard(mut self, shard: moq_sock::shard::Shard) -> Self {
		self.shard = Some(shard);
		self
	}
}

/// The accept side: the shared quiche config and handshaken connections
/// nobody has claimed yet.
struct Accepting {
	config: quiche::Config,
	/// The keep-alive every accepted connection's driver runs.
	keep_alive: Option<std::time::Duration>,
	queue: VecDeque<Connection>,
}

/// One live connection: its shared state and the routes pointing at it.
struct Conn {
	shared: connection::Shared,
	/// Every id in [`Inner::routes`] naming this connection, so teardown can
	/// remove exactly them.
	cids: Vec<Vec<u8>>,
}

/// State shared by the handles, the demux task, and the per-connection
/// teardown tasks.
struct Inner {
	handle: Handle,
	socket: Rc<udp::Socket>,
	local: SocketAddr,
	accepting: RefCell<Option<Accepting>>,
	conns: RefCell<slab::Slab<Conn>>,
	/// Destination connection id -> the connection it belongs to.
	routes: RefCell<HashMap<Vec<u8>, usize>>,
	/// Incoming handshakes in flight. Together with the accept queue this is
	/// bounded by [`Config::backlog`].
	pending: Cell<usize>,
	backlog: usize,
	/// The reuseport slot every issued connection id steers to, if any.
	shard: Option<moq_sock::shard::Shard>,
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
		let accepting = match config.server {
			Some(server) => Some(Accepting {
				config: server.quiche()?,
				keep_alive: server.transport.keep_alive,
				queue: VecDeque::new(),
			}),
			None => None,
		};
		let inner = Rc::new(Inner {
			handle: handle.clone(),
			socket: Rc::new(socket),
			local,
			accepting: RefCell::new(accepting),
			conns: RefCell::new(slab::Slab::new()),
			routes: RefCell::new(HashMap::new()),
			pending: Cell::new(0),
			backlog: config.backlog,
			shard: config.shard,
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

	/// Dial [`Config::peer`](super::client::Config::peer) through this
	/// endpoint's socket, driving the handshake to completion.
	pub async fn connect(&self, config: &super::client::Config) -> Result<Connection, Error> {
		if let Some(err) = &*self.inner.closed.borrow() {
			return Err(err.clone());
		}
		let mut quiche_config = config.quiche()?;
		let scid = self.inner.cid();
		let conn = quiche::connect(
			Some(&config.server_name),
			&quiche::ConnectionId::from_ref(&scid),
			self.inner.local,
			config.peer,
			&mut quiche_config,
		)?;
		let (shared, _key) = self
			.inner
			.launch(conn, vec![scid.to_vec()], config.transport.keep_alive);
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

	/// Route every datagram in one receive, then service the connections fed.
	fn demux(self: &Rc<Self>, packet: &mut udp::Packet) {
		let from = packet.from();
		let info = quiche::RecvInfo { from, to: self.local };

		// The connections this packet fed; almost always exactly one (a GRO
		// coalesce is a single source, and ids rarely change mid-burst).
		let mut fed: Vec<usize> = Vec::new();

		for segment in packet.segments() {
			// A datagram can coalesce several QUIC packets, but they all
			// belong to one connection, so the first header routes it whole.
			let hdr = match quiche::Header::from_slice(segment, CID_LEN) {
				Ok(hdr) => hdr,
				Err(err) => {
					tracing::trace!(%err, "dropping an unparseable datagram");
					continue;
				}
			};
			let key = self.routes.borrow().get(hdr.dcid.as_ref()).copied();
			match key {
				Some(key) => {
					let conn = self.conns.borrow()[key].shared.clone();
					// A malformed/undecryptable datagram is UDP noise.
					if let Err(err) = conn.conn.borrow_mut().recv(segment, info) {
						tracing::debug!(%err, "quiche dropped a datagram");
					}
					conn.mark_ingested();
					if !fed.contains(&key) {
						fed.push(key);
					}
				}
				None => {
					if let Some(key) = self.greet(hdr, segment, info)
						&& !fed.contains(&key)
					{
						fed.push(key);
					}
				}
			}
		}

		for key in fed {
			self.service(key);
		}
	}

	/// A datagram no connection claims: the start of a handshake, or noise.
	/// Returns the new connection's key when one is born.
	fn greet(self: &Rc<Self>, hdr: quiche::Header<'_>, segment: &mut [u8], info: quiche::RecvInfo) -> Option<usize> {
		// Nobody left to claim the connection, or a dial-only endpoint. This
		// also suppresses stateless responses from a socket that does not serve.
		if self.handles.get() == 0 || self.accepting.borrow().is_none() {
			tracing::trace!(from = %info.from, "dropping an unsolicited handshake");
			return None;
		}
		// Long-header packet types are version-specific. Negotiate an unknown
		// nonzero version before interpreting those bits with our version's map.
		if hdr.version != 0 && !quiche::version_is_supported(hdr.version) {
			self.negotiate(&hdr, info.from);
			return None;
		}
		if hdr.ty != quiche::Type::Initial {
			tracing::trace!(from = %info.from, ty = ?hdr.ty, "dropping an unroutable datagram");
			return None;
		}
		// Bound every connection the application has not claimed, whether its
		// handshake is still pending or it is already queued for accept.
		let queued = self.accepting.borrow().as_ref().expect("checked above").queue.len();
		if self.pending.get() + queued >= self.backlog {
			tracing::debug!(from = %info.from, "dropping a handshake over the backlog");
			return None;
		}

		let scid = self.cid();
		let keep_alive = self.accepting.borrow().as_ref().expect("checked above").keep_alive;
		let mut conn = {
			let mut accepting = self.accepting.borrow_mut();
			let accepting = accepting.as_mut().expect("checked above");
			match quiche::accept(
				&quiche::ConnectionId::from_ref(&scid),
				None,
				self.local,
				info.from,
				&mut accepting.config,
			) {
				Ok(conn) => conn,
				Err(err) => {
					tracing::debug!(%err, "failed to accept a connection");
					return None;
				}
			}
		};
		if let Err(err) = conn.recv(segment, info) {
			// Without a valid first flight there is no handshake to continue.
			tracing::debug!(%err, "dropping a connection whose Initial was rejected");
			return None;
		}

		// Route our chosen id, and the client's Initial id: retransmitted
		// Initials (and 0-RTT) keep carrying it until our id takes effect.
		let (shared, key) = self.launch(conn, vec![scid.to_vec(), hdr.dcid.to_vec()], keep_alive);
		shared.mark_ingested();

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

	/// Tell a peer speaking an unknown QUIC version which ones we do speak.
	/// Best effort: under send backpressure the packet is dropped and the
	/// peer's retransmit tries again.
	fn negotiate(&self, hdr: &quiche::Header<'_>, to: SocketAddr) {
		let Poll::Ready(Ok(mut tx)) = self.socket.poll_acquire(&kio::Waiter::noop()) else {
			return;
		};
		match quiche::negotiate_version(&hdr.scid, &hdr.dcid, &mut tx) {
			Ok(len) => {
				if let Err(err) = tx.send(len, to, len) {
					tracing::debug!(%err, "failed to send a version negotiation packet");
				}
			}
			Err(err) => tracing::debug!(%err, "failed to build a version negotiation packet"),
		}
	}

	/// Register `conn`, spawn its driver, and arrange its teardown.
	fn launch(
		self: &Rc<Self>,
		conn: quiche::Connection,
		cids: Vec<Vec<u8>>,
		keep_alive: Option<std::time::Duration>,
	) -> (connection::Shared, usize) {
		let (shared, driver) = connection::launch(&self.handle, self.socket.clone(), conn, keep_alive);
		let key = self.conns.borrow_mut().insert(Conn {
			shared: shared.clone(),
			cids: cids.clone(),
		});
		{
			let mut routes = self.routes.borrow_mut();
			for cid in cids {
				routes.insert(cid, key);
			}
		}

		let inner = self.clone();
		self.handle.spawn(async move {
			driver.await;
			inner.release(key);
		});
		(shared, key)
	}

	/// Post-ingress service for one connection: mint replacement connection
	/// ids while the peer has credit, drop retired ones, and kick the driver
	/// so fresh egress reaches the wire.
	fn service(&self, key: usize) {
		let shared = self.conns.borrow()[key].shared.clone();
		{
			let mut conn = shared.conn.borrow_mut();

			// The peer's active_connection_id_limit is the credit; keep it
			// spent so a migration always has a fresh id to switch to.
			while conn.scids_left() > 0 {
				let cid = self.cid();
				if conn
					.new_scid(&quiche::ConnectionId::from_ref(&cid), rand::random(), false)
					.is_err()
				{
					break;
				}
				self.routes.borrow_mut().insert(cid.to_vec(), key);
				self.conns.borrow_mut()[key].cids.push(cid.to_vec());
			}
			while let Some(retired) = conn.retired_scid_next() {
				self.routes.borrow_mut().remove(retired.as_ref());
				self.conns.borrow_mut()[key].cids.retain(|cid| cid != retired.as_ref());
			}
		}
		shared.kick();
	}

	/// A connection's driver ended; forget it and its routes.
	fn release(&self, key: usize) {
		let conn = self.conns.borrow_mut().remove(key);
		let mut routes = self.routes.borrow_mut();
		for cid in conn.cids {
			routes.remove(&cid);
		}
		drop(routes);
		// The demux task may be waiting on us to exit.
		self.task_waiters.borrow_mut().wake();
	}

	/// A fresh [`CID_LEN`]-byte connection id, leading with the steering
	/// prefix when this endpoint sits in a reuseport group.
	fn cid(&self) -> [u8; CID_LEN] {
		let mut cid: [u8; CID_LEN] = rand::random();
		if let Some(shard) = self.shard {
			cid[0] = moq_sock::shard::cid_prefix(shard);
		}
		cid
	}

	/// The socket died: everything on it fails with `err`.
	fn fail(&self, err: Error) {
		*self.closed.borrow_mut() = Some(err.clone());
		let conns: Vec<connection::Shared> = self
			.conns
			.borrow()
			.iter()
			.map(|(_, conn)| conn.shared.clone())
			.collect();
		for conn in conns {
			conn.fail(err.clone());
		}
		self.accept_waiters.borrow_mut().wake();
	}
}
