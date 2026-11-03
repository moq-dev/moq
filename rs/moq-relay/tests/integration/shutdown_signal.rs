//! SIGINT has to go through the relay's graceful drain.
//!
//! `--drain-timeout` promises that the first shutdown signal sends every session
//! a GOAWAY and keeps serving for that long, so clients reconnect elsewhere
//! instead of being cut off. This raises a real SIGINT at a relay with a live
//! session and checks the promise end to end.
//!
//! The regression it guards: the accept loop installed its own ctrl-C handler
//! and reported the interrupt as end-of-stream, so `serve` returned "stopped
//! accepting connections" in milliseconds and won `Relay::run`'s `select!`
//! against the drain future that deliberately waits out the window. SIGTERM was
//! unaffected (only the drain future watches it), which is how the gap stayed
//! hidden behind systemd while an operator's ctrl-C dropped every session.
//!
//! An embedder draining on its own schedule (withdraw from DNS, wait out the
//! TTL, then drain) turns the relay's signal handling off and fires the trigger
//! itself; a session that still arrives mid-drain is sent a GOAWAY at once,
//! carrying only what is left of the window.
//!
//! The drain ends as soon as every session has left rather than waiting out the
//! window, which a stop-time budget depends on.
//!
//! A reconnecting client rides out a restart on the same address: the GOAWAYs
//! the drain sends its redials must not exhaust its retry window.
//!
//! The relay owns its signals from before it serves, so a signal right after
//! READY drains rather than kills it, yet a relay whose startup fails leaves
//! them to their default action for an embedder that recovers.
//!
//! A raised signal reaches every listener in the process, and a relay exits at
//! once on its second one, so the tests that raise a signal hold [`SIGINT`]:
//! `cargo test` (the Nix check phase) runs them as threads of one process.
//! `a_trigger_before_run_keeps_the_deadline` fires the trigger before `run`
//! instead of a signal.

#![cfg(unix)]

use std::sync::Mutex;
use std::time::Duration;

use moq_relay::{Config, Relay, auth};

/// Long enough that "exited immediately" and "waited out the window" cannot be
/// confused, short enough to keep the test quick: a session that never leaves
/// keeps `Relay::run` up this long.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

/// Held by every test that raises a signal, so one test's signal cannot drain (or,
/// as a second signal, kill) another test's relay.
static SIGINT: Mutex<()> = Mutex::new(());

/// Run `test` on a current-thread runtime with a large stack.
fn run_test<F: std::future::Future<Output = ()> + 'static>(test: fn() -> F) {
	// Same reason as the cluster tests: under `--all-features` a `Connection`
	// carries every transport backend, and holding one across awaits overflows
	// libtest's 2 MiB per-test stack in an unoptimized build.
	std::thread::Builder::new()
		.stack_size(32 * 1024 * 1024)
		.spawn(move || {
			tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.expect("build test runtime")
				.block_on(test());
		})
		.expect("spawn test thread")
		.join()
		.expect("test thread panicked");
}

#[test]
fn sigint_drains_sessions_before_exiting() {
	let _sigint = SIGINT.lock().unwrap_or_else(|e| e.into_inner());
	run_test(sigint_drains_sessions_before_exiting_inner);
}

#[test]
fn a_signal_right_after_ready_drains() {
	let _sigint = SIGINT.lock().unwrap_or_else(|e| e.into_inner());
	run_test(a_signal_right_after_ready_drains_inner);
}

/// The default action has to survive a failed startup, and only a fresh process
/// shows it: a handler anything else in this one installed would hide a leak.
#[test]
fn a_failed_startup_leaves_the_signals_alone() {
	const CHILD: &str = "MOQ_RELAY_FAILED_STARTUP_CHILD";
	if std::env::var_os(CHILD).is_some() {
		run_test(a_failed_startup_leaves_the_signals_alone_inner);
		return;
	}
	let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
		.args(["--exact", "shutdown_signal::a_failed_startup_leaves_the_signals_alone"])
		.env(CHILD, "1")
		.status()
		.expect("spawn the child test process");
	use std::os::unix::process::ExitStatusExt;
	assert_eq!(
		status.signal(),
		Some(libc::SIGTERM),
		"SIGTERM did not kill the process after a failed startup: {status}"
	);
}

#[test]
fn an_embedder_owns_the_signals() {
	let _sigint = SIGINT.lock().unwrap_or_else(|e| e.into_inner());
	run_test(an_embedder_owns_the_signals_inner);
}

#[test]
fn a_session_arriving_mid_drain_gets_what_is_left() {
	run_test(a_session_arriving_mid_drain_gets_what_is_left_inner);
}

#[test]
fn a_drain_ends_once_every_session_leaves() {
	run_test(a_drain_ends_once_every_session_leaves_inner);
}

#[test]
fn a_trigger_before_run_keeps_the_deadline() {
	run_test(a_trigger_before_run_keeps_the_deadline_inner);
}

#[test]
fn a_reconnecting_client_rides_out_a_restart() {
	run_test(a_reconnecting_client_rides_out_a_restart_inner);
}

async fn sigint_drains_sessions_before_exiting_inner() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	// Install tokio's SIGINT handler before anything raises one: the default
	// disposition would kill the test process instead. Held for the whole test so
	// nothing can conclude the signal is unwatched.
	let _interrupt =
		tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).expect("register SIGINT");

	let relay = Relay::load(relay_config()).await.expect("load relay");
	let port = relay.tcp_addr().expect("TCP listener bound").port();
	let run = tokio::spawn(relay.run());
	let client = client(Vec::new());

	let connection = connect(&client, port).await;
	let draining = connection.draining().expect("connected");
	// The one-shot client leaves on the GOAWAY; this peer is what keeps the drain
	// open for its whole window.
	let _straggler = straggler(port).await;

	let signalled = kio::clock::now();
	// SAFETY: `raise` is async-signal-safe, and SIGINT's disposition is tokio's
	// handler, registered above.
	assert_eq!(unsafe { libc::raise(libc::SIGINT) }, 0, "failed to raise SIGINT");

	let goaway = kio::clock::timeout(
		Duration::from_secs(5),
		moq_net::kio::wait(|waiter| draining.poll(waiter)),
	)
	.await
	.expect("no GOAWAY within 5s of SIGINT")
	.expect("session closed without a GOAWAY");
	// Empty URI: the relay is restarting, not moving. The window itself is the
	// sender's own timer here (only moq-transport puts it on the wire),
	// so it is the elapsed time below that proves it was honored.
	assert_eq!(goaway.uri(), "", "expected a reconnect-to-me GOAWAY");

	// It exits on its own, cleanly, and not before the window has passed: the point
	// of the drain is the time it buys, not the notice. This is what the old ctrl-C
	// race broke.
	kio::clock::timeout(Duration::from_secs(15), run)
		.await
		.expect("relay never exited after the drain window")
		.expect("relay task panicked")
		.expect("relay exited with an error");
	assert!(
		signalled.elapsed() >= DRAIN_TIMEOUT,
		"relay exited after {:?}, short of the {DRAIN_TIMEOUT:?} drain window",
		signalled.elapsed()
	);
}

async fn a_signal_right_after_ready_drains_inner() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let relay = Relay::load(relay_config()).await.expect("load relay");
	let port = relay.tcp_addr().expect("TCP listener bound").port();
	let ready = relay.ready();
	let run = tokio::spawn(relay.run());
	ready.wait().await.expect("relay ready");

	let connection = connect(&client(Vec::new()), port).await;
	let draining = connection.draining().expect("connected");
	// SAFETY: raise has no memory-safety preconditions. Nothing else here listens
	// for SIGTERM, so in a process of its own (nextest) the relay's handler is all
	// that keeps this from killing the test.
	assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0, "failed to raise SIGTERM");
	let goaway = kio::clock::timeout(
		Duration::from_secs(5),
		moq_net::kio::wait(|waiter| draining.poll(waiter)),
	)
	.await
	.expect("no GOAWAY within 5s of SIGTERM")
	.expect("session closed without a GOAWAY");
	assert_eq!(goaway.uri(), "", "expected a reconnect-to-me GOAWAY");
	kio::clock::timeout(Duration::from_secs(15), run)
		.await
		.expect("relay never exited after the drain")
		.expect("relay task panicked")
		.expect("relay exited with an error");
}

async fn a_failed_startup_leaves_the_signals_alone_inner() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	// The cluster starts last before the relay serves, and an unreadable token
	// fails it.
	let dir = tempfile::tempdir().expect("tempdir");
	let mut config = relay_config();
	config.cluster.connect = vec![moq_relay::cluster::Peer::new("https://127.0.0.1:9")];
	config.cluster.token = Some(dir.path().join("missing.jwt"));
	let err = Relay::load(config)
		.await
		.expect("load relay")
		.run()
		.await
		.expect_err("started without its cluster token");
	assert!(format!("{err:#}").contains("cluster token"), "{err:#}");

	// SAFETY: raise has no memory-safety preconditions. With the default action
	// this never returns.
	unsafe { libc::raise(libc::SIGTERM) };
}

async fn an_embedder_owns_the_signals_inner() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	// The embedder's own handler, which is also what keeps SIGINT from killing
	// the test process.
	let mut interrupt =
		tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).expect("register SIGINT");

	let relay = Relay::load(relay_config())
		.await
		.expect("load relay")
		.with_signals(false);
	let port = relay.tcp_addr().expect("TCP listener bound").port();
	let trigger = relay.shutdown_trigger().clone();
	let run = tokio::spawn(relay.run());
	let client = client(Vec::new());

	let connection = connect(&client, port).await;
	let draining = connection.draining().expect("connected");
	let _straggler = straggler(port).await;

	// SAFETY: `raise` is async-signal-safe, and SIGINT's disposition is tokio's
	// handler, registered above.
	assert_eq!(unsafe { libc::raise(libc::SIGINT) }, 0, "failed to raise SIGINT");
	kio::clock::timeout(Duration::from_secs(5), interrupt.recv())
		.await
		.expect("the embedder never saw SIGINT");

	// The signal is the embedder's: the relay keeps serving without a GOAWAY.
	assert!(
		kio::clock::timeout(DRAIN_TIMEOUT / 2, moq_net::kio::wait(|waiter| draining.poll(waiter)))
			.await
			.is_err(),
		"the relay drained on a signal the embedder owns"
	);
	assert!(!run.is_finished(), "the relay exited on a signal the embedder owns");

	// Until the embedder decides.
	let started = kio::clock::now();
	trigger.start();
	let goaway = kio::clock::timeout(
		Duration::from_secs(5),
		moq_net::kio::wait(|waiter| draining.poll(waiter)),
	)
	.await
	.expect("no GOAWAY within 5s of the trigger")
	.expect("session closed without a GOAWAY");
	assert_eq!(goaway.uri(), "", "expected a reconnect-to-me GOAWAY");

	kio::clock::timeout(Duration::from_secs(15), run)
		.await
		.expect("relay never exited after the drain window")
		.expect("relay task panicked")
		.expect("relay exited with an error");
	assert!(
		started.elapsed() >= DRAIN_TIMEOUT,
		"relay exited after {:?}, short of the {DRAIN_TIMEOUT:?} drain window",
		started.elapsed()
	);
}

async fn a_session_arriving_mid_drain_gets_what_is_left_inner() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let relay = Relay::load(relay_config())
		.await
		.expect("load relay")
		.with_signals(false);
	let port = relay.tcp_addr().expect("TCP listener bound").port();
	let trigger = relay.shutdown_trigger().clone();
	let run = tokio::spawn(relay.run());
	// moq-transport's GOAWAY carries its deadline on the wire, which is what this
	// test reads.
	let client = client(vec!["moq-transport-18".parse().expect("parse version")]);

	let established = connect(&client, port).await;
	let draining = established.draining().expect("connected");
	// Keeps the relay up for the arrival below once `established` leaves.
	let _straggler = straggler(port).await;
	trigger.start();
	let deadline = kio::clock::now() + DRAIN_TIMEOUT;
	let goaway = kio::clock::timeout(
		Duration::from_secs(5),
		moq_net::kio::wait(|waiter| draining.poll(waiter)),
	)
	.await
	.expect("no GOAWAY within 5s of the trigger")
	.expect("session closed without a GOAWAY");
	assert_eq!(goaway.uri(), "", "expected a reconnect-to-me GOAWAY");
	let timeout = goaway.timeout().expect("the GOAWAY carries its deadline");
	assert!(
		timeout > DRAIN_TIMEOUT / 2 && timeout <= DRAIN_TIMEOUT,
		"an established session gets the whole window, got {timeout:?}"
	);

	// A straggler dialing mid-drain (a cached DNS resolve) is still admitted,
	// then told to leave at once.
	kio::clock::sleep(DRAIN_TIMEOUT / 2).await;
	let dialed = kio::clock::now();
	let arrival = connect(&client, port).await;
	let draining = arrival.draining().expect("connected");
	let goaway = kio::clock::timeout(
		Duration::from_secs(1),
		moq_net::kio::wait(|waiter| draining.poll(waiter)),
	)
	.await
	.expect("an arrival mid-drain was not sent a GOAWAY")
	.expect("arrival closed without a GOAWAY");
	assert_eq!(goaway.uri(), "", "expected a reconnect-to-me GOAWAY");

	// Only what is left of the window, not a fresh one that would outlive the
	// relay. The wire rounds up to whole milliseconds.
	let left = deadline.saturating_duration_since(dialed) + Duration::from_millis(1);
	let timeout = goaway.timeout().expect("the GOAWAY carries its deadline");
	assert!(
		timeout <= left,
		"an arrival mid-drain got {timeout:?}, past the {left:?} left of the window"
	);

	kio::clock::timeout(Duration::from_secs(15), run)
		.await
		.expect("relay never exited after the drain window")
		.expect("relay task panicked")
		.expect("relay exited with an error");
}

async fn a_drain_ends_once_every_session_leaves_inner() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	// A window the test would time out long before, so only an early exit passes.
	let mut config = relay_config();
	config.drain_timeout = Some((Duration::from_secs(600)).into());
	config.internal.listen = Some("127.0.0.1:0".parse().expect("parse addr"));
	let relay = Relay::load(config).await.expect("load relay").with_signals(false);
	let port = relay.tcp_addr().expect("TCP listener bound").port();
	let internal = relay.internal().addr().expect("internal listener bound").port();
	let trigger = relay.shutdown_trigger().clone();
	let run = tokio::spawn(relay.run());

	let left = connect(&client(Vec::new()), port).await;
	let straggler = straggler(port).await;
	trigger.start();

	// The one-shot client leaves on its GOAWAY; the straggler is still draining.
	kio::clock::timeout(Duration::from_secs(5), left.closed())
		.await
		.expect("the one-shot client did not leave on the GOAWAY")
		.expect("the one-shot client failed");
	wait_metric(internal, "moq_relay_draining_sessions 1").await;
	assert!(!run.is_finished(), "the relay exited with a session still draining");

	drop(straggler);
	kio::clock::timeout(Duration::from_secs(5), run)
		.await
		.expect("relay kept running after every session left")
		.expect("relay task panicked")
		.expect("relay exited with an error");
}

async fn a_trigger_before_run_keeps_the_deadline_inner() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let relay = Relay::load(relay_config())
		.await
		.expect("load relay")
		.with_signals(false);
	let trigger = relay.shutdown_trigger().clone();

	// Fired while `run` is still starting. The session deadline is this instant;
	// `drain` must not open a fresh window when it finally observes the watch.
	trigger.start();
	let deadline = kio::clock::now() + DRAIN_TIMEOUT;
	kio::clock::sleep(DRAIN_TIMEOUT / 2).await;

	let run = tokio::spawn(relay.run());
	kio::clock::timeout(Duration::from_secs(15), run)
		.await
		.expect("relay never exited")
		.expect("relay task panicked")
		.expect("relay exited with an error");
	// A fresh window opened by `run` would end half a window past the deadline.
	let over = kio::clock::now().saturating_duration_since(deadline);
	assert!(
		over < DRAIN_TIMEOUT / 2,
		"run returned {over:?} past the recorded deadline, as if it opened a fresh window"
	);
}

/// A restart on the same address, with a reconnecting publisher whose retry window is a third of
/// the drain. The drain sends each of its redials an empty-URI GOAWAY until the window closes.
/// Every one of those sessions served, so none counts against the retry window, and the
/// publisher is still there to land on the relay that takes the address over.
async fn a_reconnecting_client_rides_out_a_restart_inner() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let relay = Relay::load(relay_config())
		.await
		.expect("load relay")
		.with_signals(false);
	let addr = relay.tcp_addr().expect("TCP listener bound");
	let trigger = relay.shutdown_trigger().clone();
	let run = tokio::spawn(relay.run());
	let url: url::Url = format!("tcp://{addr}/").parse().expect("parse url");

	let origin = moq_tokio::origin::spawn();
	let broadcast = origin.create_broadcast("cam").expect("create broadcast");
	broadcast.announce(Default::default()).expect("announce");
	let mut publisher = moq_tokio::connect::Config::default();
	// Redials paced well inside the drain, under a window that ends long before it.
	publisher.backoff.initial = Some((DRAIN_TIMEOUT / 15).into());
	publisher.backoff.max = Some((DRAIN_TIMEOUT / 15).into());
	publisher.backoff.timeout = Some((DRAIN_TIMEOUT / 3).into());
	let publisher = publisher
		.init(Default::default())
		.expect("client init")
		.with_publisher(&origin)
		.connect(url.clone())
		.established()
		.await
		.expect("connect");

	// Holds the drain open for its whole window, as the rest of a busy relay's sessions would.
	let straggler = straggler(addr.port()).await;

	trigger.start();
	kio::clock::timeout(Duration::from_secs(15), run)
		.await
		.expect("relay never exited after the drain window")
		.expect("relay task panicked")
		.expect("relay exited with an error");
	drop(straggler);

	let mut config = relay_config();
	config.listen.tcp.bind = Some(addr);
	let relay = Relay::load(config)
		.await
		.expect("restart on the same address")
		.with_signals(false);
	let trigger = relay.shutdown_trigger().clone();
	let run = tokio::spawn(relay.run());

	let subscriber = moq_tokio::origin::spawn();
	let consumer = subscriber.consume();
	let observer = client(Vec::new())
		.with_subscriber(subscriber)
		.connect(url)
		.established()
		.await
		.expect("connect");
	let mut following = consumer.follow("cam", None).unwrap();
	tokio::select! {
		res = following.available() => {
			res.expect("the publisher's broadcast never resolved");
		}
		res = publisher.closed() => panic!("the publisher gave up during the restart: {res:?}"),
		_ = kio::clock::sleep(Duration::from_secs(15)) => panic!("the publisher never reached the new relay"),
	}

	drop((publisher, observer));
	trigger.start();
	kio::clock::timeout(Duration::from_secs(15), run)
		.await
		.expect("relay never exited after its sessions left")
		.expect("relay task panicked")
		.expect("relay exited with an error");
}

/// A one-shot client offering `version` (every version when empty): a
/// reconnecting one would migrate on the GOAWAY and hide whether the relay
/// closed the original session.
fn client(version: Vec<moq_tokio::moq_net::Version>) -> moq_tokio::Client {
	let mut client_config = moq_tokio::connect::Config::default();
	client_config.tls.insecure = Some(true);
	client_config.version = version;
	client_config
		.init(Default::default())
		.expect("client init")
		.with_reconnect(false)
}

/// A session that stays until the drain deadline closes it: moq-lite-03 has no
/// GOAWAY message, so the relay drains it without the peer ever knowing.
async fn straggler(port: u16) -> moq_tokio::Connection {
	connect(&client(vec!["moq-lite-03".parse().expect("parse version")]), port).await
}

/// A session to the relay on `port`.
async fn connect(client: &moq_tokio::Client, port: u16) -> moq_tokio::Connection {
	let url: url::Url = format!("tcp://127.0.0.1:{port}/").parse().expect("parse url");
	client.connect(url).established().await.expect("connect")
}

/// A stream-only relay on an ephemeral loopback TCP port, fully public, with a
/// short drain window, to hand [`Relay::load`].
fn relay_config() -> Config {
	// Fully public auth: any no-JWT stream client gets the whole root.
	let mut auth = auth::Config::default();
	auth.public = vec![moq_auth::Pattern::all()];

	let mut config = Config::default();
	config.listen.tcp.bind = Some("127.0.0.1:0".parse().expect("parse addr"));
	config.auth = auth;
	config.drain_timeout = Some((DRAIN_TIMEOUT).into());
	config
}

/// Wait until the internal listener on `port` reports `line` at `/metrics`.
async fn wait_metric(port: u16, line: &str) {
	let deadline = kio::clock::now() + Duration::from_secs(5);
	loop {
		let metrics = reqwest::get(format!("http://127.0.0.1:{port}/metrics"))
			.await
			.expect("scrape metrics")
			.text()
			.await
			.expect("read metrics");
		if metrics.lines().any(|l| l == line) {
			break;
		}
		assert!(kio::clock::now() < deadline, "never saw {line} in:\n{metrics}");
		kio::clock::sleep(Duration::from_millis(25)).await;
	}
}
