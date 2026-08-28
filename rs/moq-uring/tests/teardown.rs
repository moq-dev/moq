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
//! Kernel-gated: skips loudly below the Linux 6.12 floor (GitHub-hosted CI),
//! and runs everywhere else.

#![cfg(target_os = "linux")]

#[path = "support/quiche.rs"]
mod support;

use std::net::UdpSocket;
use std::time::Duration;

use moq_uring::{Config, Error, Worker, quic, udp};
use web_transport_trait::poll::Session as _;

const ALPN: &str = "moq-uring-teardown";
const CLOSE_CODE: u32 = 42;
const CLOSE_REASON: &str = "done";

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) => {
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
			tx.send(3, peer_addr, 3).expect("send");
		})
		.expect("worker");
	drop(sock);
	drop(worker);

	let mut buf = [0u8; 16];
	let (len, _) = peer.recv_from(&mut buf).expect("the staged send reached the wire");
	assert_eq!(&buf[..len], b"bye");
}

fn server_config(certs: &support::Certs) -> quic::server::Config {
	let mut config = quic::server::Config::new(quic::Identity::open(&certs.cert, &certs.key).expect("identity"));
	config.alpn = vec![ALPN.to_string()];
	config
}

fn dial_config(peer: std::net::SocketAddr) -> quic::client::Config {
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
	let Some(mut client_worker) = worker() else { return };
	let certs = support::certs().expect("certificates");

	let server_sock = UdpSocket::bind("127.0.0.1:0").expect("bind server");
	let server_addr = server_sock.local_addr().expect("server addr");

	// The server outlives the client's teardown, so its verdict is only about
	// what the client managed to send.
	let server = std::thread::spawn(move || -> quic::Error {
		let mut worker = Worker::new(Config::default()).expect("server worker");
		let handle = worker.handle();
		let sock = handle.udp(server_sock, udp::Config::default()).expect("server socket");
		let endpoint = quic::Endpoint::new(
			&handle,
			sock,
			quic::endpoint::Config::default().with_server(server_config(&certs)),
		)
		.expect("endpoint");
		worker
			.block_on(async move {
				let mut conn = endpoint.accept().await.expect("accept");
				std::future::poll_fn(|cx| conn.poll_closed(cx)).await
			})
			.expect("server worker")
	});

	let handle = client_worker.handle();
	let sock = handle
		.udp(
			UdpSocket::bind("127.0.0.1:0").expect("bind client"),
			udp::Config::default(),
		)
		.expect("client socket");
	let endpoint = quic::Endpoint::new(&handle, sock, quic::endpoint::Config::default()).expect("client endpoint");

	client_worker
		.block_on(async move {
			let mut conn = endpoint.connect(&dial_config(server_addr)).await.expect("dial");
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
