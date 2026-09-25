//! What ends the reconnect loop, and where a peer's GOAWAY can send it.
//!
//! Dials over plain TCP (`tcp://`), which fails fast and locally: no TLS material, no QUIC
//! handshake, no server. That keeps the assertion about the *budget* rather than about how long a
//! particular backend takes to give up.

#![cfg(feature = "tcp")]

use std::time::Duration;

use moq_tokio::moq_net;

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
	backoff.initial = Duration::from_millis(20);
	backoff.max = Duration::from_millis(40);
	backoff.timeout = Duration::from_millis(200);

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

/// The epoch advances in the same write that reports `Connected`, so a caller
/// pairing [`Connection::status`] with [`Connection::epoch`] never sees a stale
/// count, however fast the redial.
#[tokio::test]
async fn the_epoch_advances_on_reconnect() {
	let (port, mut sessions, _task) = spawn_server().await;
	let url: url::Url = format!("tcp://localhost:{port}/").parse().expect("parse url");
	let mut connection = quick_client(Default::default()).connect(url);

	let status = tokio::time::timeout(Duration::from_secs(10), connection.status())
		.await
		.expect("status timed out")
		.expect("status failed");
	assert_eq!(status, moq_tokio::Status::Connected);
	assert_eq!(connection.epoch(), 1, "the first connect is epoch 1");

	// Dropping the server's handle closes the session, so the loop redials.
	let session = sessions.recv().await.expect("server stopped accepting");
	drop(session);

	tokio::time::timeout(Duration::from_secs(10), async {
		while connection.epoch() < 2 {
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
	})
	.await
	.expect("the epoch never advanced past the first connect");
}

#[tokio::test]
async fn monitor_is_cloneable_without_keeping_the_connection_alive() {
	tokio::time::pause();
	let connection = quick_client(Default::default()).connect("tcp://127.0.0.1:1".parse::<url::Url>().unwrap());
	let monitor: moq_tokio::connection::Monitor = connection.monitor();
	let mut cloned = monitor.clone();
	let snapshot: Option<moq_tokio::connection::Snapshot> = cloned.snapshot();
	assert!(snapshot.is_none());
	assert!(monitor.stats().is_none());
	assert_eq!(monitor.presence(), moq_net::stats::Presence::default());

	drop(connection);
	assert!(matches!(
		cloned.presence_changed().await,
		Err(moq_tokio::Error::Stopped)
	));
	assert!(monitor.snapshot().is_none());
}

/// A stream-only moq server on an ephemeral loopback TCP port.
///
/// Returns the port, a receiver yielding every accepted session (so a test can
/// drain one), and the listener task.
async fn spawn_server() -> (
	u16,
	tokio::sync::mpsc::UnboundedReceiver<moq_net::Session>,
	tokio::task::JoinHandle<()>,
) {
	let mut config = moq_tokio::listen::Config::default();
	config.tcp.bind = Some("127.0.0.1:0".parse().expect("parse addr"));
	let server = config.init(Default::default()).expect("init server");
	let mut server = server.listen().await.expect("bind tcp listener");
	let port = server.tcp_local_addr().expect("tcp listener bound").port();

	let (accepted, sessions) = tokio::sync::mpsc::unbounded_channel();
	let handle = tokio::spawn(async move {
		while let Some(request) = server.accept().await {
			let origin = moq_tokio::origin::spawn();
			match request.with_publisher(&origin).ok().await {
				Ok(session) => {
					let _ = accepted.send(session);
				}
				Err(err) => tracing::warn!(%err, "accept failed"),
			}
		}
	});

	(port, sessions, handle)
}

/// A client that redials fast, so a refused redirect lands back on the original
/// server inside the test's patience.
fn quick_client(redirect: moq_tokio::Redirect) -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.backoff.initial = Duration::from_millis(20);
	config.backoff.max = Duration::from_millis(40);
	config.backoff.timeout = Duration::ZERO;
	config.goaway.redirect = redirect;
	config.init(Default::default()).expect("failed to init client")
}

/// The default refuses peer-selected host changes and redials the configured URL.
#[tokio::test]
async fn a_redirect_to_another_host_is_refused_by_default() {
	let (port_a, mut sessions_a, _task_a) = spawn_server().await;
	let (port_b, mut sessions_b, _task_b) = spawn_server().await;

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
	let (port_a, mut sessions_a, _task_a) = spawn_server().await;
	let (port_b, mut sessions_b, _task_b) = spawn_server().await;

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

/// Refusing a peer-selected host must not discard caller-selected fallbacks.
#[tokio::test]
async fn a_refused_redirect_preserves_configured_fallbacks() {
	let (port_a, mut sessions_a, task_a) = spawn_server().await;
	let (port_b, mut sessions_b, _task_b) = spawn_server().await;
	let primary: url::Url = format!("tcp://localhost:{port_a}/").parse().expect("primary URL");
	let fallback: url::Url = format!("tcp://127.0.0.1:{port_b}/").parse().expect("fallback URL");
	let addrs = moq_tokio::connect::Addrs::new(primary).or(fallback);
	let _connection = quick_client(Default::default()).connect(addrs);
	let first = tokio::time::timeout(Duration::from_secs(10), sessions_a.recv())
		.await
		.expect("first dial timed out")
		.expect("server A stopped accepting");

	// Stop accepting before GOAWAY so reconnect must use the configured fallback.
	task_a.abort();
	assert!(task_a.await.expect_err("listener was aborted").is_cancelled());
	first
		.drain()
		.send(moq_net::goaway::Goaway::redirect("tcp://127.0.0.1:1/"))
		.expect("send goaway");

	tokio::time::timeout(Duration::from_secs(10), sessions_b.recv())
		.await
		.expect("configured fallback was discarded")
		.expect("server B stopped accepting");
}
