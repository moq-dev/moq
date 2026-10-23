#![cfg(feature = "async")]

//! Each side advertises its implementation in SETUP and reads the peer's.
//!
//! Deterministic: paused time over the mock transport.

use crate::support;

use moq_net::Version;
use support::harness::{MockConnectOptions, connect_mock};

/// Every version, and whether its SETUP carries an implementation.
const VERSIONS: [(&str, bool); 16] = [
	("moq-lite-01", false),
	("moq-lite-02", false),
	("moq-lite-03", false),
	("moq-lite-04", false),
	("moq-lite-05", true),
	("moq-lite-06", true),
	("moq-lite-07-wip", true),
	("moq-transport-14", true),
	("moq-transport-15", true),
	("moq-transport-16", true),
	("moq-transport-17", true),
	("moq-transport-18", true),
	("moq-transport-19", true),
	("moq-transport-20", true),
	("moq-transport-21", true),
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

#[moq_clock::test]
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
#[moq_clock::test]
async fn the_default_names_the_crate_and_empty_omits_it() {
	for name in ["moq-lite-06", "moq-transport-16", "moq-transport-22"] {
		let version: Version = name.parse().unwrap();
		let (seen_by_server, seen_by_client) = exchange(version, None, Some("")).await;
		assert_eq!(seen_by_server.as_deref(), Some(moq_net::IMPLEMENTATION), "{name}");
		assert_eq!(seen_by_client, None, "{name}");
	}
}
