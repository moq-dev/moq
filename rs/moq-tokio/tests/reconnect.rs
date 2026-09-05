//! What ends the reconnect loop, and where a peer's GOAWAY can send it.
//!
//! Dials over plain TCP (`tcp://`), which fails fast and locally: no TLS material, no QUIC
//! handshake, no server. That keeps the assertion about the *budget* rather than about how long a
//! particular backend takes to give up.

#![cfg(feature = "tcp")]

use std::net::TcpListener;
use std::time::Duration;

use moq_tokio::moq_net::{self, Hop};

/// A client whose reconnect loop escalates fast enough to assert on inside a test.
fn client(backoff: moq_tokio::Backoff) -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.backoff = backoff;
	config.init(Default::default()).expect("failed to init client")
}

/// A transient failure is retried, escalating, until the budget runs out. The give-up error names
/// the underlying cause so an operator sees why rather than just "timed out".
#[tokio::test]
async fn a_transient_failure_retries_until_the_budget_runs_out() {
	let mut backoff = moq_tokio::Backoff::default();
	backoff.initial = Duration::from_millis(20).into();
	backoff.max = Duration::from_millis(40).into();
	backoff.timeout = Duration::from_millis(200).into();

	// Nothing listens on port 1, so every attempt is refused: transient as far as this layer knows.
	let url: url::Url = "tcp://127.0.0.1:1".parse().expect("failed to parse url");
	let started = tokio::time::Instant::now();
	let reconnect = client(backoff).connect(url);

	let err = tokio::time::timeout(Duration::from_secs(10), reconnect.closed())
		.await
		.expect("reconnect loop never gave up")
		.expect_err("reconnect loop stopped without an error");

	assert!(
		matches!(err, moq_tokio::Error::Reconnect(_)),
		"stopped with {err} rather than exhausting the budget"
	);
	assert_ne!(
		err.to_string(),
		"reconnect timed out after 200ms",
		"give-up error lost the underlying cause"
	);
	// The budget is spent on sleeping between attempts, so reaching it takes at least most of it.
	// Jitter draws each delay from the top half of its window, hence half rather than the whole.
	assert!(
		started.elapsed() >= Duration::from_millis(100),
		"gave up after {:?} without retrying",
		started.elapsed()
	);
}

/// A stream-only moq server on a free loopback TCP port.
///
/// Returns the port, a receiver yielding every accepted session (so a test can
/// drain one), and the listener task. The free-port probe can lose a race with
/// another test between the probe closing and the real bind, so retry rather
/// than panicking in `init`.
fn spawn_server() -> (
	u16,
	tokio::sync::mpsc::UnboundedReceiver<moq_net::Session>,
	tokio::task::JoinHandle<()>,
) {
	for _ in 0..20 {
		let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
		let port = probe.local_addr().expect("local addr").port();
		drop(probe);

		let mut config = moq_tokio::listen::Config::default();
		config.tcp.bind = Some(format!("127.0.0.1:{port}").parse().expect("parse addr"));
		let Ok(server) = config.init(Default::default()) else {
			continue;
		};

		let (accepted, sessions) = tokio::sync::mpsc::unbounded_channel();
		let handle = tokio::spawn(async move {
			let mut server = server.listen().await.expect("listen");
			while let Some(request) = server.accept().await {
				let origin = moq_tokio::origin::spawn(Hop::random());
				match request.with_publisher(&origin).ok().await {
					Ok(session) => {
						let _ = accepted.send(session);
					}
					Err(err) => tracing::warn!(%err, "accept failed"),
				}
			}
		});

		return (port, sessions, handle);
	}
	panic!("could not bind a free TCP port after 20 attempts");
}

/// Wait for a listener to come up, so the first dial isn't racing the bind.
async fn wait_listening(port: u16) {
	let deadline = std::time::Instant::now() + Duration::from_secs(5);
	while tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err() {
		assert!(std::time::Instant::now() < deadline, "port {port} never came up");
		tokio::time::sleep(Duration::from_millis(25)).await;
	}
}

/// A client that redials fast, so a refused redirect lands back on the original
/// server inside the test's patience.
fn quick_client(redirect: moq_tokio::Redirect) -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.backoff.initial = Duration::from_millis(20).into();
	config.backoff.max = Duration::from_millis(40).into();
	config.backoff.timeout = Duration::ZERO.into();
	config.goaway.redirect = redirect;
	config.init(Default::default()).expect("failed to init client")
}

/// A GOAWAY naming another host is refused by the default policy, and the loop
/// redials the URL it was configured with.
///
/// Both servers are on loopback, so the host the peer names here is one we can
/// obviously reach. That is the point: the guard this replaced judged the URL,
/// and a peer that named a host rather than an address walked past it, since the
/// name is never resolved (and resolving it here would settle nothing anyway,
/// because the dial resolves it again). The default no longer lets the peer
/// choose a host at all.
#[tokio::test]
async fn a_redirect_to_another_host_is_refused_by_default() {
	let (port_a, mut sessions_a, _task_a) = spawn_server();
	let (port_b, mut sessions_b, _task_b) = spawn_server();
	wait_listening(port_a).await;
	wait_listening(port_b).await;

	let url: url::Url = format!("tcp://localhost:{port_a}/").parse().expect("parse url");
	let _connection = quick_client(Default::default()).connect(url);

	let first = tokio::time::timeout(Duration::from_secs(10), sessions_a.recv())
		.await
		.expect("first dial timed out")
		.expect("server A stopped accepting");

	first
		.drain()
		.send(moq_net::goaway::Goaway::redirect(format!("tcp://127.0.0.1:{port_b}/")))
		.expect("send goaway");

	// Refused, so the redial goes back to A rather than to the host the peer named.
	tokio::time::timeout(Duration::from_secs(10), sessions_a.recv())
		.await
		.expect("never redialed the configured URL")
		.expect("server A stopped accepting");

	assert!(
		sessions_b.try_recv().is_err(),
		"the peer moved us onto the host it named"
	);
}

/// `--goaway-redirect follow` is the opt-in that hands the peer the host, so the
/// same redirect is followed. Without this the default above would be
/// indistinguishable from ignoring the URI outright.
#[tokio::test]
async fn follow_still_honors_a_cross_host_redirect() {
	let (port_a, mut sessions_a, _task_a) = spawn_server();
	let (port_b, mut sessions_b, _task_b) = spawn_server();
	wait_listening(port_a).await;
	wait_listening(port_b).await;

	let url: url::Url = format!("tcp://localhost:{port_a}/").parse().expect("parse url");
	let _connection = quick_client(moq_tokio::Redirect::Follow).connect(url);

	let first = tokio::time::timeout(Duration::from_secs(10), sessions_a.recv())
		.await
		.expect("first dial timed out")
		.expect("server A stopped accepting");

	first
		.drain()
		.send(moq_net::goaway::Goaway::redirect(format!("tcp://127.0.0.1:{port_b}/")))
		.expect("send goaway");

	tokio::time::timeout(Duration::from_secs(10), sessions_b.recv())
		.await
		.expect("never followed the redirect")
		.expect("server B stopped accepting");
}
