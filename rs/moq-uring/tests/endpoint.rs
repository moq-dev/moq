//! Endpoint mechanics the session tests don't reach: one socket dialing and
//! accepting at once, version negotiation, and the dial-only refusal.
//!
//! Kernel-gated: skips loudly below the Linux 6.12 floor (GitHub-hosted CI),
//! and runs everywhere else.

#![cfg(target_os = "linux")]

#[path = "support/quiche.rs"]
mod support;

use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use moq_uring::{Config, Error, Worker, quic, udp};
use web_transport_trait::poll::{RecvStream as _, SendStream as _, Session as _};

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) => {
			eprintln!("skipping io_uring endpoint test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

const ALPN: &str = "moq-uring-test";

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

/// One socket carries dials and accepts at once: two endpoints dial each
/// other, and each also accepts the other's dial. This is the relay cluster
/// shape (a worker's socket serves inbound sessions and upstream dials).
#[test]
fn dial_and_accept_share_one_socket() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");

	let endpoint = |handle: &moq_uring::Handle| {
		let sock = handle
			.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
			.expect("socket");
		quic::Endpoint::new(
			handle,
			sock,
			quic::endpoint::Config::default().with_server(server_config(&certs)),
		)
		.expect("endpoint")
	};
	let a = endpoint(&handle);
	let b = endpoint(&handle);

	worker
		.block_on(async move {
			let a_to_b = a.connect(&dial_config(b.local_addr())).await.expect("a dials b");
			let b_in = b.accept().await.expect("b accepts a");
			let b_to_a = b.connect(&dial_config(a.local_addr())).await.expect("b dials a");
			let a_in = a.accept().await.expect("a accepts b");

			for conn in [&a_to_b, &b_in, &b_to_a, &a_in] {
				assert_eq!(
					web_transport_trait::poll::Session::protocol(conn),
					Some(ALPN),
					"negotiated ALPN"
				);
			}
		})
		.expect("worker");
}

/// A server negotiates an unsupported version, while a dial-only endpoint and
/// junk stay silent.
#[test]
fn unsupported_version_is_negotiated_only_by_servers() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");

	let sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("socket");
	let endpoint = quic::Endpoint::new(
		&handle,
		sock,
		quic::endpoint::Config::default().with_server(server_config(&certs)),
	)
	.expect("endpoint");
	let server = endpoint.local_addr();
	let dial_only_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("dial-only socket");
	let dial_only =
		quic::Endpoint::new(&handle, dial_only_sock, quic::endpoint::Config::default()).expect("dial-only endpoint");
	let dial_only_addr = dial_only.local_addr();

	// The raw client runs on its own thread (the worker owns this one) and
	// wakes the worker when it has a verdict.
	#[derive(Default)]
	struct Verdict {
		result: Option<anyhow::Result<Vec<u8>>>,
		waker: Option<std::task::Waker>,
	}
	let verdict: Arc<Mutex<Verdict>> = Arc::new(Mutex::new(Verdict::default()));
	let thread_verdict = verdict.clone();
	let probe = std::thread::spawn(move || {
		let result = (|| -> anyhow::Result<Vec<u8>> {
			let sock = UdpSocket::bind("127.0.0.1:0")?;
			sock.set_read_timeout(Some(Duration::from_secs(5)))?;

			// Junk first: it must be ignored, not answered or fatal.
			sock.send_to(&[0u8; 64], server)?;

			// The long-header type bits are version-specific. Use bits that mean
			// 0-RTT in v1 to prove we negotiate before interpreting them.
			let mut packet = Vec::new();
			packet.push(0xD0); // long header, fixed bit, unknown-version type
			packet.extend_from_slice(&0x0a0a_0a0au32.to_be_bytes());
			packet.push(8); // dcid
			packet.extend_from_slice(&[0xAB; 8]);
			packet.push(8); // scid
			packet.extend_from_slice(&[0xCD; 8]);
			packet.push(0); // empty token
			packet.resize(1200, 0);
			sock.send_to(&packet, server)?;

			let mut response = vec![0u8; 1500];
			let (len, _) = sock.recv_from(&mut response)?;
			response.truncate(len);

			// A dial-only socket owns no inbound handshake policy and must not
			// spend its shared transmit pool on stateless responses.
			sock.set_read_timeout(Some(Duration::from_millis(200)))?;
			sock.send_to(&packet, dial_only_addr)?;
			let mut unexpected = [0u8; 1500];
			match sock.recv_from(&mut unexpected) {
				Err(err)
					if matches!(
						err.kind(),
						std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
					) => {}
				Err(err) => return Err(err.into()),
				Ok((len, from)) => anyhow::bail!("dial-only endpoint answered {len} bytes from {from}"),
			}
			Ok(response)
		})();
		let mut verdict = thread_verdict.lock().unwrap();
		verdict.result = Some(result);
		if let Some(waker) = verdict.waker.take() {
			waker.wake();
		}
	});

	let mut response = worker
		.block_on(std::future::poll_fn(move |cx| {
			let mut verdict = verdict.lock().unwrap();
			if let Some(result) = verdict.result.take() {
				return Poll::Ready(result);
			}
			verdict.waker = Some(cx.waker().clone());
			Poll::Pending
		}))
		.expect("worker")
		.expect("version negotiation response");
	probe.join().expect("probe thread");

	let hdr = quiche::Header::from_slice(&mut response, 0).expect("parse response");
	assert_eq!(hdr.ty, quiche::Type::VersionNegotiation);
	let versions = hdr.versions.expect("advertised versions");
	assert!(
		versions.contains(&quiche::PROTOCOL_VERSION),
		"the supported version is offered: {versions:?}"
	);
	drop((endpoint, dial_only));
}

/// A dial nobody answers must time out rather than hang: with no ingress ever
/// arriving, the driver's own deadline is the only thing that can wake it, so
/// this fails if the deadline is armed without a registered waiter (the driver
/// must arm before polling, not after).
#[test]
fn an_unanswered_dial_times_out() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();

	// A bound socket nobody reads: the Initial goes nowhere.
	let hole = UdpSocket::bind("127.0.0.1:0").expect("bind");
	let client_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("client socket");
	let mut dial = dial_config(hole.local_addr().expect("addr"));
	// quiche stretches this to 3x the initial probe timeout (RFC 9000 §10.1),
	// so the dial resolves in about three seconds.
	dial.transport.idle_timeout = Duration::from_millis(500);

	let result = worker
		.block_on(async move { quic::client::connect(&handle, client_sock, &dial).await })
		.expect("worker");
	assert!(result.is_err(), "a dial into a black hole must time out");
}

/// The handshake backlog bounds what an unauthenticated Initial can allocate:
/// past it, Initials are dropped before any per-connection state exists. Zero
/// is the forcing value, refusing every handshake outright.
#[test]
fn the_backlog_bounds_pending_handshakes() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");

	let sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("socket");
	let mut config = quic::endpoint::Config::default().with_server(server_config(&certs));
	config.backlog = 0;
	let endpoint = quic::Endpoint::new(&handle, sock, config).expect("endpoint");
	let server = endpoint.local_addr();

	let accepted = std::rc::Rc::new(std::cell::Cell::new(false));
	let accept_flag = accepted.clone();
	handle.spawn(async move {
		if endpoint.accept().await.is_ok() {
			accept_flag.set(true);
		}
	});

	let client_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("client socket");
	let mut dial = dial_config(server);
	// The server never answers, so the dial ends at the idle timeout; keep the
	// test short.
	dial.transport.idle_timeout = Duration::from_millis(500);

	let result = worker
		.block_on(async move { quic::client::connect(&handle, client_sock, &dial).await })
		.expect("worker");
	assert!(result.is_err(), "a handshake over the backlog must not complete");
	assert!(!accepted.get(), "a handshake over the backlog must not be accepted");
}

/// Completed connections stay in the backlog until the application accepts
/// them, so finishing a handshake cannot make room for an unbounded queue.
#[test]
fn the_backlog_bounds_queued_connections() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");

	let sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("socket");
	let mut config = quic::endpoint::Config::default().with_server(server_config(&certs));
	config.backlog = 1;
	let endpoint = quic::Endpoint::new(&handle, sock, config).expect("endpoint");
	let server = endpoint.local_addr();

	let first_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("first client socket");
	let first = worker
		.block_on(async { quic::client::connect(&handle, first_sock, &dial_config(server)).await })
		.expect("worker")
		.expect("first connection");

	let second_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("second client socket");
	let mut second_config = dial_config(server);
	second_config.transport.idle_timeout = Duration::from_millis(500);
	let second = worker
		.block_on(async { quic::client::connect(&handle, second_sock, &second_config).await })
		.expect("worker");
	assert!(second.is_err(), "a completed connection must still occupy the backlog");

	let accepted = worker
		.block_on(endpoint.accept())
		.expect("worker")
		.expect("first accepted connection");
	drop((first, accepted));
}

/// One busy connection cannot monopolize a shared transmit pool and starve a
/// later connection's handshake. One buffer keeps the pool contended.
#[test]
fn connections_share_one_tx_buffer_fairly() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");

	let mut udp_config = udp::Config::default();
	udp_config.tx_buffers = 1;
	let sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp_config)
		.expect("socket");
	let endpoint = quic::Endpoint::new(
		&handle,
		sock,
		quic::endpoint::Config::default().with_server(server_config(&certs)),
	)
	.expect("endpoint");
	let server = endpoint.local_addr();

	worker
		.block_on(async {
			let first_sock = handle
				.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
				.expect("first client socket");
			let first_client = quic::client::connect(&handle, first_sock, &dial_config(server))
				.await
				.expect("first connection");
			let mut first_server = endpoint.accept().await.expect("first accepted connection");

			let mut send = std::future::poll_fn(|cx| first_server.poll_open_uni(cx))
				.await
				.expect("open busy stream");
			let running = std::rc::Rc::new(std::cell::Cell::new(true));
			#[derive(Default)]
			struct Progress {
				bytes: usize,
				waker: Option<std::task::Waker>,
			}
			let progress = std::rc::Rc::new(std::cell::RefCell::new(Progress::default()));
			let send_running = running.clone();
			let send_progress = progress.clone();
			handle.spawn(async move {
				let chunk = vec![0x5a; 64 * 1024];
				while send_running.get() {
					let mut offset = 0;
					while offset < chunk.len() {
						match std::future::poll_fn(|cx| send.poll_write(cx, &chunk[offset..])).await {
							Ok(n) => offset += n,
							// The reader drops its half as the test winds down, which
							// stops this stream mid-write. Only a stop *before* that is
							// a real failure.
							Err(_) if !send_running.get() => return,
							Err(err) => panic!("write busy stream: {err}"),
						}
					}
					let mut progress = send_progress.borrow_mut();
					progress.bytes += chunk.len();
					if let Some(waker) = progress.waker.take() {
						waker.wake();
					}
				}
			});
			let recv_running = running.clone();
			handle.spawn(async move {
				let mut first_client = first_client;
				let mut recv = std::future::poll_fn(|cx| first_client.poll_accept_uni(cx))
					.await
					.expect("accept busy stream");
				let mut chunk = vec![0; 64 * 1024];
				while recv_running.get() {
					if std::future::poll_fn(|cx| recv.poll_read(cx, &mut chunk))
						.await
						.expect("read busy stream")
						.is_none()
					{
						break;
					}
				}
			});
			std::future::poll_fn(|cx| {
				let mut progress = progress.borrow_mut();
				if progress.bytes >= 4 * 1024 * 1024 {
					return Poll::Ready(());
				}
				progress.waker = Some(cx.waker().clone());
				Poll::Pending
			})
			.await;

			let second_sock = handle
				.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
				.expect("second client socket");
			let mut second_config = dial_config(server);
			second_config.transport.idle_timeout = Duration::from_millis(500);
			let second = quic::client::connect(&handle, second_sock, &second_config).await;
			running.set(false);
			let second = second.expect("the second connection must not starve");
			let accepted = endpoint.accept().await.expect("second accepted connection");
			drop((second, accepted));
		})
		.expect("worker");
}

/// A dial-only endpoint refuses to accept, loudly.
#[test]
fn accept_needs_a_server_config() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();

	let sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("socket");
	let endpoint = quic::Endpoint::new(&handle, sock, quic::endpoint::Config::default()).expect("endpoint");

	let result = worker.block_on(async move { endpoint.accept().await }).expect("worker");
	assert!(matches!(result, Err(quic::Error::NotServer)), "got {result:?}");
}

/// One [`quic::Identity`] serves every endpoint built from it, without ever
/// touching the files again.
///
/// A worker group builds its endpoints on the worker threads, so re-reading
/// the certificate per endpoint means a certificate replaced on disk while
/// those threads are starting leaves earlier and later workers presenting
/// different identities, and a client pinning either one fails depending on
/// which worker accepts it.
#[test]
fn an_identity_is_read_once() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");
	let config = server_config(&certs);

	// Whatever the endpoints below present, it did not come from here.
	std::fs::remove_file(&certs.cert).expect("remove cert");
	std::fs::remove_file(&certs.key).expect("remove key");

	let listener = |handle: &moq_uring::Handle| {
		let sock = handle
			.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
			.expect("socket");
		quic::Endpoint::new(
			handle,
			sock,
			quic::endpoint::Config::default().with_server(config.clone()),
		)
		.expect("endpoint")
	};

	// Two endpoints, as two workers would build them, then a real handshake
	// against the second: the identity has to survive being cloned as well as
	// being read.
	let _first = listener(&handle);
	let second = listener(&handle);
	let server = second.local_addr();

	worker
		.block_on(async move {
			let sock = handle
				.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
				.expect("client socket");
			let dial = handle.clone();
			handle.spawn(async move {
				quic::client::connect(&dial, sock, &dial_config(server))
					.await
					.expect("dial");
			});
			second.accept().await.expect("accepted connection");
		})
		.expect("worker");
}
