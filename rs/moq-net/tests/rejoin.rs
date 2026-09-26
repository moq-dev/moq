//! A reader leaving and rejoining a track relayed over the in-memory mock transport.

mod support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version, group, track};
use support::harness::{MockConnectOptions, connect_mock};

/// Maximum time any single test may run before being treated as a deadlock.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Build an origin producer, spawning its driver on the ambient runtime.
fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
	producer
}

async fn read_all(group: &mut group::Consumer) -> Vec<Vec<u8>> {
	let mut frames = Vec::new();
	while let Some(frame) = group.read_frame().await.expect("group aborted") {
		frames.push(frame.payload.to_vec());
	}
	frames
}

/// The group in flight when the last reader left comes back whole on rejoin.
///
/// The relay cancels its idle upstream subscription, resetting that group mid-transfer.
/// Resuming the rejoin at the frame where the reset landed would ask upstream for a tail
/// whose head is gone, so the group would never reach the returning reader.
#[tokio::test]
async fn rejoin_recovers_the_group_reset_on_leave() {
	for version in ["moq-lite-05", "moq-lite-06"] {
		tokio::time::timeout(TEST_TIMEOUT, async {
			let publisher = produce_origin(1);
			let relay = produce_origin(2);

			let broadcast = publisher.create_broadcast("bench").unwrap();
			let track = broadcast.create_track("video", None).unwrap();
			broadcast.announce(Default::default()).unwrap();

			let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(relay.clone());
			let _pair = connect_mock(options).await;

			let consumer = relay.consume();
			consumer.routed("bench").await.unwrap();
			let remote = consumer.request_broadcast("bench").await.unwrap();

			let ts = |ms| Timestamp::from_millis(ms).unwrap();
			let prefs = || track::Subscription::default().with_max_age(Duration::from_secs(10));

			let mut group = track.append_group().unwrap();
			group.write_frame(ts(0), b"a0".as_ref()).unwrap();
			group.finish().unwrap();
			let mut open = track.append_group().unwrap();
			open.write_frame(ts(100), b"b0".as_ref()).unwrap();

			let mut sub = remote.track("video").unwrap().subscribe(prefs()).await.unwrap();
			loop {
				let mut group = sub.recv_group().await.unwrap().unwrap();
				group.read_frame().await.unwrap().unwrap();
				if group.sequence == 1 {
					break;
				}
			}

			// Leave mid-group; the publisher cuts the open group once demand is gone.
			drop(sub);
			track.unused().await.unwrap();
			open.write_frame(ts(133), b"b1".as_ref()).unwrap();
			open.finish().unwrap();

			let mut live = track.append_group().unwrap();
			live.write_frame(ts(5000), b"c0".as_ref()).unwrap();
			live.finish().unwrap();

			let mut sub = remote.track("video").unwrap().subscribe(prefs()).await.unwrap();
			let mut rejoined = Vec::new();
			loop {
				let mut group = sub.recv_group().await.unwrap().unwrap();
				let sequence = group.sequence;
				rejoined.push((sequence, read_all(&mut group).await));
				if sequence == 2 {
					break;
				}
			}
			assert!(
				rejoined.contains(&(1, vec![b"b0".to_vec(), b"b1".to_vec()])),
				"{version}: the reset group never came back whole: {rejoined:?}"
			);
		})
		.await
		.unwrap_or_else(|_| panic!("{version} timed out"));
	}
}
