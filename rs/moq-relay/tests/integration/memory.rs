//! A relay [`Connection`] over moq-net's in-memory transport.
//!
//! No sockets, so a test on Tokio's paused clock never waits on the host: the clock
//! only advances once every message in flight has landed.

use moq_net::transport::memory;
use moq_relay::{Accepted, Connection, auth, cluster};

/// The version a test speaks unless it pins one: the newest the relay offers.
pub fn newest() -> moq_net::Version {
	moq_net::ALPNS[0].parse().expect("a known ALPN")
}

/// Dial a relay serving `cluster` through `auth` over a fresh in-memory link, as
/// `client` speaking `version`.
///
/// The relay's half runs as [`Connection::run`] on its own task, reported to the
/// decider as QUIC. Lite 05+ answers SETUP before the verdict, so a refused session
/// may connect and then close.
pub async fn connect(
	client: moq_net::Client,
	version: moq_net::Version,
	cluster: &cluster::Cluster,
	auth: &auth::Auth,
) -> Result<moq_net::Session, moq_net::Error> {
	let mut config = memory::Config::default();
	config.protocol = Some(version.alpn().to_string());
	let (client_link, relay_link) = memory::pair(config);

	let (cluster, auth) = (cluster.clone(), auth.clone());
	tokio::spawn(async move {
		let server = moq_net::Server::new().with_versions(version.into());
		let Ok(handshake) = server.accept_request(moq_net::time::Instant::now(), relay_link).await else {
			return;
		};
		let request = Accepted::new(handshake, moq_auth::Transport::Quic);
		Connection::new(request, cluster, auth).run().await;
	});

	let (session, driver) = client
		.with_versions(version.into())
		.connect(moq_net::time::Instant::now(), client_link)
		.await?;
	tokio::spawn(moq_net::time::run(driver));
	Ok(session)
}

/// An `Auth` granting everything under `public`, `public_publish`, and
/// `public_subscribe` to anyone, as the `[auth]` config does.
pub fn public(public: &[&str], publish: &[&str], subscribe: &[&str]) -> auth::Auth {
	let patterns = |texts: &[&str]| texts.iter().map(|text| text.parse().expect("pattern")).collect();
	let mut config = auth::Config::default();
	config.public = patterns(public);
	config.public_publish = patterns(publish);
	config.public_subscribe = patterns(subscribe);
	config
		.init("test", &moq_tokio::tls::Connect::default(), false)
		.expect("auth init")
}

/// A standalone cluster with default options.
pub fn cluster() -> cluster::Cluster {
	cluster::Cluster::new(cluster::Options::default()).expect("cluster init")
}
