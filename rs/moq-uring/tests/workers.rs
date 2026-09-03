//! A steered thread-per-core group, end to end: two workers on two threads,
//! each with its own socket in one `SO_REUSEPORT` group and an endpoint
//! issuing steering-prefixed connection ids, serving clients that dial the
//! shared port. Whichever worker the Initial hashes to owns the connection,
//! and the prefix keeps every later packet (handshake continuation included)
//! on that worker; a wrong prefix stalls the handshake, so every dial
//! completing is what proves the steering.
//!
//! Kernel-gated: skips loudly below the Linux 6.12 floor (GitHub-hosted CI),
//! and runs everywhere else.

#![cfg(all(target_os = "linux", any(feature = "noq", feature = "quiche", feature = "quinn")))]

#[path = "support/quiche.rs"]
mod support;

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use moq_sock::shard::Shard;
use moq_uring::{Config, Error, Worker, quic, udp};

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) => {
			eprintln!("skipping io_uring workers test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

const ALPN: &str = "moq-uring-workers";
const WORKERS: u16 = 2;
/// Enough dials that both workers land some with near certainty: each
/// Initial's placement hashes a random byte, so all landing on one side is a
/// `2^-31` event. Steering cannot be asserted deterministically from here,
/// since the client picks its own connection id; the count is what keeps the
/// false failure below every other source of noise in the suite.
const DIALS: usize = 32;

/// A stop signal a worker parks on, wakeable from another thread.
#[derive(Default)]
struct Stop {
	stopped: AtomicBool,
	waker: Mutex<Option<std::task::Waker>>,
}

impl Stop {
	async fn wait(self: &Arc<Self>) {
		std::future::poll_fn(|cx| {
			if self.stopped.load(Ordering::Acquire) {
				return Poll::Ready(());
			}
			*self.waker.lock().unwrap() = Some(cx.waker().clone());
			Poll::Pending
		})
		.await
	}

	fn stop(&self) {
		self.stopped.store(true, Ordering::Release);
		if let Some(waker) = self.waker.lock().unwrap().take() {
			waker.wake();
		}
	}
}

#[test]
fn a_steered_group_serves_a_shared_port() {
	// Gate on the kernel before spawning anything.
	let Some(client_worker) = worker() else { return };
	let certs = support::certs().expect("certificates");

	// Bind the group up front, in index order: that order is the identity the
	// kernel steers by, and binding before any thread spawns is what
	// guarantees it.
	let mut members = Vec::new();
	let mut addr: SocketAddr = "127.0.0.1:0".parse().expect("addr");
	for index in 0..WORKERS {
		let shard = Shard::new(index, WORKERS).expect("shard");
		let socket = moq_sock::shard::bind(addr, Some(shard)).expect("bind group member");
		addr = socket.local_addr().expect("addr");
		members.push((shard, socket));
	}

	let accepted: Arc<Vec<AtomicUsize>> = Arc::new((0..WORKERS).map(|_| AtomicUsize::new(0)).collect());
	// One stop each: the wakers are per-thread, so a shared slot would let one
	// worker's registration clobber the other's.
	let stops: Vec<Arc<Stop>> = (0..WORKERS).map(|_| Arc::new(Stop::default())).collect();

	let threads: Vec<_> = members
		.into_iter()
		.zip(&stops)
		.map(|((shard, socket), stop)| {
			let accepted = accepted.clone();
			let stop = stop.clone();
			let cert = certs.cert.clone();
			let key = certs.key.clone();
			std::thread::spawn(move || {
				let mut worker = Worker::new(Config::default()).expect("worker");
				let handle = worker.handle();
				let socket = handle.udp(socket, udp::Config::default()).expect("socket");

				let mut server = quic::server::Config::new(quic::Identity::open(cert, key).expect("identity"));
				server.alpn = vec![ALPN.to_string()];
				let endpoint = quic::Endpoint::new(
					&handle,
					socket,
					quic::endpoint::Config::default().with_server(server).with_shard(shard),
				)
				.expect("endpoint");

				handle.spawn(async move {
					// Accepted connections are dropped once counted; the
					// client is what closes them.
					while endpoint.accept().await.is_ok() {
						accepted[usize::from(shard.index())].fetch_add(1, Ordering::AcqRel);
					}
				});
				worker.block_on(stop.wait()).expect("worker loop");
			})
		})
		.collect();

	// Dial the shared port repeatedly from one client worker. Every handshake
	// completing is the steering assertion (see the module docs).
	let mut client_worker = client_worker;
	let handle = client_worker.handle();
	let mut dial = quic::client::Config::new(addr, "localhost");
	dial.alpn = vec![ALPN.to_string()];
	dial.verify = false;

	client_worker
		.block_on(async {
			for _ in 0..DIALS {
				let socket = handle
					.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
					.expect("client socket");
				let mut conn = quic::client::connect(&handle, socket, &dial).await.expect("connect");
				assert_eq!(
					web_transport_trait::poll::Session::protocol(&conn),
					Some(ALPN),
					"negotiated ALPN"
				);
				web_transport_trait::poll::Session::close(&mut conn, 0, "done");
			}

			// The server side counts a connection when its accept loop takes
			// it, which can trail the client's handshake; wait for the tally.
			let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
			loop {
				let total: usize = accepted.iter().map(|count| count.load(Ordering::Acquire)).sum();
				if total == DIALS {
					break;
				}
				assert!(
					std::time::Instant::now() < deadline,
					"only {total} of {DIALS} dials were accepted"
				);
				moq_net::runtime::Deadline::after(&handle, std::time::Duration::from_millis(10))
					.wait()
					.await;
			}
		})
		.expect("client worker");

	// Every member has to have been fed, or the group is steering into a
	// subset and the rest sit idle.
	for (index, count) in accepted.iter().enumerate() {
		assert!(count.load(Ordering::Acquire) > 0, "worker {index} accepted nothing");
	}

	for stop in &stops {
		stop.stop();
	}
	for thread in threads {
		thread.join().expect("worker thread");
	}
}
