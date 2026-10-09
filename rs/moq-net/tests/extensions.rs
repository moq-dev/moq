//! A side may decline a moq-transport extension with `setup::Extensions`, and then
//! connects as a peer that does not speak it.
//!
//! Deterministic: paused time over the mock transport.

mod support;

use std::time::Duration;

use moq_net::{Error, Hop, Version, origin, setup::Extensions};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

/// Long enough, in virtual time, for anything in flight to reach the far side.
const SETTLE: Duration = Duration::from_secs(1);

/// Every moq-transport draft, each of which carries the SOLICIT option.
const MOQT: [&str; 9] = [
	"moq-transport-14",
	"moq-transport-15",
	"moq-transport-16",
	"moq-transport-17",
	"moq-transport-18",
	"moq-transport-19",
	"moq-transport-20",
	"moq-transport-21",
	"moq-transport-22",
];

/// The first draft that negotiates MoQ Auth, and the newest.
const AUTH: [&str; 2] = ["moq-transport-17", "moq-transport-22"];

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

fn declined(f: impl FnOnce(&mut Extensions)) -> Extensions {
	let mut extensions = Extensions::default();
	f(&mut extensions);
	extensions
}

fn assert_open(pair: &MockPair, context: &str) {
	assert_eq!(pair.client_transport.close_reason(), None, "{context}: client closed");
	assert_eq!(pair.server_transport.close_reason(), None, "{context}: server closed");
}

/// Whether `path` is announced in `origin` right now.
fn announced(origin: &origin::Producer, path: &str) -> bool {
	let mut announced = origin.consume().announced();
	let mut live = false;
	while let Some(event) = announced.try_next() {
		match event {
			moq_net::announce::Event::Start(update) | moq_net::announce::Event::Update(update) => {
				live |= update.prefix.as_str() == path;
			}
			moq_net::announce::Event::End(update) => {
				live &= update.prefix.as_str() != path;
			}
		}
	}
	live
}

/// A side that declines MoQ Solicit invites unsolicited advertisements, so its peer
/// announces unasked and that is not held against the peer, even though the peer
/// implements the extension itself.
#[moq_net_sim::test]
async fn a_side_that_declines_solicit_takes_unsolicited_announces() {
	for name in MOQT {
		let version: Version = name.parse().unwrap();
		for server_declines in [true, false] {
			let context = format!("{name}, server declines: {server_declines}");
			let publisher = produce_origin(1);
			let subscriber = produce_origin(2);
			let no_solicit = declined(|e| e.solicit = false);

			let mut options = MockConnectOptions::new(version);
			match server_declines {
				true => {
					options.client_publish = Some(publisher.consume());
					options.server_subscribe = Some(subscriber.clone());
					options.server_extensions = no_solicit;
				}
				false => {
					options.server_publish = Some(publisher.consume());
					options.client_subscribe = Some(subscriber.clone());
					options.client_extensions = no_solicit;
				}
			}
			let pair = connect_mock(options).await;

			let broadcast = publisher.create_broadcast("room/cam").unwrap();
			broadcast.announce(Default::default()).unwrap();
			moq_net_sim::sleep(SETTLE).await;

			assert!(announced(&subscriber, "room/cam"), "{context}: never announced");
			assert_open(&pair, &context);
		}
	}
}

/// A side that declines MoQ Auth leaves it un-negotiated whichever side declines:
/// neither side holds a grant, and a token is unsupported rather than presented.
#[moq_net_sim::test]
async fn a_declined_auth_extension_is_not_negotiated() {
	for name in AUTH {
		let version: Version = name.parse().unwrap();

		// Control: offered by both, the client's connection credential earns a grant.
		let pair = connect_mock(MockConnectOptions::new(version)).await;
		let mut grant = pair.client.auth().grant();
		moq_net_sim::timeout(SETTLE, async {
			while grant.peek().is_none() {
				grant.changed().await.expect("grant watch ended");
			}
		})
		.await
		.unwrap_or_else(|_| panic!("{name}: no grant with both offering"));

		for server_declines in [true, false] {
			let context = format!("{name}, server declines: {server_declines}");
			let no_auth = declined(|e| e.auth = false);
			let mut options = MockConnectOptions::new(version);
			match server_declines {
				true => options.server_extensions = no_auth,
				false => options.client_extensions = no_auth,
			}
			let pair = connect_mock(options).await;
			moq_net_sim::sleep(SETTLE).await;

			for session in [&pair.client, &pair.server] {
				assert_eq!(session.auth().grant().peek(), None, "{context}");
				let added = moq_net_sim::timeout(SETTLE, session.auth().add("token")).await;
				assert!(
					matches!(added, Ok(Err(Error::Unsupported))),
					"{context}: {:?}",
					added.map(|res| res.map(|_| ()))
				);
			}
			assert_open(&pair, &context);
		}
	}
}
