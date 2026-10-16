//! What survives a worker that stops the moment its work is handed over.
//!
//! Both halves of the send path stage rather than syscall: `TxBuf::send`
//! stages an SQE and `Shared::push` only enters the ring when the submission
//! queue fills, so a datagram handed to a socket can still be sitting in
//! userspace when the caller returns from `block_on`. Dropping the worker is
//! what has to close that window, and these pin it shut at both layers: a raw
//! datagram, and a QUIC close whose CONNECTION_CLOSE is the last thing a
//! one-shot client ever sends.
//!
//! Unsupported local kernels skip loudly; MOQ_STRICT makes CI refuse them.

#![cfg(all(target_os = "linux", feature = "noq"))]

#[path = "support.rs"]
mod support;

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use moq_uring::{Config, Error, Worker, quic, udp};
use web_transport_trait::poll::Session as _;

const ALPN: &str = "moq-uring-teardown";
const CLOSE_CODE: u32 = 42;
const CLOSE_REASON: &str = "done";

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) if std::env::var_os("MOQ_STRICT").is_none() => {
			eprintln!("skipping io_uring teardown test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

/// A datagram staged by the last turn before `block_on` returns still reaches
/// the wire: the worker's drop submits what is left staged and waits for the
/// completions.
#[test]
fn a_send_staged_on_the_way_out_still_goes() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();

	let peer = UdpSocket::bind("127.0.0.1:0").expect("bind peer");
	peer.set_read_timeout(Some(Duration::from_secs(5)))
		.expect("read timeout");
	let peer_addr = peer.local_addr().expect("peer addr");

	let sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("socket");

	// Nothing re-enters the ring after this: the future resolves on the same
	// turn that stages the send, and `block_on` returns without pumping.
	worker
		.block_on(async {
			let mut tx = sock.acquire().await.expect("acquire");
			tx[..3].copy_from_slice(b"bye");
			tx.send(udp::Transmit {
				to: peer_addr,
				len: 3,
				segment: 3,
				ecn: None,
			})
			.expect("send");
		})
		.expect("worker");
	drop(sock);
	drop(worker);

	let mut buf = [0u8; 16];
	let (len, _) = peer.recv_from(&mut buf).expect("the staged send reached the wire");
	assert_eq!(&buf[..len], b"bye");
}

fn server_config(certs: &support::Certs) -> quic::server::Config {
	quic::server::Config::new(
		quic::Identity::open(&certs.cert, &certs.key)
			.expect("identity")
			.server(&[ALPN], quic::server::ClientAuth::None)
			.expect("tls"),
	)
}

fn dial_config(peer: SocketAddr) -> quic::client::Config {
	let mut config = quic::client::Config::new(peer, "localhost");
	config.alpn = vec![ALPN.to_string()];
	config.verify = false;
	config
}

/// The one-shot client shape: dial, close, wait for the close to be published,
/// then stop the runtime outright. The peer must learn the application code
/// rather than idle out, which is what a lost CONNECTION_CLOSE would look
/// like.
#[test]
fn a_published_close_has_already_left_the_client() {
	close_right_after_dialing(None);
}

/// The same, with the client's Finished held back by its pacer, which is how
/// a loaded host first hit it. The slow return leg makes the client's first
/// RTT sample long, so its pacer refills slowly: the padded ACK it sends for
/// the server's first datagram drains it, and the Finished owed once the
/// second arrives has to wait. A close in that gap must not replace the
/// Finished, or the server never completes its side, sees only a
/// handshake-level close stripped of the application code, and never hands
/// the connection to `accept`.
#[test]
fn a_close_right_after_dialing_follows_the_handshake() {
	close_right_after_dialing(Some(Duration::from_millis(100)));
}

fn close_right_after_dialing(slow_return: Option<Duration>) {
	let Some(mut client_worker) = worker() else { return };
	let certs = support::certs().expect("certificates");

	let server_sock = UdpSocket::bind("127.0.0.1:0").expect("bind server");
	let server_addr = server_sock.local_addr().expect("server addr");
	let dial_addr = match slow_return {
		Some(delay) => slow_return_path(server_addr, delay),
		None => server_addr,
	};

	// The server outlives the client's teardown, so its verdict is only about
	// what the client managed to send. It reports in once serving, so a setup
	// failure fails here instead of as a dial that idles out.
	let (ready, started) = std::sync::mpsc::channel();
	let server = std::thread::spawn(move || -> quic::Error {
		let mut worker = Worker::new(Config::default()).expect("server worker");
		let handle = worker.handle();
		let sock = handle.udp(server_sock, udp::Config::default()).expect("server socket");
		let endpoint = quic::Endpoint::new(
			sock,
			quic::endpoint::Config::default().with_server(server_config(&certs)),
		)
		.expect("endpoint");
		ready.send(()).expect("test alive");
		worker
			.block_on(async move {
				let mut conn = endpoint.accept().await.expect("accept");
				std::future::poll_fn(|cx| conn.poll_closed(cx)).await
			})
			.expect("server worker")
	});
	started.recv().expect("the server thread failed to start");

	let handle = client_worker.handle();
	let sock = handle
		.udp(
			UdpSocket::bind("127.0.0.1:0").expect("bind client"),
			udp::Config::default(),
		)
		.expect("client socket");
	let endpoint = quic::Endpoint::new(sock, quic::endpoint::Config::default()).expect("client endpoint");

	client_worker
		.block_on(async move {
			let mut conn = endpoint.connect(&dial_config(dial_addr)).await.expect("dial");
			conn.close(CLOSE_CODE, CLOSE_REASON);
			std::future::poll_fn(|cx| conn.poll_closed(cx)).await;
		})
		.expect("client worker");
	drop(client_worker);

	let err = server.join().expect("server thread");
	assert!(
		matches!(&err, quic::Error::App { code, reason } if *code == u64::from(CLOSE_CODE) && reason == CLOSE_REASON),
		"the server saw {err} instead of the application close"
	);
}

/// A UDP relay to `server` that passes client datagrams straight through and
/// holds each server datagram for `delay`. Returns the address to dial; its
/// threads live as long as the test process.
///
/// Released server datagrams are spaced a little apart, so the client handles
/// each on a turn of its own rather than as one batch, but closer than its
/// pacer refills at an RTT of `delay`.
fn slow_return_path(server: SocketAddr, delay: Duration) -> SocketAddr {
	const SPACING: Duration = Duration::from_millis(2);

	let relay = UdpSocket::bind("127.0.0.1:0").expect("bind relay");
	let addr = relay.local_addr().expect("relay addr");
	let out = relay.try_clone().expect("clone relay");
	let (hold, release) = std::sync::mpsc::channel::<(Instant, Vec<u8>, SocketAddr)>();

	std::thread::spawn(move || {
		let mut client = None;
		let mut buf = vec![0u8; u16::MAX as usize];
		while let Ok((len, from)) = relay.recv_from(&mut buf) {
			if from != server {
				client = Some(from);
				relay.send_to(&buf[..len], server).expect("relay to server");
			} else if let Some(client) = client
				&& hold
					.send((Instant::now() + delay, buf[..len].to_vec(), client))
					.is_err()
			{
				return;
			}
		}
	});
	std::thread::spawn(move || {
		let mut next = Instant::now();
		for (at, datagram, client) in release {
			next = next.max(at);
			std::thread::sleep(next.saturating_duration_since(Instant::now()));
			out.send_to(&datagram, client).expect("relay to client");
			next += SPACING;
		}
	});
	addr
}
