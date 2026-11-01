//! QUIC-LB forwarding between the servers behind one address.
//!
//! A server whose connection IDs carry a QUIC-LB server ID hands each short-header packet naming
//! another server to that server's forward socket, wrapped with the client's address and the
//! address the packet arrived on. The owner unwraps it and processes it as if the client had sent
//! it there. Replies need nothing more: a connection sends from the address its handshake arrived
//! on, which is the shared one, so only the client-to-server direction takes the extra hop.
//!
//! Long headers are never forwarded: a client picks the destination ID of its handshake, so the
//! handshake relies on the front end keeping a flow on one server.
#![cfg_attr(not(feature = "quic"), allow(dead_code, reason = "only the QUIC backend forwards"))]

use std::{
	collections::{HashMap, HashSet},
	io,
	net::{IpAddr, Ipv6Addr, SocketAddr},
	sync::{Arc, PoisonError, RwLock},
	task::{Context, Poll, ready},
};

use hmac::{KeyInit, Mac};
use moq_sock::udp::EcnCodepoint;

type Hmac = hmac::Hmac<sha2::Sha256>;

/// Where a QUIC-LB connection ID carries its server ID: after the byte encoding its length.
pub(crate) const ID_OFFSET: usize = 1;

/// The forward header's version, so a mismatched peer's forwards are dropped rather than misread.
const VERSION: u8 = 0;
/// The truncated HMAC-SHA256 tag that authenticates a forward.
const TAG_LEN: usize = 16;
/// The flag marking a forward that knows the address its packet arrived on.
const ARRIVAL: u8 = 0b100;
/// The header ahead of a forwarded packet: version, flags (ECN and [`ARRIVAL`]), the client's
/// port and IP, the arrival IP, and the tag over all of it and the packet.
pub(crate) const OVERHEAD: usize = 1 + 1 + 2 + 16 + 16 + TAG_LEN;

/// Generates QUIC-LB connection IDs: a byte encoding the length, the server ID, then a random nonce.
pub(crate) struct ServerIdGenerator {
	id: Box<[u8]>,
	nonce_len: usize,
}

impl ServerIdGenerator {
	/// Name `id` in every connection ID, ahead of `nonce_len` random bytes.
	pub(crate) fn new(id: &[u8], nonce_len: usize) -> Self {
		Self {
			id: id.into(),
			nonce_len,
		}
	}
}

impl moq_quic::ConnectionIdGenerator for ServerIdGenerator {
	fn generate_cid(&mut self) -> moq_quic::ConnectionId {
		use rand::RngExt;
		let cid_len = self.cid_len();
		let mut cid = Vec::with_capacity(cid_len);
		// The "self-encoded length" of server ID and nonce, which puts the server ID at `ID_OFFSET`.
		cid.push((cid_len - 1) as u8);
		cid.extend(self.id.iter());
		cid.extend(rand::rng().random_iter::<u8>().take(self.nonce_len));
		moq_quic::ConnectionId::new(cid.as_slice())
	}

	fn cid_len(&self) -> usize {
		ID_OFFSET + self.id.len() + self.nonce_len
	}

	fn cid_lifetime(&self) -> Option<std::time::Duration> {
		None
	}
}

/// The other servers behind the shared address, by server ID, replaced whole on each update.
#[derive(Clone, Debug, Default)]
pub(crate) struct Peers(Arc<RwLock<Map>>);

/// A snapshot of [`Peers`].
#[derive(Debug, Default)]
pub(crate) struct Map {
	by_id: HashMap<Box<[u8]>, SocketAddr>,
	/// The forward sockets accepted as sources.
	addrs: HashSet<SocketAddr>,
}

impl Map {
	/// Name `id`'s forward socket.
	pub(crate) fn insert(&mut self, id: &[u8], addr: SocketAddr) {
		self.by_id.insert(id.into(), addr);
		self.addrs.insert(addr);
	}

	/// Every peer, by server ID.
	#[cfg(test)]
	pub(crate) fn peers(&self) -> &HashMap<Box<[u8]>, SocketAddr> {
		&self.by_id
	}
}

impl Peers {
	/// Replace every peer at once.
	pub(crate) fn set(&self, map: Map) {
		*self.0.write().unwrap_or_else(PoisonError::into_inner) = map;
	}

	fn get(&self, id: &[u8]) -> Option<SocketAddr> {
		self.0
			.read()
			.unwrap_or_else(PoisonError::into_inner)
			.by_id
			.get(id)
			.copied()
	}

	fn contains(&self, addr: &SocketAddr) -> bool {
		self.0
			.read()
			.unwrap_or_else(PoisonError::into_inner)
			.addrs
			.contains(addr)
	}
}

/// A forwarded packet, as the client sent it.
pub(crate) struct Unwrapped<'a> {
	pub remote: SocketAddr,
	pub local_ip: Option<IpAddr>,
	pub ecn: Option<EcnCodepoint>,
	pub packet: &'a [u8],
}

/// One server's forwarding: its server ID, its peers, and the socket forwards travel on.
#[derive(Debug)]
pub(crate) struct Forward {
	socket: tokio::net::UdpSocket,
	/// The same socket, sending without waiting on the reactor to report it writable, which a
	/// fresh socket is not yet.
	send: std::net::UdpSocket,
	id: Box<[u8]>,
	peers: Peers,
	key: Hmac,
	/// Scratch for one forward, sent or received.
	buf: Box<[u8]>,
}

impl Forward {
	/// Forward on `socket` between the servers in `peers`, this one named `id` in its connection IDs.
	///
	/// `max_udp_payload_size` bounds the packets a client sends, so a forward never exceeds it
	/// plus [`OVERHEAD`]. The socket may fragment: a full-size packet plus the header can exceed
	/// the path between servers, which carries it anyway.
	pub(crate) fn new(
		socket: std::net::UdpSocket,
		id: &[u8],
		peers: Peers,
		key: &[u8; 32],
		max_udp_payload_size: u16,
	) -> io::Result<Self> {
		socket.set_nonblocking(true)?;
		let send = socket.try_clone()?;
		let socket = {
			let _runtime = crate::runtime::enter();
			tokio::net::UdpSocket::from_std(socket)?
		};
		Ok(Self {
			socket,
			send,
			id: id.into(),
			peers,
			key: Hmac::new_from_slice(key).expect("HMAC accepts any key length"),
			buf: vec![0; usize::from(max_udp_payload_size) + OVERHEAD].into(),
		})
	}

	/// Forward `packet` if its connection ID names another server, returning whether it was taken.
	///
	/// A packet naming a server missing from the peers is taken and dropped: answering it here
	/// would reset a connection another server owns.
	pub(crate) fn route(
		&mut self,
		packet: &[u8],
		remote: SocketAddr,
		local_ip: Option<IpAddr>,
		ecn: Option<EcnCodepoint>,
	) -> bool {
		let Some(id) = self.foreign(packet) else {
			return false;
		};
		let Some(peer) = self.peers.get(id) else {
			tracing::trace!(id = %hex::encode(id), "dropping a packet for an unknown server");
			return true;
		};
		// A client never sends more than the advertised payload size, so a longer datagram is
		// not a packet for one of our connections.
		let Some(len) = self.seal(packet, remote, local_ip, ecn) else {
			return true;
		};
		match self.send.send_to(&self.buf[..len], peer) {
			Ok(_) => {}
			// Lost like any datagram on a full socket; the client retransmits.
			Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
			Err(err) => tracing::debug!(%err, %peer, "failed to forward a packet"),
		}
		true
	}

	/// Wrap `packet` into the scratch buffer, returning the forward's length, unless it would not fit.
	fn seal(
		&mut self,
		packet: &[u8],
		remote: SocketAddr,
		local_ip: Option<IpAddr>,
		ecn: Option<EcnCodepoint>,
	) -> Option<usize> {
		let buf = self.buf.get_mut(..OVERHEAD + packet.len())?;
		buf[0] = VERSION;
		buf[1] = ecn.map_or(0, |ecn| ecn as u8) | local_ip.map_or(0, |_| ARRIVAL);
		buf[2..4].copy_from_slice(&remote.port().to_be_bytes());
		buf[4..20].copy_from_slice(&ipv6(remote.ip()).octets());
		buf[20..36].copy_from_slice(&local_ip.map_or(Ipv6Addr::UNSPECIFIED, ipv6).octets());
		buf[OVERHEAD..].copy_from_slice(packet);
		let tag = tag(&self.key, buf).finalize().into_bytes();
		buf[OVERHEAD - TAG_LEN..OVERHEAD].copy_from_slice(&tag[..TAG_LEN]);
		Some(buf.len())
	}

	/// Receive the next authentic forward, or `None` for a datagram that is not one.
	pub(crate) fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<Option<Unwrapped<'_>>>> {
		let mut read = tokio::io::ReadBuf::new(&mut self.buf);
		let from = ready!(self.socket.poll_recv_from(cx, &mut read))?;
		let len = read.filled().len();
		Poll::Ready(Ok(self.open(from, len)))
	}

	/// Authenticate the forward in the first `len` bytes of the scratch buffer.
	fn open(&self, from: SocketAddr, len: usize) -> Option<Unwrapped<'_>> {
		// Checked before the tag, so a stranger costs a lookup and never a hash.
		if !self.peers.contains(&from) {
			return None;
		}
		let buf = &self.buf[..len];
		if len < OVERHEAD || buf[0] != VERSION {
			return None;
		}
		tag(&self.key, buf)
			.verify_truncated_left(&buf[OVERHEAD - TAG_LEN..OVERHEAD])
			.ok()?;

		let packet = &buf[OVERHEAD..];
		// Never forward again, and never answer for a connection another server owns: a peer
		// with a stale view of who owns an ID would otherwise loop or reset it.
		if self.foreign(packet).is_some() || packet.first().is_none_or(|first| first & 0x80 != 0) {
			return None;
		}

		let flags = buf[1];
		let port = u16::from_be_bytes([buf[2], buf[3]]);
		let ip = |at: usize| canonical(Ipv6Addr::from(<[u8; 16]>::try_from(&buf[at..at + 16]).unwrap()));
		Some(Unwrapped {
			remote: SocketAddr::new(ip(4), port),
			local_ip: (flags & ARRIVAL != 0).then(|| ip(20)),
			ecn: EcnCodepoint::from_bits(flags),
			packet,
		})
	}

	/// The server ID in `packet`'s connection ID, when it is a short header naming another server.
	fn foreign<'a>(&self, packet: &'a [u8]) -> Option<&'a [u8]> {
		if packet.first()? & 0x80 != 0 {
			return None;
		}
		let start = 1 + ID_OFFSET;
		let id = packet.get(start..start + self.id.len())?;
		(id != &*self.id).then_some(id)
	}
}

/// The MAC over `forward`'s header, short of the tag itself, and its packet.
fn tag(key: &Hmac, forward: &[u8]) -> Hmac {
	let mut mac = key.clone();
	mac.update(&forward[..OVERHEAD - TAG_LEN]);
	mac.update(&forward[OVERHEAD..]);
	mac
}

fn ipv6(ip: IpAddr) -> Ipv6Addr {
	match ip {
		IpAddr::V4(ip) => ip.to_ipv6_mapped(),
		IpAddr::V6(ip) => ip,
	}
}

fn canonical(ip: Ipv6Addr) -> IpAddr {
	match ip.to_ipv4_mapped() {
		Some(ip) => IpAddr::V4(ip),
		None => IpAddr::V6(ip),
	}
}

#[cfg(test)]
mod tests {
	use std::{future::poll_fn, net::Ipv4Addr};

	use super::*;

	const KEY: [u8; 32] = [7; 32];

	type Opened = (SocketAddr, Option<IpAddr>, Option<EcnCodepoint>, Vec<u8>);

	fn socket() -> std::net::UdpSocket {
		std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap()
	}

	/// The server named `id`, forwarding on `socket` under `key`.
	fn server(id: u8, socket: std::net::UdpSocket, peers: &Peers, key: &[u8; 32]) -> Forward {
		Forward::new(socket, &[id], peers.clone(), key, 1200).unwrap()
	}

	/// A short header whose connection ID names server `id`.
	fn short(id: u8) -> Vec<u8> {
		vec![0x40, 5, id, 0xa, 0xb, 0xc, 0xd, 0x01, 0x02]
	}

	/// The next datagram `forward` receives, opened.
	async fn recv(forward: &mut Forward) -> Option<Opened> {
		let next = poll_fn(|cx| {
			forward
				.poll_recv(cx)
				.map_ok(|opened| opened.map(|o| (o.remote, o.local_ip, o.ecn, o.packet.to_vec())))
		});
		tokio::time::timeout(std::time::Duration::from_secs(5), next)
			.await
			.expect("a datagram")
			.unwrap()
	}

	/// Servers 1 and 2, each listing both.
	fn pair() -> (Forward, Forward, Peers) {
		let (a, b) = (socket(), socket());
		let peers = Peers::default();
		let mut map = Map::default();
		map.insert(&[1], a.local_addr().unwrap());
		map.insert(&[2], b.local_addr().unwrap());
		peers.set(map);
		(server(1, a, &peers, &KEY), server(2, b, &peers, &KEY), peers)
	}

	#[tokio::test]
	async fn a_forward_arrives_as_the_client_sent_it() {
		let (mut a, mut b, _) = pair();
		let client: SocketAddr = "192.0.2.7:5000".parse().unwrap();
		let arrival: IpAddr = "198.51.100.1".parse().unwrap();

		assert!(a.route(&short(2), client, Some(arrival), Some(EcnCodepoint::Ce)));
		assert_eq!(
			recv(&mut b).await,
			Some((client, Some(arrival), Some(EcnCodepoint::Ce), short(2)))
		);

		// An IPv6 client, through a server that never learned the arrival address.
		let client: SocketAddr = "[2001:db8::7]:5000".parse().unwrap();
		assert!(a.route(&short(2), client, None, None));
		assert_eq!(recv(&mut b).await, Some((client, None, None, short(2))));
	}

	/// What stays: this server's own connection IDs, long headers, and anything too short to
	/// carry a server ID. A short header for a server nobody listed is taken and dropped.
	#[tokio::test]
	async fn only_another_servers_short_headers_leave() {
		let (mut a, _b, _) = pair();
		let client: SocketAddr = "192.0.2.7:5000".parse().unwrap();
		assert!(!a.route(&short(1), client, None, None));
		let mut long = short(2);
		long[0] = 0xc0;
		assert!(!a.route(&long, client, None, None));
		assert!(!a.route(&[0x40, 5], client, None, None));
		assert!(a.route(&short(3), client, None, None));
	}

	/// A forward opens only from a listed peer, under the shared key, and only for this server: a
	/// peer with a stale map must not make it forward again or answer for another server.
	#[tokio::test]
	async fn a_forward_needs_a_peer_the_key_and_this_server() {
		let (mut a, mut b, peers) = pair();
		let client: SocketAddr = "192.0.2.7:5000".parse().unwrap();
		let b_addr = b.socket.local_addr().unwrap();

		// Not listed, with the right key.
		let mut stranger = server(1, socket(), &peers, &KEY);
		assert!(stranger.route(&short(2), client, None, None));
		assert_eq!(recv(&mut b).await, None);

		// Listed, with the wrong key.
		let mut forged = server(3, socket(), &peers, &[8; 32]);
		let mut map = Map::default();
		map.insert(&[1], a.socket.local_addr().unwrap());
		map.insert(&[2], b_addr);
		map.insert(&[3], forged.socket.local_addr().unwrap());
		peers.set(map);
		assert!(forged.route(&short(2), client, None, None));
		assert_eq!(recv(&mut b).await, None);

		// Server 1 sending server 2 a packet of its own, as a stale map would.
		let len = a.seal(&short(1), client, None, None).unwrap();
		a.send.send_to(&a.buf[..len], b_addr).unwrap();
		assert_eq!(recv(&mut b).await, None);

		// Still open to a real forward.
		assert!(a.route(&short(2), client, None, None));
		assert!(recv(&mut b).await.is_some());
	}
}
