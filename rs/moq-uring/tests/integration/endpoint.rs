//! Endpoint mechanics the session tests don't reach: one socket dialing and
//! accepting at once, version negotiation, the dial-only refusal, and how a
//! close is reported.
//!
//! Unsupported local kernels skip loudly; CI_STRICT makes CI refuse them.

#![cfg(all(target_os = "linux", feature = "quic"))]

use crate::support;

use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use moq_uring::{Config, Error, Worker, quic, udp};
use web_transport_trait::poll::{SendStream as _, Session as _};

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) if std::env::var_os("CI_STRICT").is_none() => {
			eprintln!("skipping io_uring endpoint test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

const ALPN: &str = "moq-uring-test";
const CLOSE_CODE: u32 = 42;
const CLOSE_REASON: &str = "bye";

fn server_config(certs: &support::Certs) -> quic::server::Config {
	quic::server::Config::new(
		quic::Identity::open(&certs.cert, &certs.key)
			.expect("identity")
			.server(&[ALPN], quic::server::ClientAuth::None)
			.expect("tls"),
	)
}

fn dial_config() -> quic::client::Config {
	let mut config = quic::client::Config::default();
	config.alpn = vec![ALPN.to_string()];
	config.verify = false;
	config
}

/// One socket carries dials and accepts at once: two endpoints dial each
/// other, and each also accepts the other's dial. This is the relay cluster
/// shape (a worker's socket serves inbound sessions and upstream dials).
/// Every connection names its peer's socket, and an accepted one the SNI.
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
			sock,
			quic::endpoint::Config::default().with_server(server_config(&certs)),
		)
		.expect("endpoint")
	};
	let a = endpoint(&handle);
	let b = endpoint(&handle);

	let (a_addr, b_addr) = (a.local_addr(), b.local_addr());

	worker
		.block_on(async move {
			let a_to_b = a
				.connect(&dial_config(), b.local_addr(), "localhost")
				.await
				.expect("a dials b");
			let b_in = b.accept().await.expect("b accepts a");
			let b_to_a = b
				.connect(&dial_config(), a.local_addr(), "localhost")
				.await
				.expect("b dials a");
			let a_in = a.accept().await.expect("a accepts b");

			for conn in [&a_to_b, &b_in, &b_to_a, &a_in] {
				assert_eq!(conn.protocol(), Some(ALPN), "negotiated ALPN");
			}
			for (conn, peer) in [(&a_to_b, b_addr), (&b_in, a_addr), (&b_to_a, a_addr), (&a_in, b_addr)] {
				assert_eq!(conn.remote_addr(), peer, "peer address");
			}
			for conn in [&b_in, &a_in] {
				assert_eq!(conn.server_name(), Some("localhost"), "accepted SNI");
			}
			for conn in [&a_to_b, &b_to_a] {
				assert_eq!(conn.server_name(), None, "a dial has no SNI of its own");
			}
		})
		.expect("worker");
}

/// A session owns the connection independently of the driver and stream handles.
#[test]
fn dropping_the_last_raw_session_closes_the_connection() {
	dropped_raw_session_closes(false);
}

#[test]
fn an_explicit_raw_close_survives_the_last_session_drop() {
	dropped_raw_session_closes(true);
}

fn dropped_raw_session_closes(explicit: bool) {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");
	let socket = || {
		handle
			.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
			.expect("socket")
	};
	let server = quic::Endpoint::new(
		socket(),
		quic::endpoint::Config::default().with_server(server_config(&certs)),
	)
	.expect("server endpoint");
	let client = quic::Endpoint::new(socket(), quic::endpoint::Config::default()).expect("client endpoint");
	worker
		.block_on(async move {
			let conn = client
				.connect(&dial_config(), server.local_addr(), "localhost")
				.await
				.expect("dial");
			let peer = server.accept().await.expect("accept");
			let session = quic::web::Session::raw(conn);
			let mut remaining = session.clone();
			drop(session);
			let mut send = std::future::poll_fn(|cx| remaining.poll_open_uni(cx))
				.await
				.expect("surviving clone opens a stream");
			std::future::poll_fn(|cx| send.poll_write(cx, b"alive"))
				.await
				.expect("write");
			let mut recv = peer.accept_uni().await.expect("accept stream");
			let mut payload = [0; 5];
			let read = recv.read(&mut &mut payload[..]).await.expect("read");
			assert_eq!(read, Some(5));
			assert_eq!(&payload, b"alive");
			let code = if explicit {
				remaining.close(CLOSE_CODE, CLOSE_REASON);
				u64::from(CLOSE_CODE)
			} else {
				0
			};
			drop(remaining);
			// Stream handles intentionally survive the final session handle.
			let error = peer.closed().await;
			assert!(
				matches!(error, quic::Error::App { code: actual, .. } if actual == code),
				"peer saw {error:?}"
			);
			drop((send, recv));
		})
		.expect("worker");
}

/// A close reaches both ends carrying the code and reason it was given. The
/// closer reads its own close back out of `poll_closed`, which is what moq's
/// session machine waits on, and the peer reads the same one off the wire.
#[test]
fn a_close_reaches_both_ends_with_its_code() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let activity = moq_net::server::Activity::default();
	let outgoing = moq_net::server::Activity::default();
	let certs = support::certs().expect("certificates");

	let socket = |handle: &moq_uring::Handle| {
		handle
			.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
			.expect("socket")
	};
	let mut incoming_config = quic::endpoint::Config::default().with_server(server_config(&certs));
	incoming_config.activity = activity.clone();
	let server = quic::Endpoint::new(socket(&handle), incoming_config).expect("server endpoint");
	let mut outgoing_config = quic::endpoint::Config::default();
	outgoing_config.activity = outgoing.clone();
	let client = quic::Endpoint::new(socket(&handle), outgoing_config).expect("dial-only endpoint");

	worker
		.block_on(async move {
			let mut dialed = client
				.connect(&dial_config(), server.local_addr(), "localhost")
				.await
				.expect("dial");
			let closes = |activity: &moq_net::server::Activity| {
				let mut future = std::pin::pin!(activity.close());
				std::future::Future::poll(
					future.as_mut(),
					&mut std::task::Context::from_waker(std::task::Waker::noop()),
				)
				.is_ready()
			};
			assert!(!closes(&activity), "unclaimed incoming connection holds admission");
			assert!(closes(&outgoing), "outbound dials are not admissions");
			let mut accepted = server.accept().await.expect("accept");

			dialed.close(CLOSE_CODE.into(), CLOSE_REASON.as_bytes());

			for (end, conn) in [("closer", &mut dialed), ("peer", &mut accepted)] {
				match conn.closed().await {
					quic::Error::App { code, reason } => {
						assert_eq!(code, u64::from(CLOSE_CODE), "{end} close code");
						assert_eq!(reason, CLOSE_REASON, "{end} close reason");
					}
					other => panic!("{end} expected an application close, got {other:?}"),
				}
			}
			assert!(
				closes(&activity),
				"closed transport releases admission even while handles survive"
			);
			assert!(
				client
					.connect(&dial_config(), server.local_addr(), "localhost")
					.await
					.is_err(),
				"closed activity refuses a new connection at accept"
			);
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
		sock,
		quic::endpoint::Config::default().with_server(server_config(&certs)),
	)
	.expect("endpoint");
	let server = endpoint.local_addr();
	let dial_only_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("dial-only socket");
	let dial_only = quic::Endpoint::new(dial_only_sock, quic::endpoint::Config::default()).expect("dial-only endpoint");
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

			// Junk first: it must not be fatal, and whatever answer it draws
			// must be smaller than it was, so the socket is never an
			// amplifier. moq-quic answers a packet naming no connection with a
			// stateless reset, which is what tells a
			// peer holding stale state to give up.
			let junk = [0u8; 64];
			sock.send_to(&junk, server)?;
			sock.set_read_timeout(Some(Duration::from_millis(200)))?;
			let mut answer = [0u8; 1500];
			match sock.recv_from(&mut answer) {
				Ok((len, _)) => anyhow::ensure!(len < junk.len(), "a junk datagram drew {len} bytes back"),
				Err(err) if timed_out(&err) => {}
				Err(err) => return Err(err.into()),
			}
			sock.set_read_timeout(Some(Duration::from_secs(5)))?;

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
				Err(err) if timed_out(&err) => {}
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

	let response = worker
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

	assert_eq!(&response[1..5], &[0; 4], "version negotiation has version zero");
	let dcid = usize::from(response[5]);
	let scid_len = 6 + dcid;
	let scid = usize::from(response[scid_len]);
	let versions = response[scid_len + 1 + scid..]
		.as_chunks::<4>()
		.0
		.iter()
		.map(|version| u32::from_be_bytes(*version))
		.collect::<Vec<_>>();
	assert!(versions.contains(&1), "QUIC v1 is offered: {versions:?}");
	drop((endpoint, dial_only));
}

/// Await `future`, failing loudly rather than hanging if it never resolves.
async fn within<T>(handle: &moq_uring::Handle, what: &str, future: impl Future<Output = T>) -> T {
	let mut deadline = moq_uring::Timer::after(handle, Duration::from_secs(5));
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

/// A peer's stop reaches the send half of the stream it accepted.
///
/// The stop can be processed before or after the application takes the
/// stream, depending on how the driver's sweep interleaves with the accept,
/// and `poll_closed` has to report it either way rather than waiting for an
/// event that has already been and gone.
#[test]
fn a_peer_stop_reaches_the_accepted_stream() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");

	let socket = |handle: &moq_uring::Handle| {
		handle
			.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
			.expect("socket")
	};
	let server = quic::Endpoint::new(
		socket(&handle),
		quic::endpoint::Config::default().with_server(server_config(&certs)),
	)
	.expect("server endpoint");
	let client = quic::Endpoint::new(socket(&handle), quic::endpoint::Config::default()).expect("dial-only endpoint");

	worker
		.block_on(async move {
			let dialed = client
				.connect(&dial_config(), server.local_addr(), "localhost")
				.await
				.expect("dial");
			let accepted = server.accept().await.expect("accept");

			// Held, not dropped: dropping the send half would reset it, and
			// the peer would then see a reset rather than the stop.
			let (mut send, recv) = dialed.open_bi(Default::default()).await.expect("open_bi");
			// One byte makes the stream readable before the stop arrives. Nothing
			// is awaited between them, so both frames leave in the same flush.
			send.write_all(&b"x"[..]).await.expect("write");
			recv.stop(CLOSE_CODE.into());

			let (send, _recv) = within(&handle, "the stopped stream to arrive", accepted.accept_bi())
				.await
				.expect("accept_bi");

			match within(&handle, "the stop to be reported", send.stopped()).await {
				Ok(Some(code)) => assert_eq!(code, CLOSE_CODE.into(), "stop code"),
				other => panic!("expected the peer's stop, got {other:?}"),
			}
		})
		.expect("worker");
}

/// A read timeout, which the platform spells one of two ways.
fn timed_out(err: &std::io::Error) -> bool {
	matches!(
		err.kind(),
		std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
	)
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
	// The dial ends at moq-quic's handshake idle timeout (10s), not the idle timeout.
	let hole_addr = hole.local_addr().expect("addr");
	let dial = dial_config();

	let result = worker
		.block_on(async move { quic::client::connect(client_sock, &dial, hole_addr, "localhost").await })
		.expect("worker");
	assert!(result.is_err(), "a dial into a black hole must time out");
}

/// A dial to an address the host cannot route fails at once with
/// `NO_VIABLE_PATH` rather than at the handshake timeout, and fails alone: the
/// socket it shares goes on dialing. Linux refuses a v4-mapped destination on
/// an IPv6-only socket with `ENETUNREACH`, an unroutable family without
/// touching the host's routes.
#[test]
fn an_unroutable_dial_fails_alone() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");

	let server = quic::Endpoint::new(
		handle
			.udp(UdpSocket::bind("[::1]:0").expect("bind"), udp::Config::default())
			.expect("server socket"),
		quic::endpoint::Config::default().with_server(server_config(&certs)),
	)
	.expect("server endpoint");
	let v6_only = {
		use nix::sys::socket::{AddressFamily, SockFlag, SockType, SockaddrIn6, bind, setsockopt, socket, sockopt};
		use std::os::fd::AsRawFd;
		let fd = socket(AddressFamily::Inet6, SockType::Datagram, SockFlag::empty(), None).expect("socket");
		setsockopt(&fd, sockopt::Ipv6V6Only, &true).expect("IPV6_V6ONLY");
		let local: std::net::SocketAddrV6 = "[::1]:0".parse().expect("addr");
		bind(fd.as_raw_fd(), &SockaddrIn6::from(local)).expect("bind");
		UdpSocket::from(fd)
	};
	let client = quic::Endpoint::new(
		handle.udp(v6_only, udp::Config::default()).expect("client socket"),
		quic::endpoint::Config::default(),
	)
	.expect("dial-only endpoint");

	let port = server.local_addr().port();
	let unroutable = std::net::SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.to_ipv6_mapped().into(), port);
	worker
		.block_on(async move {
			// Well inside the 10s handshake timeout a swallowed error waits for.
			match within(
				&handle,
				"the unroutable dial to fail",
				client.connect(&dial_config(), unroutable, "localhost"),
			)
			.await
			{
				Err(quic::Error::Transport { code: 0x10, .. }) => {}
				other => panic!("expected NO_VIABLE_PATH, got {other:?}"),
			}

			let dialed = within(
				&handle,
				"a dial on the same socket",
				client.connect(&dial_config(), server.local_addr(), "localhost"),
			)
			.await;
			dialed.expect("the socket outlives one unroutable destination");
		})
		.expect("worker");
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
	let endpoint = quic::Endpoint::new(sock, config).expect("endpoint");
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
	// The server never answers, so the dial ends at moq-quic's handshake idle
	// timeout (10s).
	let dial = dial_config();

	let result = worker
		.block_on(async move { quic::client::connect(client_sock, &dial, server, "localhost").await })
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
	let endpoint = quic::Endpoint::new(sock, config).expect("endpoint");
	let server = endpoint.local_addr();

	let first_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("first client socket");
	let first = worker
		.block_on(async { quic::client::connect(first_sock, &dial_config(), server, "localhost").await })
		.expect("worker")
		.expect("first connection");

	let second_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("second client socket");
	let second_config = dial_config();
	let second = worker
		.block_on(async { quic::client::connect(second_sock, &second_config, server, "localhost").await })
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
	udp_config.tx_buffers_max = 1;
	let sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp_config)
		.expect("socket");
	let endpoint = quic::Endpoint::new(
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
			let first_client = quic::client::connect(first_sock, &dial_config(), server, "localhost")
				.await
				.expect("first connection");
			let first_server = endpoint.accept().await.expect("first accepted connection");

			let mut send = first_server
				.open_uni(Default::default())
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
						match send.write(&mut &chunk[offset..]).await {
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
				let first_client = first_client;
				let mut recv = first_client.accept_uni().await.expect("accept busy stream");
				let mut chunk = vec![0; 64 * 1024];
				while recv_running.get() {
					if recv
						.read(&mut &mut chunk[..])
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
			let mut second_config = dial_config();
			second_config.transport.idle_timeout = Duration::from_millis(500);
			let started = kio::clock::now();
			let second = quic::client::connect(second_sock, &second_config, server, "localhost").await;
			let elapsed = kio::clock::now() - started;
			running.set(false);
			let second = second.expect("the second connection must not starve");
			assert!(
				elapsed < Duration::from_millis(500),
				"second handshake took {elapsed:?}"
			);
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
	let endpoint = quic::Endpoint::new(sock, quic::endpoint::Config::default()).expect("endpoint");

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
		quic::Endpoint::new(sock, quic::endpoint::Config::default().with_server(config.clone())).expect("endpoint")
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
			handle.spawn(async move {
				quic::client::connect(sock, &dial_config(), server, "localhost")
					.await
					.expect("dial");
			});
			second.accept().await.expect("accepted connection");
		})
		.expect("worker");
}

/// Cancelling a dial after it created a connection closes that connection,
/// even though its driver and the endpoint remain alive for sibling dials.
#[test]
fn cancelling_a_dial_releases_its_connection() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");
	let socket = |handle: &moq_uring::Handle| {
		handle
			.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
			.expect("socket")
	};
	let server = quic::Endpoint::new(
		socket(&handle),
		quic::endpoint::Config::default().with_server(server_config(&certs)),
	)
	.expect("server endpoint");
	let client = quic::Endpoint::new(socket(&handle), quic::endpoint::Config::default()).expect("client endpoint");

	worker
		.block_on(async {
			let mut config = dial_config();
			config.transport.idle_timeout = Duration::from_secs(30);
			let mut dial = Box::pin(client.connect(&config, server.local_addr(), "localhost"));
			std::future::poll_fn(|cx| {
				assert!(
					dial.as_mut().poll(cx).is_pending(),
					"the dial must suspend during establishment"
				);
				Poll::Ready(())
			})
			.await;
			drop(dial);

			within(&handle, "cancelled dial bookkeeping to drain", async {
				loop {
					if format!("{client:?}").contains("conns: 0") {
						break;
					}
					let mut tick = moq_uring::Timer::after(&handle, Duration::from_millis(10));
					kio::wait(|waiter| tick.poll(waiter)).await;
				}
			})
			.await;

			let dialed = within(
				&handle,
				"sibling dial",
				client.connect(&dial_config(), server.local_addr(), "localhost"),
			)
			.await
			.expect("sibling dial");
			let accepted = within(&handle, "sibling accept", server.accept())
				.await
				.expect("sibling accept");
			assert_eq!(dialed.protocol(), Some(ALPN));
			assert_eq!(accepted.protocol(), Some(ALPN));
		})
		.expect("worker");
}
