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
	backoff.initial = Some((Duration::from_millis(20)).into());
	backoff.max = Some((Duration::from_millis(40)).into());
	backoff.timeout = Some((Duration::from_millis(200)).into());

	// Nothing listens on port 1, so every attempt is refused: transient as far as this layer knows.
	let url: url::Url = "tcp://127.0.0.1:1".parse().expect("failed to parse url");
	let started = moq_clock::now();
	let reconnect = client(backoff).connect(url);

	let err = moq_clock::timeout(Duration::from_secs(10), reconnect.closed())
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

/// The retry window decides whether a new attempt starts, not how long one may take: a handshake
/// slower than the window, but inside the connect timeout, lands on the first dial. Cutting it at
/// the window would only fail it, and every redial the same way.
#[tokio::test]
async fn a_handshake_slower_than_the_retry_window_still_lands() {
	const WINDOW: Duration = Duration::from_millis(300);
	const HANDSHAKE: Duration = Duration::from_secs(1);

	// `tls://`, so the dial waits on the server's half of the TLS handshake.
	let mut listen = moq_tokio::listen::Config::default();
	listen.tcp.bind = Some("127.0.0.1:0".parse().expect("parse addr"));
	listen.tcp.tls = Some(true);
	listen.tls.generate = vec!["localhost".into()];
	let mut server = listen
		.init(Default::default())
		.expect("init server")
		.listen()
		.await
		.expect("bind tcp listener");
	let server_addr = server.tcp_local_addr().expect("tcp listener bound");
	let (accepted, mut sessions) = tokio::sync::mpsc::unbounded_channel();
	let _server = tokio::spawn(async move {
		let origin = moq_tokio::origin::spawn();
		while let Some(request) = server.accept().await {
			if let Ok(session) = request.with_publisher(&origin).ok().await {
				let _ = accepted.send(session);
			}
		}
	});

	// Holds each connection for the handshake's length before forwarding it to the server.
	let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
	let port = proxy.local_addr().expect("proxy bound").port();
	let (dialed, mut dials) = tokio::sync::mpsc::unbounded_channel();
	let _proxy = tokio::spawn(async move {
		while let Ok((mut client, _)) = proxy.accept().await {
			let _ = dialed.send(());
			tokio::spawn(async move {
				moq_clock::sleep(HANDSHAKE).await;
				let mut upstream = tokio::net::TcpStream::connect(server_addr)
					.await
					.expect("connect to the server");
				let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
			});
		}
	});

	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	config.backoff.timeout = Some((WINDOW).into());
	let client = config.init(Default::default()).expect("failed to init client");
	let url: url::Url = format!("tls://127.0.0.1:{port}/").parse().expect("parse url");
	let connection = client.connect(url);

	tokio::select! {
		session = sessions.recv() => {
			session.expect("server stopped accepting");
		}
		res = connection.closed() => panic!("gave up mid-handshake: {res:?}"),
		_ = moq_clock::sleep(Duration::from_secs(30)) => panic!("never connected"),
	}
	dials.recv().await.expect("proxy stopped");
	assert!(dials.try_recv().is_err(), "the first dial was cut short and redialed");
}

/// The connect count advances in the same write that reports `Connected`, so a caller
/// pairing [`Connection::status`] with [`Connection::connects`] never sees a stale
/// count, however fast the redial.
#[tokio::test]
async fn the_connect_count_advances_on_reconnect() {
	let (port, mut sessions, _task) = spawn_server().await;
	let url: url::Url = format!("tcp://localhost:{port}/").parse().expect("parse url");
	let mut connection = quick_client(Default::default()).connect(url);

	let status = moq_clock::timeout(Duration::from_secs(10), connection.status())
		.await
		.expect("status timed out")
		.expect("status failed");
	assert_eq!(status, moq_tokio::Status::Connected);
	assert_eq!(connection.connects(), 1, "the first connect counts 1");

	// Dropping the server's handle closes the session, so the loop redials.
	let session = sessions.recv().await.expect("server stopped accepting");
	drop(session);

	moq_clock::timeout(Duration::from_secs(10), async {
		while connection.connects() < 2 {
			moq_clock::sleep(Duration::from_millis(10)).await;
		}
	})
	.await
	.expect("the connect count never advanced past the first connect");
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

/// A client that redials fast, so a migration lands inside the test's patience.
fn quick_client(redirect: moq_tokio::Redirect) -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.backoff.initial = Some((Duration::from_millis(20)).into());
	config.backoff.max = Some((Duration::from_millis(40)).into());
	config.backoff.timeout = Some((Duration::ZERO).into());
	config.goaway.redirect = redirect;
	config.init(Default::default()).expect("failed to init client")
}

/// A protocol violation is a settled failure even after a successful session.
#[tokio::test]
async fn a_protocol_violation_ends_reconnect() {
	let (port, mut sessions, task) = spawn_server().await;
	let connection =
		quick_client(Default::default()).connect(format!("tcp://127.0.0.1:{port}/").parse::<url::Url>().unwrap());
	let first = sessions.recv().await.unwrap();
	first.abort(moq_net::Error::ProtocolViolation);
	let result = tokio::select! {
		result = connection.closed() => result,
		_ = sessions.recv() => panic!("redialed after a protocol violation"),
		_ = moq_clock::sleep(Duration::from_secs(10)) => panic!("protocol failure did not end reconnect"),
	};
	task.abort();
	let _ = task.await;
	assert!(
		matches!(
			result,
			Err(moq_tokio::Error::MoqNet(moq_net::Error::Session(
				moq_net::SessionError::ProtocolViolation
			)))
		),
		"{result:?}"
	);
}

/// The default refuses a peer-selected host change, and the refusal ends the
/// connection: the peer is leaving, so redialing the configured URL would ignore it.
#[tokio::test]
async fn a_redirect_to_another_host_is_refused_by_default() {
	let (port_a, mut sessions_a, _task_a) = spawn_server().await;
	let (port_b, mut sessions_b, _task_b) = spawn_server().await;

	let url: url::Url = format!("tcp://localhost:{port_a}/").parse().expect("parse url");
	let connection = quick_client(Default::default()).connect(url);

	let first = moq_clock::timeout(Duration::from_secs(10), sessions_a.recv())
		.await
		.expect("first dial timed out")
		.expect("server A stopped accepting");

	first
		.drain()
		.send(moq_net::goaway::Goaway::redirect(format!("tcp://127.0.0.1:{port_b}/")))
		.expect("send goaway");

	let err = moq_clock::timeout(Duration::from_secs(10), connection.closed())
		.await
		.expect("the refusal never ended the connection")
		.expect_err("a refused redirect must end with an error");
	assert!(matches!(err, moq_tokio::Error::RefusedRedirect(_)), "ended with {err}");

	assert!(sessions_a.try_recv().is_err(), "redialed the configured URL");
	assert!(
		sessions_b.try_recv().is_err(),
		"the peer moved us onto the host it named"
	);
}

/// `--connect-goaway-redirect follow` is the opt-in that hands the peer the host, so the
/// same redirect is followed. Without this the default above would be
/// indistinguishable from ignoring the URI outright.
#[tokio::test]
async fn follow_still_honors_a_cross_host_redirect() {
	let (port_a, mut sessions_a, _task_a) = spawn_server().await;
	let (port_b, mut sessions_b, _task_b) = spawn_server().await;

	let url: url::Url = format!("tcp://localhost:{port_a}/").parse().expect("parse url");
	let _connection = quick_client(moq_tokio::Redirect::Follow).connect(url);

	let first = moq_clock::timeout(Duration::from_secs(10), sessions_a.recv())
		.await
		.expect("first dial timed out")
		.expect("server A stopped accepting");

	first
		.drain()
		.send(moq_net::goaway::Goaway::redirect(format!("tcp://127.0.0.1:{port_b}/")))
		.expect("send goaway");

	moq_clock::timeout(Duration::from_secs(10), sessions_b.recv())
		.await
		.expect("never followed the redirect")
		.expect("server B stopped accepting");
}

/// A refused redirect must not fall through to a caller-selected fallback either.
#[tokio::test]
async fn a_refused_redirect_skips_configured_fallbacks() {
	let (port_a, mut sessions_a, task_a) = spawn_server().await;
	let (port_b, mut sessions_b, _task_b) = spawn_server().await;
	let primary: url::Url = format!("tcp://localhost:{port_a}/").parse().expect("primary URL");
	let fallback: url::Url = format!("tcp://127.0.0.1:{port_b}/").parse().expect("fallback URL");
	let addrs = moq_tokio::connect::Addrs::new(primary).or(fallback);
	let connection = quick_client(Default::default()).connect(addrs);
	let first = moq_clock::timeout(Duration::from_secs(10), sessions_a.recv())
		.await
		.expect("first dial timed out")
		.expect("server A stopped accepting");

	// Stop accepting before GOAWAY so any redial could only land on the fallback.
	task_a.abort();
	assert!(task_a.await.expect_err("listener was aborted").is_cancelled());
	first
		.drain()
		.send(moq_net::goaway::Goaway::redirect("tcp://127.0.0.1:1/"))
		.expect("send goaway");

	let err = moq_clock::timeout(Duration::from_secs(10), connection.closed())
		.await
		.expect("the refusal never ended the connection")
		.expect_err("a refused redirect must end with an error");
	assert!(matches!(err, moq_tokio::Error::RefusedRedirect(_)), "ended with {err}");
	assert!(
		sessions_b.try_recv().is_err(),
		"fell through to the configured fallback"
	);
}

/// An empty GOAWAY is "reconnect to me", which keeps every caller-selected fallback.
#[tokio::test]
async fn an_empty_goaway_preserves_configured_fallbacks() {
	let (port_a, mut sessions_a, task_a) = spawn_server().await;
	let (port_b, mut sessions_b, _task_b) = spawn_server().await;
	let primary: url::Url = format!("tcp://localhost:{port_a}/").parse().expect("primary URL");
	let fallback: url::Url = format!("tcp://127.0.0.1:{port_b}/").parse().expect("fallback URL");
	let addrs = moq_tokio::connect::Addrs::new(primary).or(fallback);
	let _connection = quick_client(Default::default()).connect(addrs);
	let first = moq_clock::timeout(Duration::from_secs(10), sessions_a.recv())
		.await
		.expect("first dial timed out")
		.expect("server A stopped accepting");

	// Stop accepting before GOAWAY so the migration must use the configured fallback.
	task_a.abort();
	assert!(task_a.await.expect_err("listener was aborted").is_cancelled());
	first.drain().send(moq_net::goaway::Goaway::new()).expect("send goaway");

	moq_clock::timeout(Duration::from_secs(10), sessions_b.recv())
		.await
		.expect("configured fallback was discarded")
		.expect("server B stopped accepting");
}

/// A graceful restart: the server keeps accepting while it drains, and sends every new session
/// an empty-URI GOAWAY at once, for longer than the retry window. Each of those sessions serves
/// until the client moves on, so none is a failure, and the client is still there to land on
/// the first session the server lets stay.
#[tokio::test]
async fn a_draining_peer_does_not_exhaust_the_retry_window() {
	// Each redial waits at least half of its 200ms delay, so the drained sessions alone span
	// more than twice the window. A GOAWAY arriving inside the 200ms health bar is what makes a
	// session count as drained at once rather than as a healthy session that later migrated.
	let mut backoff = moq_tokio::Backoff::default();
	backoff.initial = Some((Duration::from_millis(200)).into());
	backoff.max = Some((Duration::from_millis(200)).into());
	backoff.timeout = Some((Duration::from_millis(500)).into());
	rides_out_a_drain(backoff, 12).await;
}

/// A restart's pacing sleep can be longer than the retry window. It must not spend the window,
/// or the loop would give up right after the sleep without redialing the restarting peer.
#[tokio::test]
async fn a_drain_backoff_longer_than_the_retry_window_still_redials() {
	// From the second drained session on, the delay is 400ms against a 100ms window.
	let mut backoff = moq_tokio::Backoff::default();
	backoff.initial = Some((Duration::from_millis(200)).into());
	backoff.max = Some((Duration::from_millis(400)).into());
	backoff.timeout = Some((Duration::from_millis(100)).into());
	rides_out_a_drain(backoff, 4).await;
}

/// Connect with `backoff` to a server that sends its first `drained` sessions an empty-URI
/// GOAWAY at once, and assert the client is still there to land on the session after them.
async fn rides_out_a_drain(backoff: moq_tokio::Backoff, drained: usize) {
	let client = client(backoff);

	let mut server = moq_tokio::listen::Config::default();
	server.tcp.bind = Some("127.0.0.1:0".parse().expect("parse addr"));
	let mut server = server
		.init(Default::default())
		.expect("init server")
		.listen()
		.await
		.expect("bind tcp listener");
	let port = server.tcp_local_addr().expect("tcp listener bound").port();

	let (served_tx, mut served) = tokio::sync::mpsc::unbounded_channel();
	let _task = tokio::spawn(async move {
		let origin = moq_tokio::origin::spawn();
		// Held, as a draining server does: each one closes when the client leaves it.
		let mut held = Vec::new();
		while let Some(request) = server.accept().await {
			let Ok(session) = request.with_publisher(&origin).ok().await else {
				continue;
			};
			if held.len() < drained {
				session
					.drain()
					.send(moq_net::goaway::Goaway::new().with_timeout(Duration::from_secs(30)))
					.expect("send goaway");
				held.push(session);
			} else {
				let _ = served_tx.send(session);
			}
		}
	});

	let url: url::Url = format!("tcp://127.0.0.1:{port}/").parse().expect("parse url");
	let connection = client.connect(url);

	tokio::select! {
		session = served.recv() => {
			session.expect("server stopped accepting");
		}
		res = connection.closed() => panic!("gave up during the drain: {res:?}"),
		_ = moq_clock::sleep(Duration::from_secs(30)) => panic!("never landed after the drain"),
	}
}

/// How long each session on a bouncing fleet lives before its relay sends the GOAWAY. Twice
/// the 50ms healthy bar these tests use, so every session counts as healthy.
const LIFE: Duration = Duration::from_millis(100);

/// Send every session `sessions` yields the GOAWAY `uri(n)` once it has lived [`LIFE`], `n`
/// counting its sessions, and report each accept's time on `log`. Each session is held until
/// the client leaves it, as a draining relay does.
fn bounce(
	mut sessions: tokio::sync::mpsc::UnboundedReceiver<moq_net::Session>,
	log: tokio::sync::mpsc::UnboundedSender<moq_clock::Instant>,
	uri: impl Fn(usize) -> String + Send + 'static,
) -> tokio::task::JoinHandle<()> {
	tokio::spawn(async move {
		for n in 0.. {
			let Some(session) = sessions.recv().await else { return };
			let _ = log.send(moq_clock::now());
			let uri = uri(n);
			tokio::spawn(async move {
				moq_clock::sleep(LIFE).await;
				let _ = session.drain().send(moq_net::goaway::Goaway::redirect(uri));
				session.closed().await;
			});
		}
	})
}

/// Two relays redirecting every session to each other once it has lived [`LIFE`], minting a
/// fresh credential in the query each time when `rotate` is set. Returns the first relay's
/// URL, a receiver of every accept's time across both, and their tasks.
async fn ping_pong(
	rotate: bool,
) -> (
	url::Url,
	tokio::sync::mpsc::UnboundedReceiver<moq_clock::Instant>,
	Vec<tokio::task::JoinHandle<()>>,
) {
	let (port_a, sessions_a, task_a) = spawn_server().await;
	let (port_b, sessions_b, task_b) = spawn_server().await;
	let (log, accepts) = tokio::sync::mpsc::unbounded_channel();
	let to = move |port: u16| {
		move |n: usize| match rotate {
			true => format!("tcp://127.0.0.1:{port}/?jwt={n}"),
			false => format!("tcp://127.0.0.1:{port}/"),
		}
	};
	let tasks = vec![
		task_a,
		task_b,
		bounce(sessions_a, log.clone(), to(port_b)),
		bounce(sessions_b, log, to(port_a)),
	];
	let url = format!("tcp://127.0.0.1:{port_a}/").parse().expect("parse url");
	(url, accepts, tasks)
}

/// A backoff starting at 50ms that doubles up to `max`, giving up after `timeout`, and that
/// counts a session as healthy once it outlives the initial delay.
fn bounced_backoff(max: Duration, timeout: Duration) -> moq_tokio::Backoff {
	let mut backoff = moq_tokio::Backoff::default();
	backoff.initial = Some((Duration::from_millis(50)).into());
	backoff.healthy = Some((Duration::from_millis(50)).into());
	backoff.multiplier = 2;
	backoff.max = Some((max).into());
	backoff.timeout = Some((timeout).into());
	backoff
}

/// How long the client took to redial after each of `hops` GOAWAYs: the time between two
/// accepts, less the [`LIFE`] the first one served before its GOAWAY.
async fn redial_waits(
	accepts: &mut tokio::sync::mpsc::UnboundedReceiver<moq_clock::Instant>,
	hops: usize,
) -> Vec<Duration> {
	let mut last = accepts.recv().await.expect("never connected");
	let mut waits = Vec::new();
	for hop in 0..hops {
		let next = moq_clock::timeout(Duration::from_secs(10), accepts.recv())
			.await
			.unwrap_or_else(|_| panic!("hop {hop}: never redialed"))
			.expect("the relays stopped");
		waits.push(next.saturating_duration_since(last).saturating_sub(LIFE));
		last = next;
	}
	waits
}

/// Two relays that each outlive the initial backoff can still bounce the client between them
/// forever. The first move is a healthy migration; every return after it is a loop that waits
/// out the jittered backoff, at least half of a delay escalating to the cap. The redirects are
/// still honoured: a loop is something to slow down, not a failure.
#[tokio::test]
async fn two_peers_redirecting_to_each_other_escalate_the_backoff() {
	for rotate in [false, true] {
		let (url, mut accepts, _tasks) = ping_pong(rotate).await;
		let backoff = bounced_backoff(Duration::from_millis(400), Duration::ZERO);
		let _connection = client(backoff).connect(url);

		let delays = [0, 50, 100, 200, 400, 400].map(Duration::from_millis);
		let waits = redial_waits(&mut accepts, delays.len()).await;
		for (hop, (wait, delay)) in waits.into_iter().zip(delays).enumerate() {
			assert!(
				wait >= delay / 2,
				"rotate {rotate}, hop {hop}: waited {wait:?} against a {delay:?} backoff"
			);
		}
	}
}

/// The retry window still ends a redirect loop. Without the loop guard it never would: every
/// hop is healthy and restarts the window.
#[tokio::test]
async fn a_redirect_loop_stops_at_the_retry_window() {
	let (url, mut accepts, _tasks) = ping_pong(false).await;
	let backoff = bounced_backoff(Duration::from_millis(400), Duration::from_millis(300));
	let connection = client(backoff).connect(url);
	let closed = connection.closed();
	tokio::pin!(closed);
	for hop in 0.. {
		tokio::select! {
			res = &mut closed => {
				let err = res.expect_err("the loop ended without an error");
				assert!(
					matches!(&err, moq_tokio::Error::Reconnect(msg) if msg.contains("redirect loop")),
					"{err}"
				);
				return;
			}
			accept = accepts.recv() => {
				accept.expect("the relays stopped");
				assert!(hop < 10, "the loop never gave up");
			}
		}
	}
}

/// Run `hops` migrations of a connection whose retry window is shorter than a session's
/// life. A healthy migration restarts the window, while any hop scored as a loop spends it
/// and ends the connection, so this fails on a backoff the hops should never take.
async fn migrates_without_backing_off(
	connection: moq_tokio::Connection,
	accepts: &mut tokio::sync::mpsc::UnboundedReceiver<moq_clock::Instant>,
	hops: usize,
) {
	tokio::select! {
		_ = redial_waits(accepts, hops) => {}
		res = connection.closed() => panic!("a migration backed off: {res:?}"),
	}
}

/// A loop no faster than the capped delay already redials as slowly as the backoff would, so
/// each hop starts a new run and migrates at once.
#[tokio::test]
async fn a_loop_no_faster_than_the_capped_delay_migrates_without_backing_off() {
	let (url, mut accepts, _tasks) = ping_pong(false).await;
	// The cap is the initial delay, well under each session's life.
	let backoff = bounced_backoff(Duration::from_millis(50), Duration::from_millis(60));
	migrates_without_backing_off(client(backoff).connect(url), &mut accepts, 6).await;
}

/// A one-way rolling restart never revisits a relay, and a GOAWAY back to the same place,
/// named or empty, is a restart rather than a move. None of them backs off.
#[tokio::test]
async fn a_one_way_rolling_restart_migrates_without_backing_off() {
	let (port_a, sessions_a, _task_a) = spawn_server().await;
	let (port_b, sessions_b, _task_b) = spawn_server().await;
	let (port_c, sessions_c, _task_c) = spawn_server().await;
	let (log, mut accepts) = tokio::sync::mpsc::unbounded_channel();
	let _a = bounce(sessions_a, log.clone(), move |_| format!("tcp://127.0.0.1:{port_b}/"));
	let _b = bounce(sessions_b, log.clone(), move |_| format!("tcp://127.0.0.1:{port_c}/"));
	// The last relay restarts in place, alternating a named URI and an empty one.
	let _c = bounce(sessions_c, log, move |n| match n % 2 {
		0 => format!("tcp://127.0.0.1:{port_c}/"),
		_ => String::new(),
	});

	let backoff = bounced_backoff(Duration::from_millis(400), Duration::from_millis(60));
	let url: url::Url = format!("tcp://127.0.0.1:{port_a}/").parse().expect("parse url");
	migrates_without_backing_off(client(backoff).connect(url), &mut accepts, 6).await;
}
