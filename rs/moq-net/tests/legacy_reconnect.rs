//! A publisher on a wire with no hop ids, reconnecting through a relay, is a new first
//! hop downstream.
//!
//! moq-transport without the Cluster extension names no publisher, so the relay it
//! connects to stamps each connection with a random Hop ID of its own. A reconnect is a
//! new connection and so a new first hop.

mod support;

use std::time::Duration;

use moq_net::{Hop, Version, announce, origin};
use support::harness::{MockConnectOptions, MockPair, connect_mock, peer};

const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const PATH: &str = "room/cam";

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
	producer
}

/// Connect `publisher` to `relay` over draft-14, which carries no hop ids.
async fn connect_legacy(publisher: &origin::Producer, relay: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new("moq-transport-14".parse::<Version>().unwrap());
	options.client_publish = Some(publisher.consume());
	options.server_subscribe = Some(relay.clone());
	connect_mock(options).await
}

/// The first hop of the next active update for [`PATH`]. A withdrawal in between is
/// skipped: whether the relay saw the new connection before the old one dropped is a
/// race, and either way the path comes back.
async fn next_first_hop(announced: &mut announce::Consumer) -> Hop {
	loop {
		let update = match announced.next().await.expect("announce cursor ended") {
			announce::Event::Start(update) | announce::Event::Update(update) => update,
			announce::Event::End(_) | announce::Event::Live => continue,
		};
		if update.prefix.as_str() != PATH {
			continue;
		}
		return *update.route.hops.iter().next().expect("a route names its first hop");
	}
}

#[tokio::test]
async fn a_legacy_reconnect_is_a_new_first_hop_downstream() {
	for mesh in ["moq-lite-06", "moq-transport-17"] {
		tokio::time::timeout(TEST_TIMEOUT, async {
			let publisher = produce_origin(9);
			let relay = produce_origin(1);
			let downstream = produce_origin(2);
			let _mesh = peer(mesh.parse::<Version>().unwrap(), &relay, &downstream).await;
			let mut announced = downstream.consume().announced();

			let broadcast = publisher.create_broadcast(PATH).unwrap();
			broadcast.announce(Default::default()).unwrap();

			let first = connect_legacy(&publisher, &relay).await;
			let before = next_first_hop(&mut announced).await;
			assert_ne!(before, Hop::UNKNOWN, "{mesh}: the relay stamps an unnamed publisher");
			assert_ne!(before, Hop::new(9).unwrap(), "{mesh}: draft-14 never carried the id");

			// The publisher reconnects: the same content, but nothing on the wire says so.
			let second = connect_legacy(&publisher, &relay).await;
			drop(first);
			let mut after = next_first_hop(&mut announced).await;
			while after == before {
				after = next_first_hop(&mut announced).await;
			}
			assert_ne!(after, Hop::UNKNOWN, "{mesh}: the relay stamps the new connection too");

			drop(second);
		})
		.await
		.expect("test timed out");
	}
}
