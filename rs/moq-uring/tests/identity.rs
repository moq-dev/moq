//! A socket names its worker, and everything built on it follows: an
//! endpoint runs where its socket was adopted, whichever handle is in scope,
//! and a socket whose worker is gone refuses to start anything.
//!
//! Kernel-gated: skips loudly below the Linux 6.12 floor (GitHub-hosted CI),
//! and runs everywhere else.

#![cfg(all(target_os = "linux", feature = "noq"))]

#[path = "support.rs"]
mod support;

use std::net::UdpSocket;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use moq_uring::{Config, Error, Worker, quic, udp};

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) => {
			eprintln!("skipping io_uring identity test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

const ALPN: &str = "moq-uring-identity";

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

fn socket(handle: &moq_uring::Handle) -> udp::Socket {
	handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("socket")
}

/// Poll `future` once, the way a caller that never drives a worker would.
fn poll_once<F: Future>(future: F) -> Poll<F::Output> {
	let mut future = std::pin::pin!(future);
	future.as_mut().poll(&mut Context::from_waker(Waker::noop()))
}

/// Building an endpoint on a socket whose worker has been dropped is refused
/// up front: nothing would ever run its demux, so a dial or accept through it
/// could only hang.
#[test]
fn an_endpoint_refuses_a_stopped_worker() {
	let Some(worker) = worker() else { return };
	let sock = socket(&worker.handle());
	drop(worker);

	let err = quic::Endpoint::new(sock, quic::endpoint::Config::default()).expect_err("endpoint on a stopped worker");
	assert!(
		matches!(err, quic::Error::Io(_)),
		"refused as a dead socket, got {err:?}"
	);
}

/// An endpoint that outlives its worker fails what it is asked for instead
/// of parking it forever, whether a dial or an accept.
#[test]
fn a_stopped_worker_fails_dials_and_accepts() {
	let Some(worker) = worker() else { return };
	let certs = support::certs().expect("certificates");
	let sock = socket(&worker.handle());
	let endpoint = quic::Endpoint::new(
		sock,
		quic::endpoint::Config::default().with_server(server_config(&certs)),
	)
	.expect("endpoint");
	let peer = endpoint.local_addr();
	drop(worker);

	match poll_once(endpoint.connect(&dial_config(peer))) {
		Poll::Ready(Err(quic::Error::Io(_))) => {}
		other => panic!("a dial on a stopped worker should fail at once, got {other:?}"),
	}
	match poll_once(endpoint.accept()) {
		Poll::Ready(Err(quic::Error::Io(_))) => {}
		other => panic!("an accept on a stopped worker should fail at once, got {other:?}"),
	}
}

/// Two workers on one thread: an endpoint on the second worker's socket makes
/// no progress while only the first is driven, however its futures are
/// polled, and completes once its own worker runs.
#[test]
fn an_endpoint_runs_on_the_worker_that_adopted_its_socket() {
	let Some(mut bystander) = worker() else { return };
	let mut owner = worker().expect("a second worker on the same thread");
	let certs = support::certs().expect("certificates");

	let sock = socket(&owner.handle());
	let endpoint = quic::Endpoint::new(
		sock,
		quic::endpoint::Config::default().with_server(server_config(&certs)),
	)
	.expect("endpoint");
	let addr = endpoint.local_addr();

	// A client on its own thread and worker, dialing the endpoint. Its
	// Initial reaches the socket immediately; only the owner can read it.
	// The client is driven until the server closes on it: its side of the
	// handshake completes before the server's, so it cannot stop earlier. It
	// reports in before dialing, so a setup failure fails here instead of as
	// an accept that never arrives.
	let (ready, started) = std::sync::mpsc::channel();
	let client = std::thread::spawn(move || {
		let mut worker = Worker::new(Config::default()).expect("client worker");
		let sock = socket(&worker.handle());
		ready.send(()).expect("test alive");
		worker
			.block_on(async move {
				let mut conn = quic::client::connect(sock, &dial_config(addr)).await.expect("dial");
				std::future::poll_fn(|cx| web_transport_trait::poll::Session::poll_closed(&mut conn, cx)).await
			})
			.expect("client loop")
	});
	started.recv().expect("the client thread failed to start");

	// Driving the bystander polls the accept from its loop, but the demux
	// that would feed it is a task on the owner, which is not running.
	let bystander_handle = bystander.handle();
	let stalled = bystander
		.block_on(async {
			let mut deadline = moq_uring::Timer::after(&bystander_handle, Duration::from_millis(500));
			let mut accept = std::pin::pin!(endpoint.accept());
			kio::wait(|waiter| {
				let mut cx = Context::from_waker(waiter.waker());
				if accept.as_mut().poll(&mut cx).is_ready() {
					return Poll::Ready(false);
				}
				deadline.poll(waiter).map(|()| true)
			})
			.await
		})
		.expect("bystander loop");
	assert!(stalled, "the bystander worker served an endpoint it does not own");

	owner
		.block_on(async {
			let mut accepted = endpoint.accept().await.expect("accept on the owner");
			assert_eq!(
				web_transport_trait::poll::Session::protocol(&accepted),
				Some(ALPN),
				"negotiated ALPN"
			);
			web_transport_trait::poll::Session::close(&mut accepted, 0, "done");
			std::future::poll_fn(|cx| web_transport_trait::poll::Session::poll_closed(&mut accepted, cx)).await;
		})
		.expect("owner loop");

	match client.join().expect("client thread") {
		quic::Error::App { code: 0, .. } => {}
		other => panic!("the client saw {other:?} instead of the server's close"),
	}
}
