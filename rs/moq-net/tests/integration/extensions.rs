#![cfg(feature = "async")]

//! A side may decline a moq-transport extension with `setup::Extensions`, and then
//! connects as a peer that does not speak it.
//!
//! Deterministic: paused time over the mock transport.

use crate::support;

use std::time::Duration;

use moq_net::{Version, origin, setup::Extensions};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

/// Long enough, in virtual time, for anything in flight to reach the far side.
const SETTLE: Duration = Duration::from_secs(1);

/// Every moq-transport draft, each of which carries the SOLICIT option.
const MOQT: [&str; 2] = ["moq-transport-18", "moq-transport-22"];

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer.with_proxy(true)
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
	let mut announced = origin.consume().routes();
	let mut live = false;
	while let Some(event) = announced.try_next() {
		match event {
			moq_net::origin::RouteEvent {
				kind:
					moq_net::origin::RouteKind::Start
					| moq_net::origin::RouteKind::Update
					| moq_net::origin::RouteKind::Restart,
				announce: update,
			} => {
				live |= update.prefix.as_str() == path;
			}
			moq_net::origin::RouteEvent {
				kind: moq_net::origin::RouteKind::End | moq_net::origin::RouteKind::Offline,
				announce: update,
			} => {
				live &= update.prefix.as_str() != path;
			}
		}
	}
	live
}

/// A side that declines MoQ Solicit invites unsolicited advertisements, so its peer
/// announces unasked and that is not held against the peer, even though the peer
/// implements the extension itself.
#[kio::test]
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

			let broadcast = publisher
				.route("room/cam", moq_net::origin::RouteOptions::hidden())
				.unwrap();
			broadcast.announce().unwrap();
			kio::clock::sleep(SETTLE).await;

			assert!(announced(&subscriber, "room/cam"), "{context}: never announced");
			assert_open(&pair, &context);
		}
	}
}

