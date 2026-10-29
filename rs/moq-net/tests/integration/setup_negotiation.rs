#![cfg(feature = "async")]

//! A transport that negotiated no version ALPN, such as WebTransport in a browser that cannot
//! offer subprotocols, negotiates the version in SETUP instead.

use crate::support::{
	harness::{now, spawn},
	mock::create_mock_session_pair,
};
use moq_net::{Session, Version, Versions, origin};

fn origin() -> origin::Producer {
	let (origin, driver) = origin::Producer::new(Default::default());
	spawn(driver);
	origin
}

/// Dial `client` against `server` with no ALPN, the client discovering what `upstream` serves.
async fn dial(
	client: Versions,
	server: Versions,
	upstream: &origin::Producer,
	downstream: &origin::Producer,
) -> (moq_net::Result<Session>, moq_net::Result<Session>) {
	let (dial, serve) = create_mock_session_pair(Default::default());
	futures::join!(
		async {
			let (session, driver) = moq_net::Client::new()
				.with_versions(client)
				.with_subscriber(downstream.clone())
				.connect(now(), dial)
				.await?;
			spawn(driver);
			Ok(session)
		},
		async {
			let (session, driver) = moq_net::Server::new()
				.with_versions(server)
				.with_publisher(upstream.consume())
				.accept(now(), serve)
				.await?;
			spawn(driver);
			Ok(session)
		},
	)
}

#[moq_clock::test]
async fn every_lite_version_negotiates_in_setup() {
	for name in [
		"moq-lite-07-wip",
		"moq-lite-06",
		"moq-lite-05",
		"moq-lite-04",
		"moq-lite-03",
	] {
		let version: Version = name.parse().unwrap();
		let versions: Versions = vec![version, "moq-lite-02".parse().unwrap()].into();
		let (upstream, downstream) = (origin(), origin());
		let broadcast = upstream.create_broadcast("room").unwrap();
		broadcast.announce(Default::default()).unwrap();

		let (client, server) = dial(versions.clone(), versions, &upstream, &downstream).await;
		let sessions = [client.unwrap(), server.unwrap()];
		for session in &sessions {
			assert_eq!(session.version(), version, "{name}");
		}
		// The session runs on the selected version, not on the SETUP stream.
		crate::support::announced(&downstream.consume(), "room").await;
	}
}

#[moq_clock::test]
async fn disjoint_versions_are_refused() {
	let (upstream, downstream) = (origin(), origin());
	let (client, server) = dial(
		"moq-lite-06".parse::<Version>().unwrap().into(),
		"moq-lite-07-wip".parse::<Version>().unwrap().into(),
		&upstream,
		&downstream,
	)
	.await;
	assert!(matches!(server, Err(moq_net::Error::Version)), "{:?}", server.err());
	assert!(client.is_err());
}
