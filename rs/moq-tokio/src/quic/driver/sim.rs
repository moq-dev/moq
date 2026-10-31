//! QUIC with no sockets: endpoints on `moq-clock`'s simulator, joined by seeded, impaired paths
//!
//! A [`Network`] binds a [`Socket`] per address for [`Endpoint::new`](super::endpoint::Endpoint::new).
//! Each datagram crosses the [`Path`] from its sender to its destination: the [`Link`]'s
//! bottleneck queue, a fixed one-way latency, then its loss, duplication, jitter, and
//! reordering, all drawn from the network's seed so a seed replays a run exactly. A
//! destination nobody bound drops the datagram, as UDP does.
//!
//! Not part of the public API. It runs only inside `moq_clock::sim::run`, where every
//! arrival is a simulated timer.

use std::{
	collections::{HashMap, VecDeque},
	fmt,
	io::{self, IoSliceMut},
	net::SocketAddr,
	sync::{Arc, Mutex},
	task::{Context, Poll, Waker},
};

pub use moq_quic::sim::{Bottleneck, Link, QueueStats};
use moq_sock::udp::{EcnCodepoint, RecvMeta, Transmit};
use rand::{SeedableRng, rngs::StdRng};

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
	hosts: HashMap<SocketAddr, Host>,
	/// Keyed by sender, then destination. A pair never set is a clean path with no latency.
	paths: HashMap<(SocketAddr, SocketAddr), Path>,
}

/// A bound address
#[derive(Default)]
struct Host {
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
		})))
	}

	/// Bind `addr`, which receives until the socket drops
	///
	/// # Panics
	///
	/// If `addr` is already bound.
	pub fn bind(&self, addr: SocketAddr) -> Socket {
		let previous = self.0.lock().unwrap().hosts.insert(addr, Host::default());
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
		let now = moq_clock::now();
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
			drop(moq_clock::sim::spawn(async move {
				moq_clock::sleep_until(arrival).await;
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
