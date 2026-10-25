#![cfg(feature = "async")]

//! Subscribers asking for different ranges of one track share a single upstream
//! subscription, and each still gets the groups its own range asks for.

use crate::support;

use std::time::Duration;

use moq_net::track::{Info, Range, Subscription};
use moq_net::{Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

const LITE: &[&str] = &["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"];

/// Draft-20 and later are left out: the first subscriber's absolute start goes upstream
/// in the SUBSCRIBE filter, and the subscriber never widens it when a live one joins
/// (`quest/m1/subscribe-ranges/ietf.md`), so group 3 never arrives.
const TRANSPORT: &[&str] = &["moq-transport-14", "moq-transport-17"];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) =
		moq_net::origin::Producer::new(moq_net::origin::Config::new(moq_net::Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

fn write_group(track: &moq_net::track::Producer, sequence: u64) {
	let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
	group.write_frame(Timestamp::now(), &b"x"[..]).unwrap();
	group.finish().unwrap();
}

/// The next group's sequence, or `None` once the track ends or nothing arrives in time.
async fn next(sub: &mut moq_net::track::Subscriber) -> Option<u64> {
	let group = moq_clock::timeout(TIMEOUT, sub.recv_group()).await.ok()?;
	group.expect("recv_group").map(|group| group.sequence)
}

async fn read_all(sub: &mut moq_net::track::Subscriber) -> Vec<u64> {
	let mut got = Vec::new();
	while let Some(sequence) = next(sub).await {
		got.push(sequence);
	}
	got
}

async fn subscribe(remote: &moq_net::broadcast::Consumer, subscription: Subscription) -> moq_net::track::Subscriber {
	remote
		.track("quiet")
		.unwrap()
		.subscribe(subscription)
		.await
		.expect("subscribe")
}

/// What a subscriber from group 4 and a live one each read from a quiet track whose
/// newest group is 3, then from group 4 once it arrives. `version` connects them to the
/// publisher, through a relay hop when `relay` is set; `None` reads in-process. The one
/// from group 4 subscribes first, and its request reaches the publisher before the live
/// one joins.
async fn ahead_then_live(version: Option<&str>, relay: bool) -> (Vec<u64>, Vec<u64>) {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast
		.create_track("quiet", Info::default().with_timescale(moq_net::Timescale::MILLI))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();
	for sequence in 0..=3 {
		write_group(&track, sequence);
	}

	let relay_origin = produce_origin(2);
	let subscriber = produce_origin(3);
	let mut sessions = Vec::new();
	let remote = match version {
		None => broadcast.consume(),
		Some(version) => {
			let version: Version = version.parse().unwrap();
			if relay {
				let mut options = MockConnectOptions::new(version);
				options.server_publish = Some(publisher.consume());
				options.client_subscribe = Some(relay_origin.clone());
				sessions.push(connect_mock(options).await);
			}
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(match relay {
				true => relay_origin.consume(),
				false => publisher.consume(),
			});
			options.client_subscribe = Some(subscriber.clone());
			sessions.push(connect_mock(options).await);

			let consumer = subscriber.consume();
			moq_clock::timeout(TIMEOUT, consumer.routed("bcast"))
				.await
				.expect("announce timeout")
				.expect("routed");
			moq_clock::timeout(TIMEOUT, consumer.request_broadcast("bcast", None))
				.await
				.expect("resolve timeout")
				.expect("broadcast resolves")
		}
	};

	let mut ahead = subscribe(&remote, Subscription::default().with_range(4..)).await;
	moq_clock::timeout(TIMEOUT, crate::support::wanted(track.demand()))
		.await
		.expect("no subscriber appeared")
		.unwrap();
	moq_clock::sleep(Duration::from_millis(10)).await;
	let mut live = subscribe(&remote, Subscription::default().with_ranges([Range::live()])).await;

	// The live subscriber reads group 3 before group 4 supersedes it.
	let mut live_got: Vec<u64> = next(&mut live).await.into_iter().collect();
	write_group(&track, 4);
	track.finish().unwrap();

	live_got.extend(read_all(&mut live).await);
	let ahead_got = read_all(&mut ahead).await;
	drop((track, sessions, relay_origin, subscriber, broadcast, publisher));
	(ahead_got, live_got)
}

/// Lite-07 carries every range a subscriber asks for, so the publisher is asked for only
/// the groups they name; an older wire carries the one span covering them. Either way the
/// subscriber reads just its own groups.
#[moq_clock::test]
async fn sparse_ranges_reach_the_publisher_on_lite07() {
	let sparse = [Range::groups(0..1), Range::groups(3..4)];
	for version in LITE {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		let track = broadcast
			.create_track("quiet", Info::default().with_timescale(moq_net::Timescale::MILLI))
			.unwrap();
		broadcast.announce(Default::default()).unwrap();
		for sequence in 0..=4 {
			write_group(&track, sequence);
		}

		let subscriber = produce_origin(2);
		let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(subscriber.clone());
		let session = connect_mock(options).await;
		let consumer = subscriber.consume();
		moq_clock::timeout(TIMEOUT, consumer.routed("bcast"))
			.await
			.expect("announce timeout")
			.expect("routed");
		let remote = moq_clock::timeout(TIMEOUT, consumer.request_broadcast("bcast", None))
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves");

		let subscription = Subscription::default()
			.with_max_delay(Duration::from_secs(60))
			.with_ranges(sparse);
		let mut reader = subscribe(&remote, subscription).await;
		moq_clock::timeout(TIMEOUT, crate::support::wanted(track.demand()))
			.await
			.expect("no subscriber appeared")
			.unwrap();
		let asked = track.subscription().expect("the publisher holds the demand").ranges;
		match *version {
			"moq-lite-07-wip" => assert_eq!(asked, sparse, "{version}"),
			_ => assert_eq!(asked.len(), 1, "{version}: one span, got {asked:?}"),
		}

		track.finish().unwrap();
		let mut got = read_all(&mut reader).await;
		got.sort_unstable();
		assert_eq!(got, [0, 3], "{version}");
		drop((track, session, subscriber, broadcast, publisher));
	}
}

/// A subscriber from group 4 merged with a live one doesn't floor the shared subscription
/// at 4: the live one still gets group 3.
#[moq_clock::test]
async fn a_live_subscriber_merged_with_one_ahead_still_gets_the_newest_group() {
	let want = (vec![4], vec![3, 4]);
	let mut failures = Vec::new();

	let got = ahead_then_live(None, false).await;
	if got != want {
		failures.push(format!("in-process: {got:?}"));
	}
	for version in LITE.iter().chain(TRANSPORT) {
		for relay in [false, true] {
			let got = ahead_then_live(Some(version), relay).await;
			if got != want {
				failures.push(format!("{version} relay={relay}: {got:?}"));
			}
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}
