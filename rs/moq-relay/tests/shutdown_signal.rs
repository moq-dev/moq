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

#![cfg(unix)]

use std::{net::TcpListener, time::Duration};

use moq_relay::{AuthConfig, Config, PublicConfig, Relay};

/// Long enough that "exited immediately" and "waited out the window" cannot be
/// confused, short enough to keep the test quick: `Relay::run` sleeps this plus
/// one second before exiting.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

#[test]
fn sigint_drains_sessions_before_exiting() {
	// Same reason as the cluster tests: under `--all-features` a `Connection`
	// carries every transport backend, and holding one across awaits overflows
	// libtest's 2 MiB per-test stack in an unoptimized build.
	std::thread::Builder::new()
		.stack_size(32 * 1024 * 1024)
		.spawn(|| {
			tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.expect("build test runtime")
				.block_on(sigint_drains_sessions_before_exiting_inner());
		})
		.expect("spawn test thread")
		.join()
		.expect("test thread panicked");
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

	let mut client_config = moq_tokio::connect::Config::default();
	client_config.tls.insecure = Some(true);
	let client = client_config.init(Default::default()).expect("client init");
	// One-shot: a reconnecting client would migrate on the GOAWAY and hide whether
	// the original session outlived the signal.
	let url: url::Url = format!("tcp://127.0.0.1:{port}/").parse().expect("parse url");
	let connection = client
		.with_reconnect(false)
		.connect(url)
		.established()
		.await
		.expect("connect");
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
	assert_eq!(goaway.uri, "", "expected a reconnect-to-me GOAWAY");

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

/// A stream-only relay on a free loopback TCP port, fully public, with a short
/// drain window. Returns the port and the config to hand [`Relay::load`].
fn relay_config() -> (u16, Config) {
	// The listener is bound by `Relay::run`, not here, so this leaves the usual
	// probe/bind gap; on loopback it is not worth retrying around.
	let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
	let port = probe.local_addr().expect("local addr").port();
	drop(probe);

	// Fully public auth: any no-JWT stream client gets the whole root.
	#[allow(deprecated)]
	let public = PublicConfig::Simple(vec![String::new()]);
	let mut auth = AuthConfig::default();
	auth.public = Some(public);

	let mut config = Config::default();
	config.listen.tcp.bind = Some(format!("127.0.0.1:{port}").parse().expect("parse addr"));
	config.auth = auth;
	config.drain_timeout = Some(DRAIN_TIMEOUT);

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
