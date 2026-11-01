#![cfg(feature = "async")]

//! A track that ends with an error keeps its history across a hop: a subscriber reads every
//! group below the end, then the error, and so does one subscribing after the first ended.
//! Before lite-07 the end has nowhere to carry its error, so the subscription resets with it.

use crate::support;

use std::time::Duration;

use moq_net::track::{Position, Subscription};
use moq_net::{Error, Hop, StreamError, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);
const GROUPS: u64 = 2;

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// Read every group from the first, returning their sequences and how the read ended.
async fn read(broadcast: &moq_net::broadcast::Consumer) -> (Vec<u64>, Option<Error>) {
	let subscription = Subscription::default()
		.with_range(0..)
		.with_max_delay(Duration::from_secs(30));
	let mut sub = moq_clock::timeout(TIMEOUT, broadcast.track("video").unwrap().subscribe(subscription))
		.await
		.expect("subscribe timeout")
		.expect("subscribe");
	let mut got = Vec::new();
	loop {
		match moq_clock::timeout(TIMEOUT, sub.recv_group())
			.await
			.expect("read timeout")
		{
			Ok(Some(group)) => got.push(group.sequence),
			Ok(None) => return (got, None),
			Err(err) => return (got, Some(err)),
		}
	}
}

#[moq_clock::test]
async fn an_errored_end_keeps_its_history_across_a_hop() {
	for name in ["moq-lite-06", "moq-lite-07-wip"] {
		let version: Version = name.parse().unwrap();
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI);
		let mut track = broadcast.create_track("video", info).unwrap();
		broadcast.announce(Default::default()).unwrap();
		for _ in 0..GROUPS {
			let mut group = track.append_group().unwrap();
			group.write_frame(Timestamp::ZERO, &b"frame"[..]).unwrap();
			group.finish().unwrap();
		}
		track.finish_at(Position::group(GROUPS), Some(Error::App(7))).unwrap();

		let subscriber = produce_origin(2);
		let mut options = MockConnectOptions::new(version);
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(subscriber.clone());
		let session = connect_mock(options).await;
		let remote = moq_clock::timeout(TIMEOUT, subscriber.consume().follow("bcast", None).unwrap().available())
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves");

		let first = read(&remote).await;
		let later = read(&remote).await;
		for (what, (got, err)) in [("first", first), ("later", later)] {
			assert_eq!(got, (0..GROUPS).collect::<Vec<_>>(), "{name} {what}");
			match name == "moq-lite-07-wip" {
				true => assert!(
					matches!(err, Some(Error::Stream(StreamError::App(7)))),
					"{name} {what}: {err:?}"
				),
				false => assert!(err.is_some(), "{name} {what}: ended cleanly"),
			}
		}
		// Lite-07 ends the subscription after SUBSCRIBE_END rather than resetting it.
		let reset = session.server_transport.resets().contains(&(64 + 7));
		assert_eq!(
			reset,
			name != "moq-lite-07-wip",
			"{name}: resets {:?}",
			session.server_transport.resets()
		);
		drop((session, track, broadcast, publisher, subscriber));
	}
}
