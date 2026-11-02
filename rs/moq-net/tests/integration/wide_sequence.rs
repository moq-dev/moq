#![cfg(feature = "async")]

//! Group sequence boundaries, directly and through a forwarding origin.

use crate::support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

/// Published QUIC-varint sequences keep all their bits on the live feed, directly
/// and through a forwarding origin, even beyond JavaScript's exact number range.
#[kio::test]
async fn largest_quic_sequence_reaches_the_subscriber() {
	for alpn in ["moq-lite-02", "moq-lite-06", "moq-transport-18"] {
		for relayed in [false, true] {
			let version: Version = alpn.parse().unwrap();
			let publisher = produce_origin(1);
			let relay = produce_origin(2);
			let client = produce_origin(3);
			let broadcast = publisher.create_broadcast("ladder").unwrap();
			let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MICRO);
			let track = broadcast.create_track("data", info).unwrap();
			broadcast.announce(Default::default()).unwrap();
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(if relayed { relay.clone() } else { client.clone() });
			let _upstream = connect_mock(options).await;
			let _downstream = if relayed {
				let mut options = MockConnectOptions::new(version);
				options.server_publish = Some(relay.consume());
				options.client_subscribe = Some(client.clone());
				Some(connect_mock(options).await)
			} else {
				None
			};
			let remote = client
				.consume()
				.follow("ladder", None)
				.unwrap()
				.available()
				.await
				.unwrap();
			let mut sub = remote.track("data").unwrap().subscribe(None).await.unwrap();
			for sequence in [5, 1 << 32, (1 << 53) - 1, (1 << 53) + 1, (1 << 62) - 1] {
				// Timestamps stop at 2^61 - 1, the most lite-05/06 can zigzag as a first frame.
				let timestamp = Timestamp::from_micros(sequence.min((1 << 61) - 1)).unwrap();
				let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
				group.write_frame(timestamp, b"ladder".as_ref()).unwrap();
				group.finish().unwrap();
				let mut got = kio::clock::timeout(Duration::from_secs(5), sub.recv_group())
					.await
					.unwrap_or_else(|_| panic!("{alpn} relayed={relayed} sequence={sequence} hung"))
					.unwrap()
					.unwrap();
				assert_eq!(got.sequence, sequence, "{alpn} relayed={relayed}");
				let frame = got.read_frame().await.unwrap().unwrap();
				assert_eq!(frame.payload, b"ladder".as_ref());
				// Lite-02 carries no timestamps.
				let timed = alpn != "moq-lite-02";
				assert_eq!(frame.timestamp, timed.then_some(timestamp), "{alpn} relayed={relayed}");
			}
		}
	}
}

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// The relay accepts the wide sequence from lite-07, refuses that subscription to the
/// lite-06 reader that cannot encode it, and keeps the session serving other tracks.
#[kio::test]
async fn wide_sequence_fails_only_its_subscription() {
	const WIDE: u64 = 1 << 62;

	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let client = produce_origin(3);

	let broadcast = publisher.create_broadcast("bench").unwrap();
	let wide = broadcast.create_track("wide", timed()).unwrap();
	let narrow = broadcast.create_track("narrow", timed()).unwrap();
	broadcast.announce(Default::default()).unwrap();
	let mut group = wide.create_group(moq_net::group::Info { sequence: WIDE }).unwrap();
	group.write_frame(Timestamp::ZERO, b"wide".as_ref()).unwrap();
	let mut group = narrow.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, b"narrow".as_ref()).unwrap();

	let mut options = MockConnectOptions::new("moq-lite-07-wip".parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let _upstream = connect_mock(options).await;
	let mut options = MockConnectOptions::new("moq-lite-06".parse::<Version>().unwrap());
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(client.clone());
	let _downstream = connect_mock(options).await;

	// lite-07 carries the full 64 bits, so the relay holds the group.
	let cached = relay
		.consume()
		.follow("bench", None)
		.unwrap()
		.available()
		.await
		.unwrap();
	let mut sub = cached.track("wide").unwrap().subscribe(None).await.unwrap();
	assert_eq!(sub.recv_group().await.unwrap().unwrap().sequence, WIDE);

	let consumer = client.consume();
	let remote = consumer.follow("bench", None).unwrap().available().await.unwrap();

	let result = kio::clock::timeout(Duration::from_secs(5), async {
		let mut sub = remote.track("wide").unwrap().subscribe(None).await?;
		sub.recv_group().await.map(|group| group.map(|group| group.sequence))
	})
	.await
	.expect("the wide subscription hung instead of failing");
	// The group does not fit a QUIC varint. Encode failures have no dedicated stream
	// code, so the subscribe stream resets with INTERNAL_ERROR and the session stays up.
	assert!(
		matches!(result, Err(moq_net::Error::Stream(moq_net::StreamError::Internal))),
		"lite-06 should see an internal stream error, not {result:?}"
	);

	let mut sub = remote.track("narrow").unwrap().subscribe(None).await.unwrap();
	let mut group = sub.recv_group().await.unwrap().unwrap();
	assert_eq!(group.read_frame().await.unwrap().unwrap().payload, b"narrow".as_ref());
}

/// A subscription that starts inside the QUIC range and later receives a group of
/// `1 << 62` fails only that subscription. The session and the other track continue.
#[kio::test]
async fn a_later_wide_group_fails_only_that_subscription() {
	const WIDE: u64 = 1 << 62;

	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let client = produce_origin(3);

	let broadcast = publisher.create_broadcast("bench").unwrap();
	let wide = broadcast.create_track("wide", timed()).unwrap();
	let narrow = broadcast.create_track("narrow", timed()).unwrap();
	broadcast.announce(Default::default()).unwrap();
	let mut group = wide.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, b"first".as_ref()).unwrap();
	let mut group = narrow.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, b"narrow".as_ref()).unwrap();

	let mut options = MockConnectOptions::new("moq-lite-07-wip".parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let _upstream = connect_mock(options).await;
	let mut options = MockConnectOptions::new("moq-lite-06".parse::<Version>().unwrap());
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(client.clone());
	let _downstream = connect_mock(options).await;

	let consumer = client.consume();
	let remote = consumer.follow("bench", None).unwrap().available().await.unwrap();

	let mut sub = remote.track("wide").unwrap().subscribe(None).await.unwrap();
	let mut group = sub.recv_group().await.unwrap().unwrap();
	assert_eq!(group.sequence, 0);
	assert_eq!(group.read_frame().await.unwrap().unwrap().payload, b"first".as_ref());

	let mut group = wide.create_group(moq_net::group::Info { sequence: WIDE }).unwrap();
	group.write_frame(Timestamp::ZERO, b"wide".as_ref()).unwrap();

	let result = kio::clock::timeout(Duration::from_secs(5), sub.recv_group())
		.await
		.expect("the wide group hung the subscription instead of failing it");
	match result {
		Err(moq_net::Error::Stream(moq_net::StreamError::Internal)) => {}
		Err(err) => {
			panic!("the later wide group should fail the subscription with an internal stream error, not {err:?}")
		}
		Ok(_) => panic!("the later wide group should fail the subscription with an internal stream error, got a group"),
	}

	let mut sub = remote.track("narrow").unwrap().subscribe(None).await.unwrap();
	let mut group = sub.recv_group().await.unwrap().unwrap();
	assert_eq!(group.read_frame().await.unwrap().unwrap().payload, b"narrow".as_ref());
}
