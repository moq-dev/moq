#![cfg(feature = "async")]

//! A route switch partway through a group: the subscribing relay `E` reads group 0 through `A`
//! until a cheaper route through `B` appears. `E` asks `B` for group 0 from the frame it holds
//! up to, and caps its subscription through `A` at that same frame, so neither link carries a
//! frame of group 0 the other does, and `E`'s reader gets each frame once.

use crate::route_change::{link, payload, priced_link, produce_origin, settle, timed};
use crate::support::harness::MockPair;

use std::time::Duration;

use crate::support::AppendNext;
use crate::support::AtStart;
use moq_net::{Version, origin, track};

/// The frames of group 0 that `link`'s publishing end sent on its Group Streams, the
/// unidirectional streams of type 0.
fn carried(link: &MockPair) -> Vec<u64> {
	let streams = link.server_transport.uni_writes(0);
	(0..4)
		.filter(|frame| {
			let payload = payload(0, *frame);
			streams
				.iter()
				.any(|stream| stream.windows(payload.len()).any(|w| w == payload.as_slice()))
		})
		.collect()
}

#[kio::test]
async fn a_switch_mid_group_sends_each_frame_over_one_route() {
	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let publisher = produce_origin(1);
	let relay_a = produce_origin(2);
	let relay_b = produce_origin(3);
	let relay_e = produce_origin(4);

	let broadcast = publisher.route("live", origin::RouteOptions::hidden()).unwrap();
	let track = broadcast.broadcast().create_track("video", timed()).unwrap();
	broadcast.announce().unwrap();

	let _p_a = link(version, &publisher, &relay_a).await;
	let _p_b = link(version, &publisher, &relay_b).await;
	let a_e = priced_link(version, &relay_a, &relay_e, Some(1_000)).await;

	let remote = relay_e
		.consume()
		.follow("live", None)
		.unwrap()
		.available()
		.await
		.unwrap()
		.unwrap();
	let prefs = track::Subscription::default().with_delay(track::Delay::default().with_max(Duration::from_secs(60)));
	let mut sub = remote.track("video").unwrap().subscribe(prefs).await.unwrap();

	let mut group0 = track.append_next().unwrap();
	group0.write_frame(group0.at_start(), payload(0, 0)).unwrap();
	group0.write_frame(group0.at_start(), payload(0, 1)).unwrap();
	let mut read0 = sub.recv_group().await.unwrap().unwrap();
	assert_eq!(read0.start, 0);
	for frame in 0..2 {
		assert_eq!(
			read0.read_frame().await.unwrap().unwrap().payload.to_vec(),
			payload(0, frame)
		);
	}
	settle().await;

	// The cheaper route through `B` wins while group 0 is still open.
	let b_e = link(version, &relay_b, &relay_e).await;
	settle().await;

	group0.write_frame(group0.at_start(), payload(0, 2)).unwrap();
	group0.write_frame(group0.at_start(), payload(0, 3)).unwrap();
	group0.finish().unwrap();
	settle().await;

	for frame in 2..4 {
		assert_eq!(
			read0.read_frame().await.unwrap().unwrap().payload.to_vec(),
			payload(0, frame)
		);
	}
	assert!(read0.read_frame().await.unwrap().is_none());
	assert_eq!(carried(&a_e), [0, 1], "the old route stops at the frame the new one resumes");
	assert_eq!(carried(&b_e), [2, 3], "the new route sends only the rest");
}
