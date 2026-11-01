//! The relay names itself in SETUP unless the operator renames or omits it, and the
//! auth request carries what the dialing side advertised.

use std::time::Duration;

use moq_relay::{Config, Relay};

const TIMEOUT: Duration = Duration::from_secs(10);

/// What the relay advertises when the operator sets nothing.
fn relay_default() -> String {
	format!("moq-relay/{} {}", env!("CARGO_PKG_VERSION"), moq_net::IMPLEMENTATION)
}

/// A relay on a loopback TCP listener, configured by `set`, whose admissions the test decides.
async fn spawn_relay(set: impl FnOnce(&mut Config)) -> (u16, moq_relay::auth::Admissions, tokio::task::JoinHandle<()>) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let mut config = Config::default();
	config.listen.version = vec!["moq-lite-07-wip".parse().expect("lite-07")];
	config.connect.version = config.listen.version.clone();
	config.listen.tcp.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	set(&mut config);
	let mut relay = Relay::load(config).await.expect("load relay").with_signals(false);
	let port = relay.tcp_addr().expect("TCP listener is configured").port();
	let admissions = relay.admissions().expect("embedded auth");
	let task = tokio::spawn(async move {
		let _ = relay.run().await;
	});
	(port, admissions, task)
}

/// Admit the next session, returning the implementation its auth request carried.
async fn admit_next(admissions: &mut moq_relay::auth::Admissions) -> Option<String> {
	let admission = kio::clock::timeout(TIMEOUT, admissions.next())
		.await
		.expect("admission timeout")
		.expect("admission");
	let implementation = admission.request.implementation.clone();
	let all: moq_auth::Patterns = [moq_auth::Pattern::all()].into_iter().collect();
	admission.grant(moq_auth::lease::Consumer::fixed(moq_auth::Grant::new(all.clone(), all)));
	implementation
}

/// A client advertising its own value reaches the auth server with it, and reads back
/// the relay's: the default, an override, or nothing once configured empty.
#[tokio::test]
async fn the_relay_and_client_exchange_implementations() {
	for (configured, expected) in [
		(None, Some(relay_default())),
		(Some("edge/1"), Some("edge/1".to_string())),
		(Some(""), None),
	] {
		let (port, mut admissions, relay) = spawn_relay(|config| {
			config.listen.implementation = configured.map(str::to_owned);
		})
		.await;

		let transport = qmux::tcp::connect(("127.0.0.1", port), {
			let mut config = qmux::Config::new(qmux::Version::QMux01);
			config.protocol = qmux::Protocol::Negotiate(vec!["moq-lite-07-wip".into()]);
			config
		})
		.await
		.expect("connect TCP");
		let (session, driver) = moq_net::Client::new()
			.with_versions("moq-lite-07-wip".parse::<moq_net::Version>().expect("lite-07").into())
			.with_implementation("app/1 moq-net-rs/9")
			.connect(moq_net::time::Instant::now(), transport)
			.await
			.expect("client handshake");
		tokio::spawn(moq_net::time::run(driver));

		assert_eq!(
			admit_next(&mut admissions).await.as_deref(),
			Some("app/1 moq-net-rs/9"),
			"{configured:?}"
		);
		let seen = kio::clock::timeout(TIMEOUT, session.peer_implementation())
			.await
			.expect("relay SETUP timeout");
		assert_eq!(seen, expected, "{configured:?}");
		relay.abort();
	}
}

/// A relay's cluster dials advertise the same value its listener does.
#[tokio::test]
async fn cluster_dials_advertise_the_relay() {
	let (port, mut admissions, upstream) = spawn_relay(|_| {}).await;
	let (_, _downstream_admissions, downstream) = spawn_relay(|config| {
		config.listen.implementation = Some("edge/1".to_string());
		config.cluster.connect = vec![moq_relay::cluster::Peer::new(format!("tcp://127.0.0.1:{port}"))];
	})
	.await;

	assert_eq!(admit_next(&mut admissions).await.as_deref(), Some("edge/1"));
	downstream.abort();
	upstream.abort();
}
