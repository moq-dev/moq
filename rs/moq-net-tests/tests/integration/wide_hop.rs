//! Hop IDs span 64 bits. A link whose varint stops at 2^62 - 1 carries a larger
//! one as 0, the reserved unknown hop.
//!
//! The hop chain is crate-private, so it is read through the `fuzz` module.

use crate::support;

use moq_net::{Version, cache, origin};
use support::harness::peer;

fn produce(hop: u64) -> origin::Producer {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.pool = cache::Pool::unbounded();
	let (producer, driver) = origin::Producer::new(config);
	support::harness::spawn(driver);
	producer.with_proxy(true)
}

/// A `u64::MAX` hop keeps its session and its place in the path: it becomes 0 on
/// lite-06's QUIC varint, while the relay's own hop still follows it. Lite-07 carries
/// no hop chain: a claim names its origin node. moq-transport carries none either.
#[kio::test]
async fn widest_hop_crosses_each_varint_form() {
	let version: Version = "moq-lite-06".parse().unwrap();
	let source = produce(u64::MAX);
	let relay = produce(2);
	let sink = produce(3);
	let _upstream = peer(version, &source, &relay).await;
	let _downstream = peer(version, &relay, &sink).await;
	let _route = source.claim("room", moq_net::origin::RouteOptions::default()).unwrap();
	kio::clock::sim::idle().await;

	let ids = |route: origin::RouteInfo| moq_net::fuzz::route_hops(&route);
	let at_relay = crate::support::announced(&relay.consume(), "room").await;
	assert_eq!(ids(at_relay), [0]);
	let at_sink = crate::support::announced(&sink.consume(), "room").await;
	assert_eq!(ids(at_sink), [0, 2]);
}
