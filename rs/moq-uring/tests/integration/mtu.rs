//! Path MTU through the io_uring driver: a path that carries only the QUIC
//! floor still connects, discovery stays off unless asked, and when on it
//! finds a larger MTU, keeps batching GSO trains at it, and falls back when
//! the path shrinks.
//!
//! The path is a userspace hop that drops whatever exceeds its MTU without a
//! word, the way a tunnel behind a firewall that eats ICMP does. It crosses
//! real sockets, so these run on the wall clock.
//!
//! Unsupported local kernels skip loudly; CI_STRICT makes CI refuse them.

#![cfg(all(target_os = "linux", feature = "quic"))]

use crate::support;

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use moq_uring::{Config, Error, Worker, quic, udp};

const ALPN: &str = "moq-uring-mtu";
/// The UDP payload every QUIC path must carry, and moq-quic's floor.
const FLOOR: usize = 1200;
/// The UDP payload an IPv6 path must carry: the 1280-byte minimum link MTU
/// less the IPv6 and UDP headers. Above the floor, below a 1350-byte pin.
const IPV6_MIN: usize = 1232;
/// The UDP payload of an Ethernet path over IPv4.
const ETHERNET: usize = 1472;
/// Enough data for full GSO trains.
const PAYLOAD: usize = 1 << 20;

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) if std::env::var_os("CI_STRICT").is_none() => {
			eprintln!("skipping io_uring MTU test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

/// What one direction of a [`Path`] forwarded.
#[derive(Default)]
struct Seen {
	/// The largest datagram, in bytes.
	largest: AtomicUsize,
	/// Datagrams larger than the floor.
	large: AtomicU64,
}

impl Seen {
	fn forwarded(&self, len: usize) {
		self.largest.fetch_max(len, Ordering::Relaxed);
		if len > FLOOR {
			self.large.fetch_add(1, Ordering::Relaxed);
		}
	}
}

/// A hop between one client and `server` that silently drops every datagram
/// larger than its MTU, which can change mid-connection.
struct Path {
	addr: SocketAddr,
	mtu: Arc<AtomicUsize>,
	/// Toward the server, then back toward the client.
	seen: Arc<[Seen; 2]>,
	stop: Arc<AtomicBool>,
	threads: Vec<std::thread::JoinHandle<()>>,
}

impl Path {
	fn new(server: SocketAddr, mtu: usize) -> Self {
		let front = UdpSocket::bind("127.0.0.1:0").expect("bind the client side");
		let back = UdpSocket::bind("127.0.0.1:0").expect("bind the server side");
		back.connect(server).expect("connect to the server");
		// Short, so a thread notices `stop` promptly.
		for sock in [&front, &back] {
			sock.set_read_timeout(Some(Duration::from_millis(10)))
				.expect("read timeout");
		}
		let addr = front.local_addr().expect("path address");
		let front = Arc::new(front);
		let back = Arc::new(back);
		let client: Arc<Mutex<Option<SocketAddr>>> = Arc::default();
		let mtu = Arc::new(AtomicUsize::new(mtu));
		let seen: Arc<[Seen; 2]> = Arc::default();
		let stop = Arc::new(AtomicBool::new(false));

		let up = {
			let (front, back, client, mtu, seen, stop) = (
				front.clone(),
				back.clone(),
				client.clone(),
				mtu.clone(),
				seen.clone(),
				stop.clone(),
			);
			std::thread::spawn(move || {
				let mut buf = vec![0u8; 64 * 1024];
				while !stop.load(Ordering::Relaxed) {
					let Ok((len, from)) = front.recv_from(&mut buf) else {
						continue;
					};
					*client.lock().unwrap() = Some(from);
					if len <= mtu.load(Ordering::Relaxed) && back.send(&buf[..len]).is_ok() {
						seen[0].forwarded(len);
					}
				}
			})
		};
		let down = {
			let (mtu, seen, stop) = (mtu.clone(), seen.clone(), stop.clone());
			std::thread::spawn(move || {
				let mut buf = vec![0u8; 64 * 1024];
				while !stop.load(Ordering::Relaxed) {
					let Ok(len) = back.recv(&mut buf) else {
						continue;
					};
					let Some(to) = *client.lock().unwrap() else {
						continue;
					};
					if len <= mtu.load(Ordering::Relaxed) && front.send_to(&buf[..len], to).is_ok() {
						seen[1].forwarded(len);
					}
				}
			})
		};

		Self {
			addr,
			mtu,
			seen,
			stop,
			threads: vec![up, down],
		}
	}

	fn set_mtu(&self, mtu: usize) {
		self.mtu.store(mtu, Ordering::Relaxed);
	}

	/// The largest datagram forwarded either way.
	fn largest(&self) -> usize {
		self.seen
			.iter()
			.map(|seen| seen.largest.load(Ordering::Relaxed))
			.max()
			.unwrap_or_default()
	}

	/// Datagrams larger than the floor forwarded toward the server.
	fn large_up(&self) -> u64 {
		self.seen[0].large.load(Ordering::Relaxed)
	}
}

impl Drop for Path {
	fn drop(&mut self) {
		self.stop.store(true, Ordering::Relaxed);
		for thread in self.threads.drain(..) {
			let _ = thread.join();
		}
	}
}

/// Await `future`, failing loudly rather than hanging if it never resolves.
async fn within<T>(handle: &moq_uring::Handle, what: &str, future: impl Future<Output = T>) -> T {
	let mut deadline = moq_uring::Timer::after(handle, Duration::from_secs(10));
	let mut future = std::pin::pin!(future);
	kio::wait(|waiter| {
		let mut cx = waiter.context();
		if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
			return Poll::Ready(Some(value));
		}
		deadline.poll(waiter).map(|()| None)
	})
	.await
	.unwrap_or_else(|| panic!("timed out waiting for {what}"))
}

/// A server endpoint and a dial-only client, both running `transport`, with
/// the client's dial going through a path of `mtu`.
fn endpoints(
	handle: &moq_uring::Handle,
	transport: &quic::transport::Config,
	mtu: usize,
) -> (quic::Endpoint, quic::Endpoint, quic::client::Config, Path) {
	let certs = support::certs().expect("certificates");
	let socket = |handle: &moq_uring::Handle| {
		handle
			.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
			.expect("socket")
	};
	let mut server_config = quic::server::Config::new(
		quic::Identity::open(&certs.cert, &certs.key)
			.expect("identity")
			.server(&[ALPN], quic::server::ClientAuth::None)
			.expect("tls"),
	);
	server_config.transport = transport.clone();
	let server = quic::Endpoint::new(
		socket(handle),
		quic::endpoint::Config::default().with_server(server_config),
	)
	.expect("server endpoint");
	let client = quic::Endpoint::new(socket(handle), quic::endpoint::Config::default()).expect("client endpoint");
	let path = Path::new(server.local_addr(), mtu);
	let mut dial = quic::client::Config::default();
	dial.alpn = vec![ALPN.to_string()];
	dial.verify = false;
	dial.transport = transport.clone();
	(server, client, dial, path)
}

/// Dial through the path and accept the far end.
async fn connect(
	handle: &moq_uring::Handle,
	server: &quic::Endpoint,
	client: &quic::Endpoint,
	dial: &quic::client::Config,
	remote: SocketAddr,
) -> (quic::Connection, quic::Connection) {
	let dialed = within(handle, "the handshake", client.connect(dial, remote, "localhost"))
		.await
		.expect("dial");
	let accepted = within(handle, "the accept", server.accept()).await.expect("accept");
	(dialed, accepted)
}

/// Send `len` bytes on a fresh stream from `from` and read them all at `to`.
async fn transfer(handle: &moq_uring::Handle, from: &mut quic::Connection, to: &mut quic::Connection, len: usize) {
	let mut send = from.open_uni(Default::default()).await.expect("open_uni");
	handle.spawn(async move {
		if send.write_all(bytes::Bytes::from(vec![7u8; len])).await.is_ok() {
			let _ = send.finish().stopped().await;
		}
	});

	let read = within(handle, "the transfer", async {
		let mut recv = to.accept_uni().await.expect("accept_uni");
		let mut buf = vec![0u8; 64 * 1024];
		let mut read = 0;
		while let Some(n) = recv.read(&mut &mut buf[..]).await.expect("read") {
			read += n;
		}
		read
	})
	.await;
	assert_eq!(read, len, "the whole stream arrived");
}

/// A path that carries the floor but not a byte more than IPv6 promises
/// connects and carries data.
#[test]
fn a_path_at_the_ipv6_minimum_connects() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let (server, client, dial, path) = endpoints(&handle, &quic::transport::Config::default(), IPV6_MIN);
	let remote = path.addr;

	worker
		.block_on(async move {
			let (mut dialed, mut accepted) = connect(&handle, &server, &client, &dial, remote).await;
			transfer(&handle, &mut dialed, &mut accepted, PAYLOAD).await;
			transfer(&handle, &mut accepted, &mut dialed, PAYLOAD).await;
		})
		.expect("worker");
	assert!(path.largest() <= IPV6_MIN);
}

/// Without discovery, nothing either way exceeds the floor, however much the
/// path would carry.
#[test]
fn discovery_stays_off_by_default() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let (server, client, dial, path) = endpoints(&handle, &quic::transport::Config::default(), ETHERNET);
	let remote = path.addr;

	worker
		.block_on(async move {
			let (mut dialed, mut accepted) = connect(&handle, &server, &client, &dial, remote).await;
			transfer(&handle, &mut dialed, &mut accepted, PAYLOAD).await;
			transfer(&handle, &mut accepted, &mut dialed, PAYLOAD).await;
		})
		.expect("worker");
	assert_eq!(path.largest(), FLOOR, "every full datagram sits at the floor");
}

/// Transfer until discovery has raised the MTU past the floor, so a train
/// staged after it packs the larger datagrams.
async fn discover(handle: &moq_uring::Handle, from: &mut quic::Connection, to: &mut quic::Connection, path: &Path) {
	for _ in 0..50 {
		transfer(handle, from, to, PAYLOAD / 16).await;
		if path.large_up() > 0 {
			return;
		}
	}
	panic!("discovery never sent past the floor");
}

/// With discovery on, a path that carries more than the floor gets it, and
/// trains keep batching at the larger size.
#[test]
fn discovery_finds_a_larger_mtu() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let mut transport = quic::transport::Config::default();
	transport.mtu_discovery = true;
	let (server, client, dial, path) = endpoints(&handle, &transport, ETHERNET);
	let remote = path.addr;

	let metrics = handle.metrics();
	let (path, (datagrams, sends)) = worker
		.block_on(async move {
			let (mut dialed, mut accepted) = connect(&handle, &server, &client, &dial, remote).await;
			discover(&handle, &mut dialed, &mut accepted, &path).await;

			let before = (metrics.snapshot(), path.large_up());
			transfer(&handle, &mut dialed, &mut accepted, PAYLOAD).await;
			let after = metrics.snapshot();
			// Most of the transfer rode above the floor.
			let large = path.large_up() - before.1;
			assert!(large as usize > PAYLOAD / ETHERNET / 2, "{large} large datagrams");
			(
				path,
				(
					after.tx_datagrams - before.0.tx_datagrams,
					after.tx_sends - before.0.tx_sends,
				),
			)
		})
		.expect("worker");
	assert!(path.largest() > FLOOR, "discovered {}", path.largest());
	assert!(path.largest() <= ETHERNET, "discovered {}", path.largest());
	assert!(datagrams > 2 * sends, "{datagrams} datagrams in {sends} sends");
}

/// A path that shrinks after discovery black-holes the larger datagrams, and
/// the connection falls back to the floor instead of stalling.
#[test]
fn a_shrinking_path_falls_back_to_the_floor() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let mut transport = quic::transport::Config::default();
	transport.mtu_discovery = true;
	let (server, client, dial, path) = endpoints(&handle, &transport, ETHERNET);
	let remote = path.addr;

	worker
		.block_on(async move {
			let (mut dialed, mut accepted) = connect(&handle, &server, &client, &dial, remote).await;
			discover(&handle, &mut dialed, &mut accepted, &path).await;

			path.set_mtu(IPV6_MIN);
			transfer(&handle, &mut dialed, &mut accepted, PAYLOAD).await;
			transfer(&handle, &mut accepted, &mut dialed, PAYLOAD).await;
		})
		.expect("worker");
}
