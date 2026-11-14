//! Test support, enabled by the `sim` feature: QUIC with no sockets, endpoints on kio's
//! simulator joined by seeded, impaired paths
//!
//! A [`Network`] binds a [`Socket`] per address for [`Endpoint::new`](super::endpoint::Endpoint::new),
//! and a [`Host`] is one machine's address on it, where a [`Client`](crate::Client) or
//! [`Server`](crate::Server) binds its QUIC endpoint in place of a UDP socket.
//! Each datagram crosses the [`Path`] from its sender to its destination: the [`Link`]'s
//! bottleneck queue, a fixed one-way latency, then its loss, duplication, jitter, and
//! reordering, all drawn from the network's seed. With each endpoint's `rng_seed` set too, a
//! seed replays a run exactly. A destination nobody bound drops the datagram, as UDP does.
//!
//! It runs only inside `kio::clock::sim::run`, where every arrival is a simulated timer.
//!
//! It tracks the crate's internals, so it can change in any release.

use std::{
	collections::{HashMap, VecDeque},
	fmt,
	io::{self, IoSliceMut},
	net::{IpAddr, SocketAddr},
	sync::{Arc, Mutex},
	task::{Context, Poll, Waker},
};

pub use moq_quic::sim::{Bottleneck, Link, QueueStats};
use moq_sock::udp::{EcnCodepoint, RecvMeta, Transmit};
use rand::{RngExt, SeedableRng, rngs::StdRng};

use super::{Duration, endpoint::AsyncUdpSocket};

/// One direction between two addresses
#[derive(Debug, Clone, Default)]
pub struct Path {
	/// The one-way delay every datagram takes, after any queueing
	pub latency: Duration,
	/// The bottleneck and the random impairments
	pub link: Link,
}

/// The addresses bound on the simulator and the paths between them
#[derive(Clone)]
pub struct Network(Arc<Mutex<State>>);

struct State {
	/// Draws every impairment, so the seed replays them
	rng: StdRng,
	hosts: HashMap<SocketAddr, Bound>,
	/// Keyed by sender, then destination. A pair never set is a clean path with no latency.
	paths: HashMap<(SocketAddr, SocketAddr), Path>,
	/// Where the search for a free ephemeral port starts, so a port is not reused soon after it closes
	ephemeral: u16,
}

/// The first port [`Host::bind`] hands out for port 0, the start of the IANA dynamic range
const EPHEMERAL: u16 = 49152;

/// A bound address
#[derive(Default)]
struct Bound {
	/// Datagrams that arrived and await a receive, in arrival order
	inbound: VecDeque<Datagram>,
	/// The endpoint waiting for the next arrival
	waker: Option<Waker>,
}

struct Datagram {
	from: SocketAddr,
	ecn: Option<EcnCodepoint>,
	contents: Vec<u8>,
}

impl Network {
	/// A network with nothing bound, whose impairments `seed` replays
	pub fn new(seed: u64) -> Self {
		Self(Arc::new(Mutex::new(State {
			rng: StdRng::seed_from_u64(seed),
			hosts: HashMap::new(),
			paths: HashMap::new(),
			ephemeral: EPHEMERAL,
		})))
	}

	/// The machine at `ip`, which binds its endpoints' sockets on this network
	pub fn host(&self, ip: IpAddr) -> Host {
		Host {
			network: self.clone(),
			ip,
		}
	}

	/// Bind `addr`, which receives until the socket drops
	///
	/// # Panics
	///
	/// If `addr` is already bound.
	pub fn bind(&self, addr: SocketAddr) -> Socket {
		let previous = self.0.lock().unwrap().hosts.insert(addr, Bound::default());
		assert!(previous.is_none(), "{addr} is already bound");
		Socket {
			network: self.clone(),
			addr,
		}
	}

	/// Run `f` on the path from `from` to `to`, to set its impairments or read its queue's stats
	pub fn path<R>(&self, from: SocketAddr, to: SocketAddr, f: impl FnOnce(&mut Path) -> R) -> R {
		f(self.0.lock().unwrap().paths.entry((from, to)).or_default())
	}

	/// Send one datagram from `from`, scheduling each copy that survives the path
	fn send(&self, from: SocketAddr, to: SocketAddr, ecn: Option<EcnCodepoint>, contents: &[u8]) {
		let now = kio::clock::now();
		let (marked, arrivals) = {
			let state = &mut *self.0.lock().unwrap();
			let path = state.paths.entry((from, to)).or_default();
			path.link.transmit(&mut state.rng, now, path.latency, contents.len())
		};
		// Only an ECN-capable datagram can carry the bottleneck's mark, as at a router.
		let ecn = match ecn {
			Some(_) if marked => Some(EcnCodepoint::Ce),
			ecn => ecn,
		};
		for arrival in arrivals {
			let network = self.clone();
			let datagram = Datagram {
				from,
				ecn,
				contents: contents.to_vec(),
			};
			drop(kio::clock::sim::spawn(async move {
				kio::clock::sleep_until(arrival).await;
				network.deliver(to, datagram);
			}));
		}
	}

	fn deliver(&self, to: SocketAddr, datagram: Datagram) {
		let mut state = self.0.lock().unwrap();
		// Nobody bound the destination, or it closed since: the datagram is lost.
		let Some(host) = state.hosts.get_mut(&to) else {
			return;
		};
		host.inbound.push_back(datagram);
		let waker = host.waker.take();
		drop(state);
		if let Some(waker) = waker {
			waker.wake();
		}
	}
}

/// One machine's address on a [`Network`]
///
/// Set it as [`client::Config::sim`](crate::client::Config::sim) or
/// [`server::Config::sim`](crate::server::Config::sim) to bind that side's QUIC endpoint
/// here, on the configured bind port, instead of opening a UDP socket. Each endpoint it binds
/// draws its random seed from the network's, so a seed replays the endpoints too.
#[derive(Clone, Debug)]
pub struct Host {
	network: Network,
	ip: IpAddr,
}

impl Host {
	/// The machine's address
	pub fn ip(&self) -> IpAddr {
		self.ip
	}

	/// Bind `port`, or a free ephemeral one for 0, and draw a seed for the endpoint using it
	pub(crate) fn bind(&self, port: u16) -> io::Result<(Socket, [u8; 32])> {
		let state = &mut *self.network.0.lock().unwrap();
		let port = match port {
			0 => {
				let range = u16::MAX - EPHEMERAL + 1;
				let free = (0..range)
					.map(|offset| EPHEMERAL + (state.ephemeral - EPHEMERAL + offset) % range)
					.find(|&port| !state.hosts.contains_key(&SocketAddr::new(self.ip, port)))
					.ok_or_else(|| io::Error::from(io::ErrorKind::AddrInUse))?;
				state.ephemeral = free.checked_add(1).unwrap_or(EPHEMERAL);
				free
			}
			port => port,
		};
		let addr = SocketAddr::new(self.ip, port);
		if state.hosts.contains_key(&addr) {
			return Err(io::ErrorKind::AddrInUse.into());
		}
		state.hosts.insert(addr, Bound::default());
		let seed = state.rng.random();
		let socket = Socket {
			network: self.network.clone(),
			addr,
		};
		Ok((socket, seed))
	}
}

impl fmt::Debug for Network {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Network").finish_non_exhaustive()
	}
}

/// An address bound on a [`Network`], unbound when dropped
#[derive(Debug)]
pub struct Socket {
	network: Network,
	addr: SocketAddr,
}

impl AsyncUdpSocket for Socket {
	fn poll_recv(
		&self,
		cx: &mut Context<'_>,
		bufs: &mut [IoSliceMut<'_>],
		meta: &mut [RecvMeta],
	) -> Poll<io::Result<usize>> {
		let mut state = self.network.0.lock().unwrap();
		let host = state.hosts.get_mut(&self.addr).expect("bound until dropped");
		let mut received = 0;
		for (buf, meta) in bufs.iter_mut().zip(meta.iter_mut()) {
			let Some(datagram) = host.inbound.pop_front() else {
				break;
			};
			let len = datagram.contents.len().min(buf.len());
			buf[..len].copy_from_slice(&datagram.contents[..len]);
			*meta = RecvMeta::default();
			meta.addr = datagram.from;
			meta.len = len;
			meta.stride = len;
			meta.ecn = datagram.ecn;
			meta.dst_ip = Some(self.addr.ip());
			received += 1;
		}
		if received == 0 {
			host.waker = Some(cx.waker().clone());
			return Poll::Pending;
		}
		Poll::Ready(Ok(received))
	}

	fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
		let segment = transmit.segment_size.unwrap_or(transmit.contents.len()).max(1);
		for contents in transmit.contents.chunks(segment) {
			self.network
				.send(self.addr, transmit.destination, transmit.ecn, contents);
		}
		Ok(())
	}

	fn poll_writable(&self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Poll::Ready(Ok(()))
	}

	fn local_addr(&self) -> io::Result<SocketAddr> {
		Ok(self.addr)
	}
}

impl Drop for Socket {
	fn drop(&mut self) {
		self.network.0.lock().unwrap().hosts.remove(&self.addr);
	}
}

impl From<Socket> for super::endpoint::Socket {
	fn from(socket: Socket) -> Self {
		(Box::new(socket) as Box<dyn AsyncUdpSocket>).into()
	}
}
