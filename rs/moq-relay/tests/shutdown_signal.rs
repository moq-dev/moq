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
//! Each signal test raises or handles process signals, so they rely on
//! nextest's process-per-test isolation. `a_trigger_before_run_keeps_the_deadline`
//! fires the trigger before `run` instead of a signal.

#![cfg(unix)]

use std::{net::TcpListener, time::Duration};

use moq_relay::{Config, Relay, auth};

/// Long enough that "exited immediately" and "waited out the window" cannot be
/// confused, short enough to keep the test quick: `Relay::run` sleeps this plus
/// one second before exiting.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

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
	run_test(sigint_drains_sessions_before_exiting_inner);
}

#[test]
fn an_embedder_owns_the_signals() {
	run_test(an_embedder_owns_the_signals_inner);
}

#[test]
fn a_session_arriving_mid_drain_gets_what_is_left() {
	run_test(a_session_arriving_mid_drain_gets_what_is_left_inner);
}

#[test]
fn a_trigger_before_run_keeps_the_deadline() {
	run_test(a_trigger_before_run_keeps_the_deadline_inner);
}

async fn sigint_drains_sessions_before_exiting_inner() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	// Install tokio's SIGINT handler before anything raises one: the default
	// disposition would kill the test process instead. Held for the whole test so
	// nothing can conclude the signal is unwatched.
	let _interrupt =
		tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).expect("register SIGINT");

	let (port, config) = relay_config();
	let relay = Relay::load(config).await.expect("load relay");
	let run = tokio::spawn(relay.run());
	wait_listening(port).await;
	let client = client(Vec::new());

	let connection = connect(&client, port).await;
	let draining = connection.draining().expect("connected");

	let signalled = std::time::Instant::now();
	// SAFETY: `raise` is async-signal-safe, and SIGINT's disposition is tokio's
	// handler, registered above.
	assert_eq!(unsafe { libc::raise(libc::SIGINT) }, 0, "failed to raise SIGINT");

	let goaway = tokio::time::timeout(Duration::from_secs(5), draining.recv())
		.await
		.expect("no GOAWAY within 5s of SIGINT")
		.expect("session closed without a GOAWAY");
	// Empty URI: the relay is restarting, not moving. The window itself is the
	// sender's own timer here (only moq-transport draft-17+ puts it on the wire),
	// so it is the elapsed time below that proves it was honored.
	assert_eq!(goaway.uri(), "", "expected a reconnect-to-me GOAWAY");

	// Mid-window the relay is still running: the point of the drain is the time it
	// buys, not the notice. This is what the old ctrl-C race broke.
	tokio::time::sleep(DRAIN_TIMEOUT / 2).await;
	assert!(
		!run.is_finished(),
		"relay exited {:?} after SIGINT, well inside the {DRAIN_TIMEOUT:?} drain window",
		signalled.elapsed()
	);

	// Then it exits on its own, cleanly.
	tokio::time::timeout(Duration::from_secs(15), run)
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

async fn an_embedder_owns_the_signals_inner() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	// The embedder's own handler, which is also what keeps SIGINT from killing
	// the test process.
	let mut interrupt =
		tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).expect("register SIGINT");

	let (port, config) = relay_config();
	let relay = Relay::load(config).await.expect("load relay").with_signals(false);
	let trigger = relay.shutdown_trigger().clone();
	let run = tokio::spawn(relay.run());
	wait_listening(port).await;
	let client = client(Vec::new());

	let connection = connect(&client, port).await;
	let draining = connection.draining().expect("connected");

	// SAFETY: `raise` is async-signal-safe, and SIGINT's disposition is tokio's
	// handler, registered above.
	assert_eq!(unsafe { libc::raise(libc::SIGINT) }, 0, "failed to raise SIGINT");
	tokio::time::timeout(Duration::from_secs(5), interrupt.recv())
		.await
		.expect("the embedder never saw SIGINT");

	// The signal is the embedder's: the relay keeps serving without a GOAWAY.
	assert!(
		tokio::time::timeout(Duration::from_secs(1), draining.recv())
			.await
			.is_err(),
		"the relay drained on a signal the embedder owns"
	);
	assert!(!run.is_finished(), "the relay exited on a signal the embedder owns");

	// Until the embedder decides.
	let started = std::time::Instant::now();
	trigger.start();
	let goaway = tokio::time::timeout(Duration::from_secs(5), draining.recv())
		.await
		.expect("no GOAWAY within 5s of the trigger")
		.expect("session closed without a GOAWAY");
	assert_eq!(goaway.uri(), "", "expected a reconnect-to-me GOAWAY");

	tokio::time::timeout(Duration::from_secs(15), run)
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

	let (port, config) = relay_config();
	let relay = Relay::load(config).await.expect("load relay").with_signals(false);
	let trigger = relay.shutdown_trigger().clone();
	let run = tokio::spawn(relay.run());
	wait_listening(port).await;
	// moq-transport-17 is the first version whose GOAWAY carries its deadline on
	// the wire, which is what this test reads.
	let client = client(vec!["moq-transport-17".parse().expect("parse version")]);

	let established = connect(&client, port).await;
	trigger.start();
	let deadline = std::time::Instant::now() + DRAIN_TIMEOUT;
	let goaway = tokio::time::timeout(
		Duration::from_secs(5),
		established.draining().expect("connected").recv(),
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
	tokio::time::sleep(DRAIN_TIMEOUT / 2).await;
	let dialed = std::time::Instant::now();
	let arrival = connect(&client, port).await;
	let goaway = tokio::time::timeout(Duration::from_secs(1), arrival.draining().expect("connected").recv())
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

	tokio::time::timeout(Duration::from_secs(15), run)
		.await
		.expect("relay never exited after the drain window")
		.expect("relay task panicked")
		.expect("relay exited with an error");
}

async fn a_trigger_before_run_keeps_the_deadline_inner() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let (_port, config) = relay_config();
	let relay = Relay::load(config).await.expect("load relay").with_signals(false);
	let trigger = relay.shutdown_trigger().clone();

	// Fired while `run` is still starting. The session deadline is this instant;
	// `drain` must not open a fresh window when it finally observes the watch.
	trigger.start();
	let deadline = std::time::Instant::now() + DRAIN_TIMEOUT;
	tokio::time::sleep(DRAIN_TIMEOUT / 2).await;

	let run = tokio::spawn(relay.run());
	tokio::time::timeout(Duration::from_secs(15), run)
		.await
		.expect("relay never exited")
		.expect("relay task panicked")
		.expect("relay exited with an error");
	let over = std::time::Instant::now().saturating_duration_since(deadline);
	assert!(
		over <= Duration::from_secs(2),
		"run returned {over:?} past the recorded deadline, not the one-second grace"
	);
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

/// A session to the relay on `port`.
async fn connect(client: &moq_tokio::Client, port: u16) -> moq_tokio::Connection {
	let url: url::Url = format!("tcp://127.0.0.1:{port}/").parse().expect("parse url");
	client.connect(url).established().await.expect("connect")
}

/// A stream-only relay on a free loopback TCP port, fully public, with a short
/// drain window. Returns the port and the config to hand [`Relay::load`].
fn relay_config() -> (u16, Config) {
	// The listener is bound by `Relay::run`, not here, so this leaves the usual
	// probe/bind gap; on loopback it is not worth retrying around.
	let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
	let port = probe.local_addr().expect("local addr").port();
	drop(probe);

	// Fully public auth: any no-JWT stream client gets the whole root.
	let mut auth = auth::Config::default();
	auth.public = vec![moq_auth::Pattern::all()];

	let mut config = Config::default();
	config.listen.tcp.bind = Some(format!("127.0.0.1:{port}").parse().expect("parse addr"));
	config.auth = auth;
	config.drain_timeout = DRAIN_TIMEOUT;

	(port, config)
}

async fn wait_listening(port: u16) {
	let deadline = std::time::Instant::now() + Duration::from_secs(5);
	loop {
		if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
			break;
		}
		assert!(
			std::time::Instant::now() < deadline,
			"relay never became ready on port {port}"
		);
		tokio::time::sleep(Duration::from_millis(25)).await;
	}
}
