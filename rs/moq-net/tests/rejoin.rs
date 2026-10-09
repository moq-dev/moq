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
				track.unused().await.unwrap();
				open.write_frame(ts(133), b"b1".as_ref()).unwrap();
				open.finish().unwrap();

				if advance {
					let mut live = track.append_group().unwrap();
					live.write_frame(ts(5000), b"c0".as_ref()).unwrap();
					live.finish().unwrap();
				}

				let mut sub = remote.track("video").unwrap().subscribe(prefs()).await.unwrap();
				let mut rejoined = Vec::new();
				loop {
					let mut group = sub.recv_group().await.unwrap().unwrap();
					let sequence = group.sequence;
					rejoined.push((sequence, read_all(&mut group).await));
					if sequence == if advance { 2 } else { 1 } {
						break;
					}
				}
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

/// A reader leaving long after its last read leaves the relay holding the latest group.
///
/// A catalog's latest group can sit unread past the cache's idle expiry. A downstream
/// session that read it and then leaves must not take the relay's copy of that group with
/// it: a reader that joins afterwards needs it, and no newer group may ever come.
#[tokio::test(start_paused = true)]
async fn leaving_after_the_cache_window_keeps_the_latest_group() {
	for version in [
		"moq-lite-05",
		"moq-lite-06",
		"moq-lite-07-wip",
		"moq-transport-19",
		"moq-transport-22",
	] {
		tokio::time::timeout(TEST_TIMEOUT + moq_net::cache::DEFAULT_EXPIRY * 3, async {
			let version: Version = version.parse().unwrap();
			let publisher = produce_origin(1);
			let relay = produce_origin(2);

			let broadcast = publisher.create_broadcast("bench").unwrap();
			let track = broadcast.create_track("catalog", None).unwrap();
			broadcast.announce(Default::default()).unwrap();
			let mut group = track.append_group().unwrap();
			group.write_frame(Timestamp::ZERO, b"c0".as_ref()).unwrap();
			group.finish().unwrap();

			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(relay.clone());
			let _upstream = connect_mock(options).await;

			// Each session gets its own hop, so the relay serves it through its own front.
			let session = |hop| {
				let relay = relay.clone();
				async move {
					let client = produce_origin(hop);
					let mut options = MockConnectOptions::new(version);
					options.server_publish = Some(relay.consume());
					options.client_subscribe = Some(client.clone());
					let pair = connect_mock(options).await;
					let consumer = client.consume();
					consumer.routed("bench").await.unwrap();
					let remote = consumer.request_broadcast("bench").await.unwrap();
					(remote, pair)
				}
			};

			// Holds the relay's upstream subscription, and its copy, open throughout.
			let (holder, _holder) = session(3).await;
			let mut held = holder.track("catalog").unwrap().subscribe(None).await.unwrap();
			assert_eq!(held.recv_group().await.unwrap().unwrap().sequence, 0, "{version}");

			let (leaver, leaver_session) = session(4).await;
			let mut sub = leaver.track("catalog").unwrap().subscribe(None).await.unwrap();
			let mut group = sub.recv_group().await.unwrap().unwrap();
			assert_eq!(read_all(&mut group).await.unwrap(), [b"c0"], "{version}");
			tokio::time::sleep(moq_net::cache::DEFAULT_EXPIRY * 2).await;
			drop((group, sub, leaver, leaver_session));

			// Right after the leave, and again once the leaver's front has lingered and let go
			// of the track (`IDLE_LINGER` is as long as the cache window).
			for (hop, after) in [
				(5, Duration::from_millis(100)),
				(6, moq_net::cache::DEFAULT_EXPIRY + Duration::from_secs(1)),
			] {
				tokio::time::sleep(after).await;
				let (later, _later) = session(hop).await;
				let mut sub = later.track("catalog").unwrap().subscribe(None).await.unwrap();
				let group = tokio::time::timeout(Duration::from_secs(1), sub.recv_group()).await;
				let mut group = group
					.unwrap_or_else(|_| panic!("{version}: reader {hop} never got the latest group"))
					.unwrap()
					.unwrap();
				assert_eq!(group.sequence, 0, "{version}");
				assert_eq!(read_all(&mut group).await.unwrap(), [b"c0"], "{version}");
			}
		})
		.await
		.unwrap_or_else(|_| panic!("{version}: timed out"));
	}
}
