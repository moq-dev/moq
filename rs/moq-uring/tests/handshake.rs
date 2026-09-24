//! A dial resolves only once its final handshake flight is staged, so a
//! client that stops its worker the moment the dial resolves strands no peer.
//!
//! Kernel-gated: skips loudly below the Linux 6.12 floor (GitHub-hosted CI),
//! and runs everywhere else.

#![cfg(all(target_os = "linux", feature = "noq"))]

#[path = "support.rs"]
mod support;

use std::net::{SocketAddr, UdpSocket};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use moq_uring::{Config, Error, Worker, quic, udp};

const ALPN: &str = "moq-uring-handshake";

/// One-shot dials per test, each on a worker dropped as soon as it resolves.
const DIALS: usize = 20;

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) => {
			eprintln!("skipping io_uring handshake test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

fn socket(handle: &moq_uring::Handle) -> udp::Socket {
	handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("socket")
}

/// A client certificate, and the root file the server verifies it against.
struct Client {
	identity: quic::Identity,
	root: std::path::PathBuf,
	_dir: tempfile::TempDir,
}

/// A self-signed client certificate of about 20 KB: more than the initial
/// congestion window, so congestion control splits the final flight across
/// round trips, and pacing splits it again on a slow link.
fn big_client() -> Client {
	let names: Vec<String> = (0..600).map(|i| format!("client-{i:04}.padding.example.com")).collect();
	let signed = rcgen::generate_simple_self_signed(names).expect("client certificate");
	let dir = tempfile::tempdir().expect("tempdir");
	let root = dir.path().join("client.pem");
	std::fs::write(&root, signed.cert.pem()).expect("write root");
	Client {
		identity: quic::Identity::from_pem(signed.cert.pem(), signed.signing_key.serialize_pem()),
		root,
		_dir: dir,
	}
}

/// Serve on a thread of its own, counting the handshakes that complete until
/// `quiet` passes without one.
fn serve(
	congestion: quic::Congestion,
	client_root: Option<std::path::PathBuf>,
	quiet: Duration,
) -> (SocketAddr, std::thread::JoinHandle<usize>) {
	let certs = support::certs().expect("certificates");
	let identity = quic::Identity::open(&certs.cert, &certs.key).expect("identity");
	let sock = UdpSocket::bind("127.0.0.1:0").expect("bind server");
	let addr = sock.local_addr().expect("server addr");
	let thread = std::thread::spawn(move || {
		let mut worker = Worker::new(Config::default()).expect("server worker");
		let handle = worker.handle();
		let sock = handle.udp(sock, udp::Config::default()).expect("server socket");
		let mut config = quic::server::Config::new(identity);
		config.alpn = vec![ALPN.to_string()];
		config.transport.congestion = congestion;
		if let Some(root) = client_root {
			config.client_auth = quic::server::ClientAuth::Required(vec![root]);
		}
		let endpoint =
			quic::Endpoint::new(sock, quic::endpoint::Config::default().with_server(config)).expect("endpoint");
		worker
			.block_on(async move {
				let mut accepted = Vec::new();
				loop {
					let mut deadline = moq_uring::Timer::after(&handle, quiet);
					let mut accept = std::pin::pin!(endpoint.accept());
					let conn = kio::wait(|waiter| {
						let mut cx = Context::from_waker(waiter.waker());
						if let Poll::Ready(conn) = accept.as_mut().poll(&mut cx) {
							return Poll::Ready(Some(conn.expect("accept")));
						}
						deadline.poll(waiter).map(|()| None)
					})
					.await;
					match conn {
						Some(conn) => accepted.push(conn),
						None => return accepted.len(),
					}
				}
			})
			.expect("server worker")
	});
	(addr, thread)
}

/// The one-shot client shape: dial, then drop the worker the moment the dial
/// resolves. The server must still finish every handshake, rather than time
/// out on a client that believes it is connected.
fn stop_after_dial(congestion: quic::Congestion, client: Option<Client>) {
	if worker().is_none() {
		return;
	}
	let root = client.as_ref().map(|client| client.root.clone());
	let (addr, server) = serve(congestion, root, Duration::from_secs(2));

	let mut config = quic::client::Config::new(addr, "localhost");
	config.alpn = vec![ALPN.to_string()];
	config.verify = false;
	config.transport.congestion = congestion;
	config.identity = client.as_ref().map(|client| client.identity.clone());

	for _ in 0..DIALS {
		let mut worker = worker().expect("client worker");
		let sock = socket(&worker.handle());
		worker
			.block_on(quic::client::connect(sock, &config))
			.expect("client worker")
			.expect("dial");
		drop(worker);
	}

	let accepted = server.join().expect("server thread");
	assert_eq!(accepted, DIALS, "handshakes the server finished");
}

#[test]
fn a_stopped_dial_finishes_the_server_handshake() {
	stop_after_dial(quic::Congestion::Loss, None);
	stop_after_dial(quic::Congestion::Delay, None);
}

/// A final flight too big to leave in one burst: the dial waits for all of
/// it, not just the part congestion control and pacing let out first.
#[test]
fn a_stopped_dial_sends_all_of_a_split_final_flight() {
	stop_after_dial(quic::Congestion::Loss, Some(big_client()));
	stop_after_dial(quic::Congestion::Delay, Some(big_client()));
}

/// A worker dropped mid-handshake fails the dial rather than leaving it
/// pending on a flight nothing will ever send.
#[test]
fn a_worker_stopped_mid_dial_fails_it() {
	let Some(worker) = worker() else { return };
	// A peer that never answers, so the handshake cannot finish first.
	let silent = UdpSocket::bind("127.0.0.1:0").expect("bind peer");
	let mut config = quic::client::Config::new(silent.local_addr().expect("peer addr"), "localhost");
	config.alpn = vec![ALPN.to_string()];
	config.verify = false;

	let endpoint = quic::Endpoint::new(socket(&worker.handle()), quic::endpoint::Config::default()).expect("endpoint");
	let mut dial = std::pin::pin!(endpoint.connect(&config));
	let mut cx = Context::from_waker(Waker::noop());
	assert!(dial.as_mut().poll(&mut cx).is_pending(), "the dial is under way");

	drop(worker);
	match dial.as_mut().poll(&mut cx) {
		Poll::Ready(Err(quic::Error::Io(_))) => {}
		other => panic!("a dial whose worker stopped should fail, got {other:?}"),
	}
}
