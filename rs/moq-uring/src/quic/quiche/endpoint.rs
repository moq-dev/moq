//! The quiche endpoint: one socket, many quiche connections.
//!
//! quiche connections are self-contained, so the routing table is ours: a
//! demux task parses each datagram's destination connection id, feeds the
//! connection it names, and mints replacement ids while the peer has credit.
//! Everything policy-shaped (the backlog, the shard steering, the id length)
//! lives in [`super::super::endpoint`], which is shared with the other
//! backend.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::rc::Rc;
use std::task::Poll;

use rustc_hash::FxHashMap;

use super::{Connection, Error, connection};
use crate::quic::endpoint::{CID_LEN, Config, cid};
use crate::{Handle, udp};

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
	ids: ConnectionIds,
}

/// Every destination connection id routing to one connection.
struct ConnectionIds {
	/// The ids this endpoint issued, each [`CID_LEN`] bytes.
	issued: Vec<[u8; CID_LEN]>,
	/// The destination id the client chose for its Initial, for a connection
	/// that arrived rather than one we dialed.
	initial: Option<Vec<u8>>,
}

impl ConnectionIds {
	/// The ids for a connection we dialed: just the one we issued.
	fn issued(cid: [u8; CID_LEN]) -> Self {
		Self {
			issued: vec![cid],
			initial: None,
		}
	}
}

/// Destination connection id -> the connection it belongs to, split by who
/// chose the id.
///
/// The ids we issued are random and fixed width, so FxHash is enough: a peer
/// cannot pick them, so it cannot aim at a bucket. A client picks its own
/// Initial destination id, so those keep SipHash and its per-process seed.
/// Otherwise a peer could open connections whose Initial ids all hash alike
/// and turn a per-packet lookup into a scan of them.
#[derive(Default)]
struct Routes {
	issued: FxHashMap<[u8; CID_LEN], usize>,
	initial: HashMap<Vec<u8>, usize>,
}

impl Routes {
	/// The connection `cid` names. Ids we issued win, so a client cannot
	/// steal a live route by naming it as its Initial destination.
	fn get(&self, cid: &[u8]) -> Option<usize> {
		if let Ok(cid) = <[u8; CID_LEN]>::try_from(cid)
			&& let Some(key) = self.issued.get(&cid)
		{
			return Some(*key);
		}
		self.initial.get(cid).copied()
	}

	fn insert(&mut self, key: usize, ids: &ConnectionIds) {
		for cid in &ids.issued {
			self.issued.insert(*cid, key);
		}
		if let Some(cid) = &ids.initial {
			self.initial.insert(cid.clone(), key);
		}
	}

	/// Drop `key`'s routes. Two clients may pick the same Initial destination
	/// id, so only the entries still naming `key` go: the other connection
	/// keeps the one it won.
	fn remove(&mut self, key: usize, ids: &ConnectionIds) {
		for cid in &ids.issued {
			self.remove_issued(cid, key);
		}
		if let Some(cid) = &ids.initial
			&& self.initial.get(cid) == Some(&key)
		{
			self.initial.remove(cid);
		}
	}

	/// Drop `key`'s route for an id it issued and has now retired.
	fn remove_issued(&mut self, cid: &[u8; CID_LEN], key: usize) {
		if self.issued.get(cid) == Some(&key) {
			self.issued.remove(cid);
		}
	}
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
	routes: RefCell<Routes>,
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
				config: super::server_config(&server)?,
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
			routes: RefCell::new(Routes::default()),
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
		let mut quiche_config = super::client_config(config)?;
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
			.launch(conn, ConnectionIds::issued(scid), config.transport.keep_alive);
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
			let key = self.routes.borrow().get(hdr.dcid.as_ref());
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
		let ids = ConnectionIds {
			issued: vec![scid],
			initial: Some(hdr.dcid.to_vec()),
		};
		let (shared, key) = self.launch(conn, ids, keep_alive);
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
		ids: ConnectionIds,
		keep_alive: Option<std::time::Duration>,
	) -> (connection::Shared, usize) {
		let (shared, driver) = connection::launch(&self.handle, self.socket.clone(), conn, keep_alive);
		let mut conns = self.conns.borrow_mut();
		let key = conns.vacant_key();
		self.routes.borrow_mut().insert(key, &ids);
		conns.insert(Conn {
			shared: shared.clone(),
			ids,
		});
		drop(conns);

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
				self.routes.borrow_mut().issued.insert(cid, key);
				self.conns.borrow_mut()[key].ids.issued.push(cid);
			}
			while let Some(retired) = conn.retired_scid_next() {
				// We only ever issue CID_LEN ids, so this is what we pushed.
				let retired: [u8; CID_LEN] = retired.as_ref().try_into().expect("retired a foreign id");
				self.routes.borrow_mut().remove_issued(&retired, key);
				self.conns.borrow_mut()[key].ids.issued.retain(|cid| cid != &retired);
			}
		}
		shared.kick();
	}

	/// A connection's driver ended; forget it and its routes.
	fn release(&self, key: usize) {
		let conn = self.conns.borrow_mut().remove(key);
		self.routes.borrow_mut().remove(key, &conn.ids);
		// The demux task may be waiting on us to exit.
		self.task_waiters.borrow_mut().wake();
	}

	/// A fresh connection id no live connection already answers to.
	///
	/// A sharded endpoint spends a byte on steering, so the draw is 56 bits
	/// rather than 64. Rerolling a duplicate keeps "one id, one connection" an
	/// invariant instead of a probability, since a collision would otherwise
	/// hand one connection's packets to another.
	fn cid(&self) -> [u8; CID_LEN] {
		loop {
			let cid = cid(self.shard);
			if self.routes.borrow().get(&cid).is_none() {
				return cid;
			}
		}
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn routes_register_and_remove_issued_and_initial_ids() {
		let issued = [1; CID_LEN];
		// A client's Initial destination id is any width it likes.
		let initial = vec![2; CID_LEN * 2];
		let ids = ConnectionIds {
			issued: vec![issued],
			initial: Some(initial.clone()),
		};
		let mut routes = Routes::default();

		routes.insert(7, &ids);
		assert_eq!(routes.get(&issued), Some(7));
		assert_eq!(routes.get(&initial), Some(7));

		routes.remove(7, &ids);
		assert_eq!(routes.get(&issued), None);
		assert_eq!(routes.get(&initial), None);
	}

	#[test]
	fn an_initial_id_cannot_capture_an_issued_route() {
		let cid = [3; CID_LEN];
		let mut routes = Routes::default();
		routes.insert(1, &ConnectionIds::issued(cid));

		// A second client names the first connection's id as its Initial
		// destination: it must neither take the route nor take it away.
		let squatter = ConnectionIds {
			issued: vec![[4; CID_LEN]],
			initial: Some(cid.to_vec()),
		};
		routes.insert(2, &squatter);
		assert_eq!(routes.get(&cid), Some(1));

		routes.remove(2, &squatter);
		assert_eq!(routes.get(&cid), Some(1));
	}

	#[test]
	fn releasing_a_connection_leaves_a_shared_initial_id_alone() {
		// Nothing stops two clients picking the same Initial destination id.
		let shared = vec![5; CID_LEN * 2];
		let first = ConnectionIds {
			issued: vec![[6; CID_LEN]],
			initial: Some(shared.clone()),
		};
		let second = ConnectionIds {
			issued: vec![[7; CID_LEN]],
			initial: Some(shared.clone()),
		};
		let mut routes = Routes::default();
		routes.insert(1, &first);
		routes.insert(2, &second);
		assert_eq!(routes.get(&shared), Some(2));

		// The loser going away must not strand the winner's retransmits.
		routes.remove(1, &first);
		assert_eq!(routes.get(&shared), Some(2));

		routes.remove(2, &second);
		assert_eq!(routes.get(&shared), None);
	}
}
