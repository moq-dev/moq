//! A finite track read through a relay: every group below the declared end, then a clean
//! end, even when the relay's subscriber reaches the end before the groups finish.
//!
//! The relay's downstream subscription sees the declared end as soon as every group below
//! it has opened, while their payloads are still crossing the upstream hop. Its demand must
//! last until those groups drain: a relay cancels its upstream subscription once nobody
//! subscribes, and the publisher then resets every group still on the wire.

mod support;

use std::time::Duration;

use futures::{SinkExt, StreamExt};

use moq_net::track::Subscription;
use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_DELAY: Duration = Duration::from_secs(5);
const GROUPS: u64 = 4;

const VERSIONS: &[&str] = &[
	"moq-lite-03",
	"moq-lite-05",
	"moq-lite-06",
	"moq-lite-07-wip",
	"moq-transport-14",
	"moq-transport-17",
	"moq-transport-22",
];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// How the publisher's groups reach the relay.
#[derive(Clone, Copy)]
enum Arrival {
	/// Each group's header arrives in order, but its last frame is held until the end has
	/// reached the subscriber.
	HeldTails,
	/// Every group is written whole, but the newest group's stream reaches the relay
	/// before the others, after the end: QUIC does not order streams.
	NewestFirst,
}

/// Publish groups 0..4 through a relay, delivered as `arrival` says, and return the
/// groups the subscriber read whole and how its subscription ended.
async fn round(name: &str, arrival: Arrival) -> (Vec<u64>, Result<(), moq_net::Error>) {
	let version: Version = name.parse().unwrap();
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let mut track = broadcast.create_track("tail", None).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let relay = produce_origin(2);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let upstream = connect_mock(options).await;

	let subscriber = produce_origin(3);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(subscriber.clone());
	let downstream = connect_mock(options).await;

	let consumer = subscriber.consume();
	moq_net_sim::timeout(TIMEOUT, consumer.routed("bcast"))
		.await
		.expect("announce timeout")
		.expect("routed");
	let remote = moq_net_sim::timeout(TIMEOUT, consumer.request_broadcast("bcast"))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");

	let (mut heads, mut opened) = futures::channel::mpsc::unbounded();
	let reader = moq_net_sim::spawn(async move {
		let subscription = Subscription::default().with_max_delay(MAX_DELAY).with_groups(0..);
		let mut sub = remote
			.track("tail")
			.unwrap()
			.subscribe(subscription)
			.await
			.expect("subscribe");
		let mut tails = Vec::new();
		let end = loop {
			let mut group = match sub.recv_group().await {
				Ok(Some(group)) => group,
				Ok(None) => break Ok(()),
				Err(err) => break Err(err),
			};
			let head = group.read_frame().await.unwrap().expect("head frame");
			assert_eq!(&head.payload[..], b"head");
			heads.send(group.sequence).await.unwrap();
			tails.push(moq_net_sim::spawn(async move {
				let tail = group.read_frame().await?.expect("tail frame");
				assert_eq!(&tail.payload[..], b"tail");
				assert!(group.read_frame().await?.is_none(), "extra frame");
				Ok::<_, moq_net::Error>(group.sequence)
			}));
		};
		let mut seen = Vec::new();
		for tail in tails {
			match tail.await.expect("tail reader panicked") {
				Ok(sequence) => seen.push(sequence),
				Err(err) => return (seen, Err(err)),
			}
		}
		seen.sort();
		(seen, end)
	});

	moq_net_sim::timeout(TIMEOUT, track.demand().used())
		.await
		.expect("no subscriber appeared")
		.unwrap();
	track.finish_at(GROUPS).unwrap();
	match arrival {
		Arrival::HeldTails => {
			let mut groups = Vec::new();
			for sequence in 0..GROUPS {
				let mut group = track.append_group().unwrap();
				group.write_frame(Timestamp::ZERO, &b"head"[..]).unwrap();
				groups.push(group);
				// IETF requests the live edge, so observe each header before advancing it.
				assert_eq!(
					moq_net_sim::timeout(TIMEOUT, opened.next())
						.await
						.expect("head timeout"),
					Some(sequence),
				);
			}

			// Simulated time advances only once every task is idle: each group has reached
			// the subscriber, so the relay's downstream subscription has seen the end.
			moq_net_sim::sleep(Duration::from_millis(100)).await;
			for mut group in groups {
				group.write_frame(Timestamp::ZERO, &b"tail"[..]).unwrap();
				group.finish().unwrap();
			}
		}
		Arrival::NewestFirst => {
			upstream.server_transport.hold_unis();
			for _ in 0..GROUPS {
				let mut group = track.append_group().unwrap();
				group.write_frame(Timestamp::ZERO, &b"head"[..]).unwrap();
				group.write_frame(Timestamp::ZERO, &b"tail"[..]).unwrap();
				group.finish().unwrap();
				// IETF serves from the live edge, so let each group open its stream before
				// the next one supersedes it.
				moq_net_sim::sleep(Duration::from_millis(10)).await;
			}

			// Once idle, the publisher has opened every stream and ended the subscription,
			// so the relay knows the end before any group arrives.
			moq_net_sim::sleep(Duration::from_millis(100)).await;
			// Deliver streams newest first (IETF also carries the end on a stream of its
			// own) until the last group reaches the subscriber, at the declared boundary.
			let newest = loop {
				let held = upstream.server_transport.release_newest_uni();
				moq_net_sim::sleep(Duration::from_millis(10)).await;
				if let Ok(head) = opened.try_recv() {
					break head;
				}
				assert!(held > 0, "{name}: no group reached the subscriber");
			};
			assert_eq!(newest, GROUPS - 1, "{name}");
			moq_net_sim::sleep(Duration::from_millis(100)).await;
			upstream.server_transport.release_unis();
		}
	}

	// Lite03 carries no declared end: each of the two hops waits one max-delay grace.
	let outcome = moq_net_sim::timeout(MAX_DELAY * 2 + TIMEOUT, reader)
		.await
		.unwrap_or_else(|_| panic!("{name}: the track never ended"))
		.expect("reader panicked");
	drop((track, broadcast, upstream, downstream, publisher, relay, subscriber));
	outcome
}

#[moq_net_sim::test]
async fn relay_keeps_groups_in_flight_past_the_end() {
	for version in VERSIONS {
		let (seen, end) = round(version, Arrival::HeldTails).await;
		assert_eq!(seen, [0, 1, 2, 3], "{version}: end={end:?}");
		assert!(end.is_ok(), "{version}: ended with {end:?}");
	}
}

/// The relay learns the end, then receives the last group's stream before the others: it
/// keeps serving until the upstream tail settles instead of ending at the boundary.
#[moq_net_sim::test]
async fn relay_waits_for_reordered_upstream_headers() {
	for version in VERSIONS {
		let (seen, end) = round(version, Arrival::NewestFirst).await;
		assert_eq!(seen, [0, 1, 2, 3], "{version}: end={end:?}");
		assert!(end.is_ok(), "{version}: ended with {end:?}");
	}
}
