//! A subscriber with an explicit floor receives a group created below the first group
//! that was served, while it is still inside `max_age`.
//!
//! An unfloored pre-06 subscribe joins where the publisher starts, so that same group
//! is dropped. Lite-06 and later encode a floor of group 0 as 0, which is the only
//! spelling of both an omitted floor and group 0; the draft says that 0 is group 0.

mod support;

use std::time::Duration;

use moq_net::track::{Info, Position, Subscription};
use moq_net::{Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Long enough that two groups written one after the other are both fresh.
const BUDGET: Duration = Duration::from_secs(60);

/// A budget no group outlives, on the publisher.
const FOREVER: Duration = Duration::from_millis((1 << 53) - 1);

const LITE: &[&str] = &["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"];

const TRANSPORT: &[&str] = &["moq-transport-14", "moq-transport-17", "moq-transport-22"];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) =
		moq_net::origin::Producer::new(moq_net::origin::Config::new(moq_net::Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

fn write_group(track: &moq_net::track::Producer, sequence: u64) {
	let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
	for _ in 0..4 {
		group.write_frame(Timestamp::now(), &b"x"[..]).unwrap();
	}
	group.finish().unwrap();
}

async fn read_all(sub: &mut moq_net::track::Subscriber) -> Vec<(u64, usize)> {
	let mut got = Vec::new();
	loop {
		let next = moq_net_sim::timeout(TIMEOUT, sub.recv_group())
			.await
			.expect("recv_group timed out");
		let Some(mut group) = next.expect("recv_group") else {
			break;
		};
		let mut frames = 0;
		loop {
			let frame = moq_net_sim::timeout(TIMEOUT, group.read_frame())
				.await
				.expect("read_frame timed out")
				.expect("read_frame");
			match frame {
				Some(_) => frames += 1,
				None => break,
			}
		}
		got.push((group.sequence, frames));
	}
	got
}

/// Group 1, then a fresh group 0, over `version`. `relay` inserts one hop.
async fn session(version: &str, relay: bool, start: Option<Position>) -> Vec<(u64, usize)> {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast
		.create_track("late", Info::default().with_max_age(FOREVER))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();

	let relay_origin = produce_origin(2);
	let upstream = match relay {
		true => {
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(relay_origin.clone());
			Some(connect_mock(options).await)
		}
		false => None,
	};

	let subscriber = produce_origin(3);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(match relay {
		true => relay_origin.consume(),
		false => publisher.consume(),
	});
	options.client_subscribe = Some(subscriber.clone());
	let pair = connect_mock(options).await;

	let consumer = subscriber.consume();
	moq_net_sim::timeout(TIMEOUT, consumer.routed("bcast"))
		.await
		.expect("announce timeout")
		.expect("routed");
	let remote = moq_net_sim::timeout(TIMEOUT, consumer.request_broadcast("bcast"))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");

	let subscription = Subscription::default().with_max_age(BUDGET).with_start(start);
	let reader = moq_net_sim::spawn(async move {
		let mut sub = remote
			.track("late")
			.unwrap()
			.subscribe(subscription)
			.await
			.expect("subscribe");
		read_all(&mut sub).await
	});

	moq_net_sim::timeout(TIMEOUT, track.demand().used())
		.await
		.expect("no subscriber appeared")
		.unwrap();
	// Idle time lets the subscription settle, and again lets SUBSCRIBE_START land,
	// before the lower group is created.
	moq_net_sim::sleep(Duration::from_millis(10)).await;
	write_group(&track, 1);
	moq_net_sim::sleep(Duration::from_millis(10)).await;
	write_group(&track, 0);
	track.finish().unwrap();

	let got = moq_net_sim::timeout(TIMEOUT * 3, reader)
		.await
		.expect("reader hung")
		.expect("reader panicked");
	drop((track, pair, upstream, relay_origin, broadcast, publisher, subscriber));
	got
}

async fn in_process(start: Option<Position>) -> Vec<(u64, usize)> {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast
		.create_track("late", Info::default().with_max_age(FOREVER))
		.unwrap();
	let subscription = Subscription::default().with_max_age(BUDGET).with_start(start);
	let mut sub = track.subscribe(subscription);
	let reader = moq_net_sim::spawn(async move { read_all(&mut sub).await });
	moq_net_sim::sleep(Duration::from_millis(10)).await;
	write_group(&track, 1);
	moq_net_sim::sleep(Duration::from_millis(10)).await;
	write_group(&track, 0);
	track.finish().unwrap();
	let got = moq_net_sim::timeout(TIMEOUT * 3, reader)
		.await
		.expect("reader hung")
		.expect("reader panicked");
	drop((track, broadcast, publisher));
	got
}

#[moq_net_sim::test]
async fn an_explicit_floor_receives_a_group_created_below_the_first_served_group() {
	let want = vec![(1, 4), (0, 4)];
	let mut failures = Vec::new();

	let got = in_process(Some(Position::group(0))).await;
	if got != want {
		failures.push(format!("in-process: {got:?}"));
	}

	for version in LITE.iter().chain(TRANSPORT) {
		for relay in [false, true] {
			let got = session(version, relay, Some(Position::group(0))).await;
			if got != want {
				failures.push(format!("{version} relay={relay}: {got:?}"));
			}
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

/// Pre-06, an omitted floor joins at the first served group. Group 0, created after
/// group 1 was served, is below that join. Lite-06 cannot spell this separately from
/// a floor of group 0; that case is the explicit-floor test.
#[moq_net_sim::test]
async fn an_unfloored_lite05_join_drops_a_group_created_below_the_first_served_group() {
	let want = vec![(1, 4)];
	let mut failures = Vec::new();
	for relay in [false, true] {
		let got = session("moq-lite-05", relay, None).await;
		if got != want {
			failures.push(format!("relay={relay}: {got:?}"));
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

/// A `None` subscriber is already reading group 1. A second subscriber names group 0.
/// The fresh group 0 reaches only the second. Lite-05 is the draft that can tell the
/// two subscriptions apart on the wire.
#[moq_net_sim::test]
async fn a_relayed_explicit_floor_survives_an_unfloored_subscriber() {
	let version: Version = "moq-lite-05".parse().unwrap();
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast
		.create_track("late", Info::default().with_max_age(FOREVER))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();

	let relay = produce_origin(2);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let upstream = connect_mock(options).await;

	let unfloored_origin = produce_origin(3);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(unfloored_origin.clone());
	let unfloored_pair = connect_mock(options).await;
	let consumer = unfloored_origin.consume();
	moq_net_sim::timeout(TIMEOUT, consumer.routed("bcast"))
		.await
		.expect("announce timeout")
		.unwrap();
	let remote = consumer.request_broadcast("bcast").await.unwrap();
	let unfloored_sub = Subscription::default().with_max_age(BUDGET);
	let reader = moq_net_sim::spawn(async move {
		let mut sub = remote.track("late").unwrap().subscribe(unfloored_sub).await.unwrap();
		let mut group = moq_net_sim::timeout(TIMEOUT, sub.recv_group())
			.await
			.expect("group 1")
			.unwrap()
			.expect("group 1 ended");
		assert_eq!(group.sequence, 1);
		for _ in 0..4 {
			assert!(group.read_frame().await.unwrap().is_some());
		}
		assert!(group.read_frame().await.unwrap().is_none());
		sub
	});

	track.demand().used().await.unwrap();
	moq_net_sim::sleep(Duration::from_millis(10)).await;
	write_group(&track, 1);
	let mut unfloored = moq_net_sim::timeout(TIMEOUT, reader)
		.await
		.expect("unfloored reader")
		.unwrap();

	let floored_origin = produce_origin(4);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(floored_origin.clone());
	let floored_pair = connect_mock(options).await;
	let consumer = floored_origin.consume();
	consumer.routed("bcast").await.unwrap();
	let remote = consumer.request_broadcast("bcast").await.unwrap();
	let floored_sub = Subscription::default()
		.with_max_age(BUDGET)
		.with_start(Position::group(0));
	let floored_reader = moq_net_sim::spawn(async move {
		let mut sub = remote.track("late").unwrap().subscribe(floored_sub).await.unwrap();
		let mut saw_zero = false;
		for _ in 0..2 {
			let Some(mut group) = moq_net_sim::timeout(TIMEOUT, sub.recv_group())
				.await
				.expect("floored recv")
				.unwrap()
			else {
				break;
			};
			let sequence = group.sequence;
			while group.read_frame().await.unwrap().is_some() {}
			if sequence == 0 {
				saw_zero = true;
				break;
			}
		}
		assert!(saw_zero, "explicit floor missed group 0");
	});

	// The spawned subscribe runs to its first wait, which forwards the new floor.
	moq_net_sim::sleep(Duration::from_millis(10)).await;
	write_group(&track, 0);
	moq_net_sim::timeout(TIMEOUT, floored_reader)
		.await
		.expect("floored reader")
		.unwrap();

	let late = moq_net_sim::timeout(Duration::from_millis(50), unfloored.recv_group()).await;
	assert!(late.is_err(), "unfloored subscriber received a group below its join");

	drop((
		unfloored,
		unfloored_pair,
		floored_pair,
		upstream,
		track,
		broadcast,
		publisher,
		relay,
		unfloored_origin,
		floored_origin,
	));
}
