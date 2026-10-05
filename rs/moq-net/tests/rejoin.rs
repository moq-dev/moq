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

async fn read_all(group: &mut group::Consumer) -> moq_net::Result<Vec<Vec<u8>>> {
	let mut frames = Vec::new();
	while let Some(frame) = group.read_frame().await? {
		frames.push(frame.payload.to_vec());
	}
	Ok(frames)
}

/// A rejoin recovers the group reset on leave, whether or not the live edge moved past it.
///
/// The relay cancels its idle upstream subscription, resetting that group mid-transfer.
/// Resuming the rejoin at the frame where the reset landed would ask upstream for a tail
/// whose head is gone, so the group would never reach the returning reader.
#[tokio::test(start_paused = true)]
async fn rejoin_recovers_the_group_reset_on_leave() {
	for advance in [false, true] {
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
				track.demand().unused().await.unwrap();
				open.write_frame(ts(133), b"b1".as_ref()).unwrap();
				open.finish().unwrap();

				if advance {
					let mut live = track.append_group().unwrap();
					live.write_frame(ts(5000), b"c0".as_ref()).unwrap();
					live.finish().unwrap();
				}

				let mut sub = remote.track("video").unwrap().subscribe(prefs()).await.unwrap();
				let mut rejoined = Vec::new();
				// Arrival order: the reset group comes back once the route delivers it again,
				// which may be after a newer group.
				let wanted: &[u64] = if advance { &[1, 2] } else { &[1] };
				while !wanted.iter().all(|want| rejoined.iter().any(|(seq, _)| seq == want)) {
					let mut group = sub.recv_group().await.unwrap().unwrap();
					let sequence = group.sequence;
					rejoined.push((sequence, read_all(&mut group).await));
				}
				rejoined.sort_by_key(|(seq, _)| *seq);
				let reset = rejoined.iter().find(|(seq, _)| *seq == 1).expect("reset group");
				// The reader asks for the group it is missing, so the reset group comes back
				// whole even once the live edge has moved past it.
				assert_eq!(
					reset.1.as_ref().unwrap(),
					&vec![b"b0".to_vec(), b"b1".to_vec()],
					"{version}"
				);
				if advance {
					assert_eq!(rejoined.last().unwrap().1.as_ref().unwrap(), &vec![b"c0".to_vec()]);
				}
			})
			.await
			.unwrap_or_else(|_| panic!("{version} timed out"));
		}
	}
}

/// A reader rejoining a track another handle kept open is not handed what the relay cached
/// before it went idle.
///
/// A fetch-only reader holds the relay's copy of the track, so the relay keeps it when the
/// last subscriber leaves and only cancels the upstream subscription. The publisher moves
/// on meanwhile. The cache is stale until the route answers again, so the rejoining reader
/// starts at the live edge, whether the answer carries the largest position (lite-07) or
/// the first frame says where the feed is.
#[tokio::test(start_paused = true)]
async fn rejoin_skips_a_cache_kept_by_another_handle() {
	for version in [
		"moq-lite-05",
		"moq-lite-06",
		"moq-lite-07-wip",
		"moq-transport-19",
		"moq-transport-22",
	] {
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

			// Held for fetches only: it keeps the relay's copy without subscribing.
			let _held = remote.track("video").unwrap();

			let mut group = track.append_group().unwrap();
			group.write_frame(ts(0), b"old".as_ref()).unwrap();
			group.finish().unwrap();
			let mut sub = remote.track("video").unwrap().subscribe(None).await.unwrap();
			let mut group = sub.recv_group().await.unwrap().unwrap();
			assert_eq!(read_all(&mut group).await.unwrap(), [b"old".to_vec()], "{version}");
			drop((group, sub));
			track.demand().unused().await.unwrap();

			// The publisher moves on while nobody subscribes.
			for sequence in 1..=3u64 {
				let mut group = track.append_group().unwrap();
				group.write_frame(ts(sequence * 1000), b"new".as_ref()).unwrap();
				group.finish().unwrap();
			}

			let mut sub = remote.track("video").unwrap().subscribe(None).await.unwrap();
			let group = sub.recv_group().await.unwrap().unwrap();
			assert_eq!(
				group.sequence, 3,
				"{version}: the rejoining reader got the stale cache first"
			);
		})
		.await
		.unwrap_or_else(|_| panic!("{version}: timed out"));
	}
}
