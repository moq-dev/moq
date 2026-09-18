//! A userspace UDP shaper.
//!
//! Forwards datagrams between any number of clients and one upstream address,
//! treating each direction with a one-way delay, seeded random loss, and a rate
//! cap with a bounded queue, so a QUIC session runs over an impaired link on one
//! host with no privileges and nothing touching the host's network. A client is
//! recognized by its source address and gets its own upstream socket, so the
//! upstream sees one peer per client and QUIC never mistakes two for one.

use std::{
	collections::HashMap,
	net::SocketAddr,
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
	time::Duration,
};

use rand::{RngExt, SeedableRng, rngs::StdRng};
use tokio::{net::UdpSocket, sync::mpsc, time::Instant};

/// How each direction of a link is impaired.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Profile {
	/// Added to every datagram, one way: half the round trip the link should show.
	pub delay: Duration,
	/// The probability a datagram is dropped.
	pub loss: f64,
	/// Bits per second the link carries; `None` is unlimited.
	pub rate: Option<u64>,
	/// How much serialization the rate cap queues before dropping, the way a
	/// router buffer would.
	pub queue: Duration,
	/// Seeds the loss decisions, so a run can be reproduced.
	pub seed: u64,
}

impl Profile {
	/// A clean link showing this round trip.
	pub fn rtt(rtt: Duration) -> Self {
		Self {
			delay: rtt / 2,
			loss: 0.0,
			rate: None,
			queue: Duration::from_millis(100),
			seed: 0,
		}
	}

	/// Drop this fraction of datagrams in each direction.
	pub fn with_loss(mut self, loss: f64) -> Self {
		self.loss = loss;
		self
	}

	/// Cap each direction at this many bits per second.
	pub fn with_rate(mut self, bits_per_second: u64) -> Self {
		self.rate = Some(bits_per_second);
		self
	}

	/// Seed the loss decisions.
	pub fn with_seed(mut self, seed: u64) -> Self {
		self.seed = seed;
		self
	}
}

/// What a shaper did to the traffic through it, both directions summed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
	/// Datagrams delivered.
	pub forwarded: u64,
	/// Payload bytes delivered.
	pub bytes: u64,
	/// Datagrams dropped by the loss profile.
	pub lost: u64,
	/// Datagrams dropped because the rate cap's queue was full.
	pub overflowed: u64,
}

#[derive(Default)]
struct Totals {
	forwarded: AtomicU64,
	bytes: AtomicU64,
	lost: AtomicU64,
	overflowed: AtomicU64,
}

/// A running shaper in front of one upstream address. Dropping it stops the
/// forwarding; sessions through it then time out like a cut cable.
pub struct Shaper {
	addr: SocketAddr,
	totals: Arc<Totals>,
	task: tokio::task::JoinHandle<()>,
}

impl Shaper {
	/// Listen on a fresh loopback port and forward to `upstream` under `profile`.
	pub async fn start(upstream: SocketAddr, profile: Profile) -> std::io::Result<Self> {
		let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);
		let addr = socket.local_addr()?;
		let totals = Arc::new(Totals::default());
		let task = tokio::spawn(run(socket, upstream, profile, totals.clone()));
		Ok(Self { addr, totals, task })
	}

	/// The address clients dial instead of the upstream.
	pub fn addr(&self) -> SocketAddr {
		self.addr
	}

	pub fn counters(&self) -> Counters {
		Counters {
			forwarded: self.totals.forwarded.load(Ordering::Relaxed),
			bytes: self.totals.bytes.load(Ordering::Relaxed),
			lost: self.totals.lost.load(Ordering::Relaxed),
			overflowed: self.totals.overflowed.load(Ordering::Relaxed),
		}
	}
}

impl Drop for Shaper {
	fn drop(&mut self) {
		self.task.abort();
	}
}

/// Where a shaped datagram goes.
enum Sink {
	/// The client's own upstream socket, connected to the upstream address.
	Upstream(Arc<UdpSocket>),
	/// The listening socket, back to the client.
	Client(Arc<UdpSocket>, SocketAddr),
}

impl Sink {
	async fn send(&self, datagram: &[u8]) -> std::io::Result<usize> {
		match self {
			Sink::Upstream(socket) => socket.send(datagram).await,
			Sink::Client(socket, client) => socket.send_to(datagram, client).await,
		}
	}
}

/// One direction of one client's link: decides each datagram's fate and
/// departure time on arrival, and a delay line releases them in order.
struct Pipe {
	profile: Profile,
	rng: StdRng,
	/// When the rate cap finishes serializing the last accepted datagram.
	departs: Instant,
	line: mpsc::UnboundedSender<(Instant, Vec<u8>)>,
	totals: Arc<Totals>,
	task: tokio::task::JoinHandle<()>,
}

impl Pipe {
	fn new(profile: Profile, seed: u64, sink: Sink, totals: Arc<Totals>) -> Self {
		let (line, mut rx) = mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
		let task = tokio::spawn({
			let totals = totals.clone();
			async move {
				// Departure times are non-decreasing (a constant delay over a
				// monotonic serialization clock), so waiting on each in turn is a
				// delay line, not a stall.
				while let Some((at, datagram)) = rx.recv().await {
					tokio::time::sleep_until(at).await;
					if sink.send(&datagram).await.is_ok() {
						totals.forwarded.fetch_add(1, Ordering::Relaxed);
						totals.bytes.fetch_add(datagram.len() as u64, Ordering::Relaxed);
					}
				}
			}
		});
		Self {
			profile,
			rng: StdRng::seed_from_u64(seed),
			departs: Instant::now(),
			line,
			totals,
			task,
		}
	}

	/// Drop, queue, or delay one datagram.
	fn push(&mut self, datagram: Vec<u8>) {
		if self.profile.loss > 0.0 && self.rng.random::<f64>() < self.profile.loss {
			self.totals.lost.fetch_add(1, Ordering::Relaxed);
			return;
		}
		let now = Instant::now();
		let mut at = now;
		if let Some(rate) = self.profile.rate {
			let start = self.departs.max(now);
			if start.duration_since(now) > self.profile.queue {
				self.totals.overflowed.fetch_add(1, Ordering::Relaxed);
				return;
			}
			let serialization = Duration::from_secs_f64(datagram.len() as f64 * 8.0 / rate as f64);
			self.departs = start + serialization;
			at = self.departs;
		}
		at += self.profile.delay;
		let _ = self.line.send((at, datagram));
	}
}

impl Drop for Pipe {
	fn drop(&mut self) {
		self.task.abort();
	}
}

/// One client's flow: the pipe toward the upstream and the pump reading the
/// upstream's replies into the pipe back to the client.
struct Flow {
	to_upstream: Pipe,
	pump: tokio::task::JoinHandle<()>,
}

impl Drop for Flow {
	fn drop(&mut self) {
		self.pump.abort();
	}
}

async fn run(socket: Arc<UdpSocket>, upstream: SocketAddr, profile: Profile, totals: Arc<Totals>) {
	let mut flows: HashMap<SocketAddr, Flow> = HashMap::new();
	let mut buf = vec![0u8; 65536];
	loop {
		let Ok((n, client)) = socket.recv_from(&mut buf).await else {
			return;
		};
		if !flows.contains_key(&client) {
			let Ok(up) = UdpSocket::bind("127.0.0.1:0").await else {
				continue;
			};
			if up.connect(upstream).await.is_err() {
				continue;
			}
			let up = Arc::new(up);
			// Distinct seeds per flow and direction, derived from the profile's.
			let index = flows.len() as u64;
			let seed = profile.seed.wrapping_mul(1_000_003).wrapping_add(index * 2);
			let to_upstream = Pipe::new(profile, seed, Sink::Upstream(up.clone()), totals.clone());
			let mut to_client = Pipe::new(profile, seed + 1, Sink::Client(socket.clone(), client), totals.clone());
			let pump = tokio::spawn(async move {
				let mut buf = vec![0u8; 65536];
				while let Ok(n) = up.recv(&mut buf).await {
					to_client.push(buf[..n].to_vec());
				}
			});
			flows.insert(client, Flow { to_upstream, pump });
		}
		flows
			.get_mut(&client)
			.expect("inserted above")
			.to_upstream
			.push(buf[..n].to_vec());
	}
}
