#![cfg(feature = "async")]

//! A frame read partway when its route dies continues on the next route from the byte it
//! reached: over lite-07 the subscriber asks for only the rest of the frame, so the new route
//! sends none of the bytes already read.
//!
//! The publisher `P` is pulled by relays `A` and `B`. The subscribing relay `E` reads a large
//! frame of group 0 halfway through `A`, then a cheaper route through `B` wins and `A`'s
//! session to `E` dies. `B`'s subscription starts at group 1, so `E` FETCHes the rest of the
//! frame from `B`.

use crate::route_change::{link, priced_link, produce_origin, settle, timed};
use crate::support::harness::{MockConnectOptions, connect_mock};

use moq_net::{Error, Timestamp, Version, frame, origin, stats, track};

/// Several storage segments, so the rest spans more than one.
const SIZE: usize = 5 * 1024 * 1024 + 7;
/// Where the first route dies, partway into a segment.
const HALF: usize = 2 * 1024 * 1024 + 3;

fn pattern(len: usize) -> Vec<u8> {
	(0..len).map(|i| (i % 251) as u8).collect()
}

/// The payload bytes `registry`'s sessions sent as publisher.
fn sent(registry: &stats::Registry) -> u64 {
	registry
		.snapshot()
		.traffic()
		.into_iter()
		.filter(|(_, role, _)| *role == stats::Role::Publisher)
		.map(|(_, _, traffic)| traffic.bytes)
		.sum()
}

#[kio::test]
async fn a_half_read_frame_continues_from_its_byte() {
	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let publisher = produce_origin(1);
	let relay_a = produce_origin(2);
	let relay_b = produce_origin(3);
	let relay_e = produce_origin(4);

	let broadcast = publisher.route("live", origin::RouteOptions::hidden()).unwrap();
	let track = broadcast.broadcast().create_track("file", timed()).unwrap();
	broadcast.announce().unwrap();

	let _p_to_a = link(version, &publisher, &relay_a).await;
	let _p_to_b = link(version, &publisher, &relay_b).await;
	let a_to_e = priced_link(version, &relay_a, &relay_e, Some(1_000)).await;

	let remote = relay_e
		.consume()
		.follow("live", None)
		.unwrap()
		.available()
		.await
		.unwrap()
		.unwrap();
	let prefs =
		track::Subscription::default().with_delay(track::Delay::default().with_max(std::time::Duration::from_secs(60)));
	let mut sub = remote.track("file").unwrap().subscribe(prefs).await.unwrap();

	let payload = pattern(SIZE);
	let group0 = track.append_group().unwrap();
	let mut writing = group0
		.create_frame(frame::Info {
			size: SIZE as u64,
			timestamp: Some(Timestamp::ZERO),
		})
		.unwrap();
	writing.write(payload[..HALF].to_vec()).unwrap();

	let mut read0 = sub.recv_group().await.unwrap().unwrap();
	assert_eq!(read0.sequence, 0);
	let mut reading = read0.next_frame().await.unwrap().unwrap();
	let mut got = Vec::new();
	while got.len() < HALF {
		got.extend_from_slice(&reading.read_chunk().await.unwrap().unwrap());
	}
	assert_eq!(got, payload[..HALF]);

	let mut group1 = track.append_group().unwrap();
	group1.write_frame(Timestamp::ZERO, b"next".to_vec()).unwrap();
	group1.finish().unwrap();
	let mut read1 = sub.recv_group().await.unwrap().unwrap();
	assert_eq!(read1.sequence, 1);
	assert_eq!(&read1.read_frame().await.unwrap().unwrap().payload[..], b"next");

	// The cheaper route through `B` wins, metered so its egress shows what it sent `E`.
	let registry = stats::Registry::new(Default::default());
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(relay_b.consume());
	options.client_subscribe = Some(relay_e.clone());
	options.server_stats = registry
		.tier(stats::Tier::default())
		.session(stats::SessionInfo::new(moq_net::Path::from_static("")));
	let _b_to_e = connect_mock(options).await;
	settle().await;
	a_to_e.server.abort(Error::Cancel);
	a_to_e.client.abort(Error::Cancel);
	settle().await;

	writing.write(payload[HALF..].to_vec()).unwrap();
	writing.finish().unwrap().finish().unwrap();

	while let Some(chunk) = reading.read_chunk().await.unwrap() {
		got.extend_from_slice(&chunk);
	}
	assert!(got == payload, "the frame continues at its byte");

	// A publisher bills what the transport acknowledged, which trails what `E` read.
	settle().await;
	let rest = (SIZE - HALF) as u64;
	let sent = sent(&registry);
	assert!(sent >= rest, "{sent} bytes cannot carry the {rest} left");
	assert!(
		sent < rest + 1024,
		"{sent} bytes resent what `E` read: only {rest} were left"
	);
}
