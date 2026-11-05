#![cfg(feature = "async")]

//! Hop IDs span 64 bits. A link whose varint stops at 2^62 - 1 carries a larger
//! one as 0, the reserved unknown hop.

use crate::support;

use moq_net::{Hop, Version, cache, origin};
use support::harness::peer;

fn produce(hop: u64) -> origin::Producer {
	let mut config = origin::Config::new(Hop::new(hop).unwrap());
	config.pool = cache::Pool::unbounded();
	let (producer, driver) = origin::Producer::new(config);
	support::harness::spawn(driver);
	producer
}

/// A draft-18 peer declaring `u64::MAX` keeps its session and its place in the path.
/// Forwarded on, the hop survives lite-07's leading-ones varint and becomes 0 on
/// lite-06's QUIC varint, while the relay's own hop still follows it.
#[kio::test]
async fn widest_hop_crosses_each_varint_form() {
	for (downstream, forwarded) in [("moq-lite-06", Hop::UNKNOWN.id()), ("moq-lite-07-wip", u64::MAX)] {
		let source = produce(u64::MAX);
		let relay = produce(2);
		let sink = produce(3);
		let upstream: Version = "moq-transport-18".parse().unwrap();
		let _upstream = peer(upstream, &source, &relay).await;
		let _downstream = peer(downstream.parse().unwrap(), &relay, &sink).await;
		let _route = source.requests("room").unwrap();
		kio::clock::sim::idle().await;

		let ids = |route: origin::Route| route.hops.iter().map(|hop| hop.id()).collect::<Vec<_>>();
		let at_relay = crate::support::announced(&relay.consume(), "room").await;
		assert_eq!(ids(at_relay), [u64::MAX], "{downstream}");
		let at_sink = crate::support::announced(&sink.consume(), "room").await;
		assert_eq!(ids(at_sink), [forwarded, 2], "{downstream}");
	}
}
