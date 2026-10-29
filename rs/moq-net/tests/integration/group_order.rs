#![cfg(feature = "async")]

//! A subscription's `Order` decides which pending group a publisher sends first, directly
//! and through a relay, and one upstream request serves subscribers of both orders.
//!
//! The mock ignores stream priority on its own, so each test holds the group streams on
//! the hop into the subscriber and releases them by send order, the way a congested
//! sender would transmit them.

use crate::support;

use std::time::Duration;

use moq_net::track::{Info, Order, Subscription};
use moq_net::{Hop, Timescale, Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

/// A budget no group outlives, so order alone decides what goes first.
const FOREVER: Duration = Duration::from_millis((1 << 53) - 1);

/// The backlog every test writes while the subscriber's hop is held.
const BACKLOG: [u64; 4] = [0, 1, 2, 3];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// Connect a session serving `publish` to a fresh subscribing origin.
async fn connect(
	version: Version,
	publish: moq_net::origin::Consumer,
	hop: u64,
) -> (MockPair, moq_net::origin::Producer) {
	let subscriber = produce_origin(hop);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publish);
	options.client_subscribe = Some(subscriber.clone());
	(connect_mock(options).await, subscriber)
}

/// Subscribe to the test track through `origin` in `order`, then read every backlog group
/// in the order it arrives.
fn read_arrivals(origin: moq_net::origin::Producer, order: Order) -> moq_clock::sim::JoinHandle<Vec<u64>> {
	moq_clock::sim::spawn(async move {
		let consumer = origin.consume();
		let remote = consumer
			.follow("bcast", None)
			.unwrap()
			.available()
			.await
			.expect("broadcast resolves");
		let subscription = Subscription::default()
			.with_max_delay(FOREVER)
			.with_range(0..)
			.with_order(order);
		let mut sub = remote
			.track("ordered")
			.unwrap()
			.subscribe(subscription)
			.await
			.expect("subscribe");

		let mut arrivals = Vec::new();
		while arrivals.len() < BACKLOG.len() {
			let group = moq_clock::timeout(TIMEOUT, sub.recv_group())
				.await
				.expect("group timeout")
				.expect("recv_group")
				.expect("track open");
			arrivals.push(group.sequence);
		}
		arrivals
	})
}

/// Write the backlog oldest first, leaving each group open.
///
/// A held stream's FIN is acknowledged at once, which ends its group machine, and a machine
/// that ended stops following its rank. Open groups keep every stream re-ranked as the
/// newer ones queue, as the unacknowledged streams of a congested link would be.
fn write_backlog(track: &moq_net::track::Producer) -> Vec<moq_net::group::Producer> {
	BACKLOG
		.iter()
		.map(|&sequence| {
			let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
			group
				.write_frame(Timestamp::from_millis(sequence * 10).unwrap(), &b"frame"[..])
				.unwrap();
			group
		})
		.collect()
}

fn expected(order: Order) -> Vec<u64> {
	match order {
		Order::Ascending => BACKLOG.to_vec(),
		Order::Descending => BACKLOG.iter().rev().copied().collect(),
	}
}

/// Whether `version` can carry an ascending order at all.
fn carries_order(version: &str) -> bool {
	version == "moq-lite-07-wip" || version.starts_with("moq-transport")
}

/// Serve one subscriber in `order`, optionally through a relay, and return the order its
/// backlog arrived in.
async fn round(version: &str, relay: bool, order: Order) -> Vec<u64> {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast
		.create_track(
			"ordered",
			Info::default().with_timescale(Timescale::MILLI).with_max_age(FOREVER),
		)
		.unwrap();
	broadcast.announce(Default::default()).unwrap();

	let upstream = match relay {
		true => Some(connect(version, publisher.consume(), 2).await),
		false => None,
	};
	let serving = match &upstream {
		Some((_, relay)) => relay.consume(),
		None => publisher.consume(),
	};
	let (pair, subscriber) = connect(version, serving, 3).await;
	let reader = read_arrivals(subscriber.clone(), order);

	support::harness::subscribed(&track).await;
	// Simulated time advances only once every task is idle, so this settles the
	// subscription on every hop before the groups exist.
	moq_clock::sleep(Duration::from_millis(10)).await;

	pair.server_transport.hold_unis();
	let backlog = write_backlog(&track);
	// Every group stream is open and ranked before any reaches the subscriber.
	moq_clock::sleep(Duration::from_millis(10)).await;
	pair.server_transport.release_unis_by_priority();

	let got = moq_clock::timeout(TIMEOUT * 3, reader)
		.await
		.expect("reader hung")
		.expect("reader panicked");
	drop((backlog, track, pair, upstream, broadcast, publisher, subscriber));
	got
}

#[moq_clock::test]
async fn a_backlog_is_served_in_the_subscription_order() {
	let mut failures = Vec::new();
	for version in [
		"moq-lite-05",
		"moq-lite-06",
		"moq-lite-07-wip",
		"moq-transport-18",
		"moq-transport-22",
	] {
		for relay in [false, true] {
			for order in [Order::Ascending, Order::Descending] {
				let got = round(version, relay, order).await;
				// An older lite wire cannot ask for ascending, so it is served newest first.
				let served = match carries_order(version) {
					true => order,
					false => Order::Descending,
				};
				if got != expected(served) {
					failures.push(format!("{version} relay={relay} {order:?}: {got:?}"));
				}
			}
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

/// One relay request serves an ascending and a descending subscriber: upstream asks for
/// descending so the live subscriber stays fresh, and each downstream session still sends
/// its own subscriber's backlog in that subscriber's order.
#[moq_clock::test]
async fn merged_subscribers_keep_their_own_order() {
	for version in ["moq-lite-07-wip", "moq-transport-18"] {
		let version: Version = version.parse().unwrap();
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		let track = broadcast
			.create_track(
				"ordered",
				Info::default().with_timescale(Timescale::MILLI).with_max_age(FOREVER),
			)
			.unwrap();
		broadcast.announce(Default::default()).unwrap();

		let (upstream, relay) = connect(version, publisher.consume(), 2).await;
		let (descending_pair, descending_origin) = connect(version, relay.consume(), 3).await;
		let (ascending_pair, ascending_origin) = connect(version, relay.consume(), 4).await;

		let upstream_order = || track.subscription().map(|subscription| subscription.order);

		let ascending = read_arrivals(ascending_origin.clone(), Order::Ascending);
		support::harness::subscribed(&track).await;
		moq_clock::sleep(Duration::from_millis(10)).await;
		assert_eq!(
			upstream_order(),
			Some(Order::Ascending),
			"{version}: alone, its own order"
		);

		let descending = read_arrivals(descending_origin.clone(), Order::Descending);
		moq_clock::sleep(Duration::from_millis(10)).await;
		// lite-07 moves the upstream request with a SUBSCRIBE_UPDATE. This moq-transport
		// subscriber never updates a request, priority included, so its first order stands.
		let merged = match version.is_lite() {
			true => Order::Descending,
			false => Order::Ascending,
		};
		assert_eq!(upstream_order(), Some(merged), "{version}: one upstream request");

		descending_pair.server_transport.hold_unis();
		ascending_pair.server_transport.hold_unis();
		let backlog = write_backlog(&track);
		moq_clock::sleep(Duration::from_millis(10)).await;
		descending_pair.server_transport.release_unis_by_priority();
		ascending_pair.server_transport.release_unis_by_priority();

		for (reader, order) in [(descending, Order::Descending), (ascending, Order::Ascending)] {
			let got = moq_clock::timeout(TIMEOUT * 3, reader)
				.await
				.expect("reader hung")
				.expect("reader panicked");
			assert_eq!(got, expected(order), "{version} {order:?}");
		}
		drop((
			backlog,
			track,
			upstream,
			descending_pair,
			ascending_pair,
			relay,
			descending_origin,
			ascending_origin,
			broadcast,
			publisher,
		));
	}
}
