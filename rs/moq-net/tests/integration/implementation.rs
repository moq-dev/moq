#![cfg(feature = "async")]

//! Each side advertises its implementation in SETUP and reads the peer's.
//!
//! Deterministic: paused time over the mock transport.

use crate::support;

use moq_net::Version;
use support::harness::{MockConnectOptions, connect_mock};

/// Every version, and whether its SETUP carries an implementation.
const VERSIONS: [(&str, bool); 9] = [
	("moq-lite-01", false),
	("moq-lite-02", false),
	("moq-lite-03", false),
	("moq-lite-04", false),
	("moq-lite-05", false),
	("moq-lite-06", false),
	("moq-lite-07-wip", true),
	("moq-transport-18", true),
	("moq-transport-22", true),
];

/// Connect with the given overrides and read what each side learned of the other.
async fn exchange(version: Version, client: Option<&str>, server: Option<&str>) -> (Option<String>, Option<String>) {
	let mut opts = MockConnectOptions::new(version);
	opts.client_implementation = client.map(str::to_owned);
	opts.server_implementation = server.map(str::to_owned);
	let pair = connect_mock(opts).await;
	(
		pair.server.peer_implementation().await,
		pair.client.peer_implementation().await,
	)
}

#[kio::test]
async fn each_side_reads_the_others_implementation() {
	for (name, carried) in VERSIONS {
		let version: Version = name.parse().unwrap();
		let (seen_by_server, seen_by_client) = exchange(version, Some("app/1 moq-net-rs/9"), Some("relay/2")).await;
		match carried {
			true => {
				assert_eq!(seen_by_server.as_deref(), Some("app/1 moq-net-rs/9"), "{name}");
				assert_eq!(seen_by_client.as_deref(), Some("relay/2"), "{name}");
			}
			false => {
				assert_eq!(seen_by_server, None, "{name}");
				assert_eq!(seen_by_client, None, "{name}");
			}
		}
	}
}

/// Unconfigured, each side names this crate; configured empty, it advertises nothing.
#[kio::test]
async fn the_default_names_the_crate_and_empty_omits_it() {
	for name in ["moq-lite-07-wip", "moq-transport-22"] {
		let version: Version = name.parse().unwrap();
		let (seen_by_server, seen_by_client) = exchange(version, None, Some("")).await;
		assert_eq!(seen_by_server.as_deref(), Some(moq_net::IMPLEMENTATION), "{name}");
		assert_eq!(seen_by_client, None, "{name}");
	}
}

/// A SETUP too large to encode fails the dial instead of leaving the peer waiting for it.
#[kio::test]
async fn an_oversized_implementation_fails_the_dial() {
	let version: Version = "moq-lite-06".parse().unwrap();
	let mut transport = moq_net::transport::memory::Config::default();
	transport.protocol = Some(version.alpn().to_string());
	let (client, _server) = support::mock::create_mock_session_pair(transport);
	let dial = moq_net::Client::new()
		.with_versions(version.into())
		.with_implementation("x".repeat(u16::MAX.into()))
		.connect(support::harness::now(), client)
		.await;
	assert!(matches!(dial, Err(moq_net::Error::Encode(_))), "{:?}", dial.err());
}

/// A session that ends before the peer's SETUP resolves `None`, even while the caller still
/// holds the finished driver.
#[kio::test]
async fn the_wait_ends_with_the_session() {
	let version: Version = "moq-lite-06".parse().unwrap();
	let mut transport = moq_net::transport::memory::Config::default();
	transport.protocol = Some(version.alpn().to_string());
	// The server end never answers, so the client's wait for SETUP can only end with the session.
	let (client, _server) = support::mock::create_mock_session_pair(transport);
	let (session, mut driver) = moq_net::Client::new()
		.with_versions(version.into())
		.connect(support::harness::now(), client)
		.await
		.expect("the client dials without waiting for SETUP");
	session.abort(moq_net::Error::Cancel);

	// The driver outlives its result, as one kept in a struct or a join handle would.
	drop(kio::clock::sim::spawn(kio::clock::drive(move |now, waiter| {
		Ok::<_, std::convert::Infallible>(driver.poll(now, waiter).unwrap_or(None))
	})));

	let seen = kio::clock::timeout(std::time::Duration::from_secs(5), session.peer_implementation())
		.await
		.expect("the wait ends with the session");
	assert_eq!(seen, None);
}
