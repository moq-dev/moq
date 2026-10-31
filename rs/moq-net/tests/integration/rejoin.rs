#![cfg(feature = "async")]

//! A reader leaving and rejoining a track relayed over the in-memory mock transport.

use crate::support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version, announce, group, track};
use support::harness::{MockConnectOptions, connect_mock};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// Maximum time any single test may run before being treated as a deadlock.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Build an origin producer, spawning its driver on the ambient runtime.
fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
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
#[kio::test]
async fn rejoin_recovers_the_group_reset_on_leave() {
	for advance in [false, true] {
		for version in ["moq-lite-05", "moq-lite-06"] {
			kio::clock::timeout(TEST_TIMEOUT, async {
				let publisher = produce_origin(1);
				let relay = produce_origin(2);

				let broadcast = publisher.create_broadcast("bench").unwrap();
				let track = broadcast.create_track("video", timed()).unwrap();
				broadcast.announce(Default::default()).unwrap();

				let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
				options.server_publish = Some(publisher.consume());
				options.client_subscribe = Some(relay.clone());
				let _pair = connect_mock(options).await;

				let consumer = relay.consume();
				let remote = consumer.follow("bench", None).unwrap().available().await.unwrap();

				let ts = |ms| Timestamp::from_millis(ms).unwrap();
				let prefs = || track::Subscription::default().with_max_delay(Duration::from_secs(10));

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
				crate::support::unwanted(track.demand()).await.unwrap();
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
#[kio::test]
async fn rejoin_skips_a_cache_kept_by_another_handle() {
	for version in [
		"moq-lite-05",
		"moq-lite-06",
		"moq-lite-07-wip",
		"moq-transport-18",
		"moq-transport-22",
	] {
		kio::clock::timeout(TEST_TIMEOUT, async {
			let publisher = produce_origin(1);
			let relay = produce_origin(2);

			let broadcast = publisher.create_broadcast("bench").unwrap();
			let track = broadcast.create_track("video", timed()).unwrap();
			broadcast.announce(Default::default()).unwrap();

			let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(relay.clone());
			let _pair = connect_mock(options).await;

			let consumer = relay.consume();
			let remote = consumer.follow("bench", None).unwrap().available().await.unwrap();
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
			crate::support::unwanted(track.demand()).await.unwrap();

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

/// A rejoin keeps the cached group its max age still allows, though the backlog lands
/// newest first.
///
/// A fetch-only reader holds the relay's copy, so it idles with group 0 cached when the
/// last subscriber leaves. The publisher writes groups 1 to 3 meanwhile, and the rejoin
/// asks again from the cached head, which a descending track answers newest first. Group
/// 3 landing first is not a gap after group 0, so a reader asking from group 0 gets it.
#[kio::test]
async fn rejoin_keeps_the_cache_when_the_backlog_lands_newest_first() {
	for version in ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"] {
		kio::clock::timeout(TEST_TIMEOUT, async {
			let publisher = produce_origin(1);
			let relay = produce_origin(2);

			let broadcast = publisher.create_broadcast("bench").unwrap();
			let track = broadcast.create_track("video", timed()).unwrap();
			broadcast.announce(Default::default()).unwrap();

			let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(relay.clone());
			let pair = connect_mock(options).await;

			let consumer = relay.consume();
			let remote = consumer.follow("bench", None).unwrap().available().await.unwrap();
			let ts = |ms| Timestamp::from_millis(ms).unwrap();
			let prefs = || track::Subscription::default().with_max_delay(Duration::from_secs(10));

			// Held for fetches only: it keeps the relay's copy without subscribing.
			let _held = remote.track("video").unwrap();

			let mut group = track.append_group().unwrap();
			group.write_frame(ts(0), b"0".as_ref()).unwrap();
			group.finish().unwrap();
			let mut sub = remote.track("video").unwrap().subscribe(prefs()).await.unwrap();
			let mut group = sub.recv_group().await.unwrap().unwrap();
			assert_eq!(read_all(&mut group).await.unwrap(), [b"0".to_vec()], "{version}");
			drop((group, sub));
			crate::support::unwanted(track.demand()).await.unwrap();

			// The publisher moves on while nobody subscribes, all within the max age.
			for sequence in 1..=3u64 {
				let mut group = track.append_group().unwrap();
				group.write_frame(ts(sequence * 100), sequence.to_string()).unwrap();
				group.finish().unwrap();
			}

			pair.server_transport.hold_unis();
			let rejoin = remote.track("video").unwrap().subscribe(prefs().with_range(0..));
			let release = async {
				kio::clock::sleep(Duration::from_millis(100)).await;
				pair.server_transport.release_unis_reversed();
			};
			let (sub, ()) = futures::join!(rejoin, release);
			let mut sub = sub.unwrap();
			let mut rejoined = Vec::new();
			// Arrival order, newest first: whatever the reader gets once the backlog settled.
			while let Ok(group) = kio::clock::timeout(Duration::from_secs(1), sub.recv_group()).await {
				rejoined.push(group.unwrap().unwrap().sequence);
			}
			rejoined.sort();
			assert_eq!(rejoined, [0, 1, 2, 3], "{version}: the rejoin skipped the cache");
		})
		.await
		.unwrap_or_else(|_| panic!("{version}: timed out"));
	}
}

/// A reader rejoining an open group keeps reading it, and leaves the relay holding it whole.
///
/// The client still holds the group's head when it rejoins, so it may ask the relay for
/// only the rest. A later reader at the relay joins at the live edge and needs the group
/// from its first frame, so the relay's copy must keep it.
#[kio::test]
async fn rejoin_mid_group_keeps_the_head_for_later_readers() {
	for version in [
		"moq-lite-05",
		"moq-lite-06",
		"moq-lite-07-wip",
		"moq-transport-18",
		"moq-transport-22",
	] {
		kio::clock::timeout(TEST_TIMEOUT, async {
			let publisher = produce_origin(1);
			let relay = produce_origin(2);
			let client = produce_origin(3);

			let broadcast = publisher.create_broadcast("bench").unwrap();
			let track = broadcast.create_track("video", timed()).unwrap();
			broadcast.announce(Default::default()).unwrap();
			// Left open, as a JSON snapshot group stays open for its deltas.
			let mut open = track.append_group().unwrap();
			open.write_frame(Timestamp::ZERO, b"a0".as_ref()).unwrap();

			let version: Version = version.parse().unwrap();
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(relay.clone());
			let _upstream = connect_mock(options).await;
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(relay.consume());
			options.client_subscribe = Some(client.clone());
			let _downstream = connect_mock(options).await;

			let consumer = client.consume();
			let remote = consumer.follow("bench", None).unwrap().available().await.unwrap();
			let mut sub = remote.track("video").unwrap().subscribe(None).await.unwrap();
			let mut group = sub.recv_group().await.unwrap().unwrap();
			assert_eq!(group.read_frame().await.unwrap().unwrap().payload, b"a0".as_ref());
			drop((group, sub));
			crate::support::unwanted(track.demand()).await.unwrap();

			let mut rejoined = remote.track("video").unwrap().subscribe(None).await.unwrap();
			let mut rejoined = rejoined.recv_group().await.unwrap().unwrap();
			assert_eq!(rejoined.read_frame().await.unwrap().unwrap().payload, b"a0".as_ref());
			crate::support::wanted(track.demand()).await.unwrap();

			let later = relay.consume().request("bench", None).await.unwrap();
			let mut sub = later.track("video").unwrap().subscribe(None).await.unwrap();
			let mut group = sub.recv_group().await.unwrap().unwrap();
			assert_eq!(group.sequence, 0, "{version}");
			let frame = group.read_frame().await;
			assert!(
				matches!(&frame, Ok(Some(frame)) if frame.payload == b"a0".as_ref()),
				"{version}: the later reader lost the group's head: {frame:?}"
			);

			// The group stays open, so both readers get the frames written after they joined.
			open.write_frame(Timestamp::from_millis(33).unwrap(), b"a1".as_ref())
				.unwrap();
			let frame = rejoined.read_frame().await;
			assert!(
				matches!(&frame, Ok(Some(frame)) if frame.payload == b"a1".as_ref()),
				"{version}: the rejoined reader lost the open group: {frame:?}"
			);
			let frame = group.read_frame().await;
			assert!(
				matches!(&frame, Ok(Some(frame)) if frame.payload == b"a1".as_ref()),
				"{version}: the later reader lost the open group: {frame:?}"
			);
		})
		.await
		.unwrap_or_else(|_| panic!("{version}: timed out"));
	}
}

/// A rejoin whose join head never arrives still goes live once a newer group does.
///
/// The publisher accepts the join, but its stream is lost before its header. The open group
/// is gone for this copy, but the groups after it must still reach readers, local and
/// downstream, rather than wait on a head that is never coming.
#[kio::test]
async fn rejoin_goes_live_without_the_join_head() {
	for version in ["moq-transport-18", "moq-transport-22"] {
		kio::clock::timeout(TEST_TIMEOUT, async {
			let publisher = produce_origin(1);
			let relay = produce_origin(2);
			let client = produce_origin(3);

			let broadcast = publisher.create_broadcast("bench").unwrap();
			let track = broadcast.create_track("video", timed()).unwrap();
			broadcast.announce(Default::default()).unwrap();
			let mut open = track.append_group().unwrap();
			open.write_frame(Timestamp::ZERO, b"a0".as_ref()).unwrap();

			let version: Version = version.parse().unwrap();
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(relay.clone());
			let upstream = connect_mock(options).await;
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(relay.consume());
			options.client_subscribe = Some(client.clone());
			let _downstream = connect_mock(options).await;

			let consumer = client.consume();
			let remote = consumer.follow("bench", None).unwrap().available().await.unwrap();
			let mut sub = remote.track("video").unwrap().subscribe(None).await.unwrap();
			let mut group = sub.recv_group().await.unwrap().unwrap();
			assert_eq!(group.read_frame().await.unwrap().unwrap().payload, b"a0".as_ref());
			drop((group, sub));
			crate::support::unwanted(track.demand()).await.unwrap();

			// The rejoin's join stream is lost before its header.
			upstream.server_transport.hold_unis();
			let rejoin = kio::clock::sim::spawn(async move {
				let mut sub = remote.track("video").unwrap().subscribe(None).await.unwrap();
				loop {
					let mut group = sub.recv_group().await.unwrap().unwrap();
					if group.sequence == 1 {
						return read_all(&mut group).await.unwrap();
					}
				}
			});
			crate::support::wanted(track.demand()).await.unwrap();
			kio::clock::sleep(Duration::from_millis(100)).await;
			upstream.server_transport.drop_unis();

			let mut next = track.append_group().unwrap();
			next.write_frame(Timestamp::from_millis(1000).unwrap(), b"b0".as_ref())
				.unwrap();
			next.finish().unwrap();

			let local = relay.consume().request("bench", None).await.unwrap();
			let mut sub = local.track("video").unwrap().subscribe(None).await.unwrap();
			let mut group = sub.recv_group().await.unwrap().unwrap();
			assert_eq!(group.sequence, 1, "{version}");
			assert_eq!(read_all(&mut group).await.unwrap(), [b"b0".to_vec()], "{version}");

			assert_eq!(rejoin.await.unwrap(), [b"b0".to_vec()], "{version}");
		})
		.await
		.unwrap_or_else(|_| panic!("{version}: timed out"));
	}
}

/// A reader rejoining while the relay is still cancelling upstream is not handed the stale cache.
///
/// Cancelling waits on the publisher, a round trip on a slow link, but the publisher stops
/// serving as soon as the cancel lands. A reader returning in between must already find the
/// copy idle, or it takes the cached group as the live edge.
#[kio::test]
async fn rejoin_during_the_cancel_skips_the_cache() {
	for version in support::every_version() {
		kio::clock::timeout(TEST_TIMEOUT, async {
			let publisher = produce_origin(1);
			let relay = produce_origin(2);

			let broadcast = publisher.create_broadcast("bench").unwrap();
			let track = broadcast.create_track("video", timed()).unwrap();
			broadcast.announce(Default::default()).unwrap();

			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(relay.clone());
			options.latency = Duration::from_millis(50);
			let _pair = connect_mock(options).await;

			let consumer = relay.consume();
			let remote = consumer.follow("bench", None).unwrap().available().await.unwrap();
			let ts = |ms| Timestamp::from_millis(ms).unwrap();

			let mut group = track.append_group().unwrap();
			group.write_frame(ts(0), b"old".as_ref()).unwrap();
			group.finish().unwrap();
			let mut sub = remote.track("video").unwrap().subscribe(None).await.unwrap();
			let mut group = sub.recv_group().await.unwrap().unwrap();
			assert_eq!(read_all(&mut group).await.unwrap(), [b"old".to_vec()], "{version}");
			drop((group, sub));
			crate::support::unwanted(track.demand()).await.unwrap();

			// The publisher moves on while the relay's cancel is still in flight.
			for sequence in 1..=3u64 {
				let mut group = track.append_group().unwrap();
				group.write_frame(ts(sequence * 1000), b"new".as_ref()).unwrap();
				group.finish().unwrap();
			}

			let mut sub = remote.track("video").unwrap().subscribe(None).await.unwrap();
			let group = sub.recv_group().await.unwrap().unwrap();
			// A track that arrives untimed skips the cached group by the wall clock instead.
			assert_eq!(
				group.sequence, 3,
				"{version}: the rejoining reader got the stale cache first"
			);
		})
		.await
		.unwrap_or_else(|_| panic!("{version}: timed out"));
	}
}

/// A rejoining reader is not handed the cached group while the route's next group has no
/// frame yet.
///
/// The publisher moved on by one group while nobody subscribed, so the route answers the
/// rejoin with that group, right after the one the relay cached. Its header lands before its
/// first frame. Shown in between, it would leave the cached group without a stamped
/// successor, and a reader at the live edge would be handed that older group first.
#[kio::test]
async fn rejoin_waits_for_the_answers_first_frame() {
	// Lite01/02 carry no max age, so the publisher serves the relay's cached head again and
	// that answer is what makes the cache current.
	let unbudgeted = ["moq-lite-01", "moq-lite-02"];
	for version in support::every_version()
		.into_iter()
		.filter(|version| !unbudgeted.contains(&version.as_str()))
	{
		kio::clock::timeout(TEST_TIMEOUT, async {
			let publisher = produce_origin(1);
			let relay = produce_origin(2);

			let broadcast = publisher.create_broadcast("bench").unwrap();
			let track = broadcast.create_track("video", timed()).unwrap();
			broadcast.announce(Default::default()).unwrap();

			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(relay.clone());
			let pair = connect_mock(options).await;

			let consumer = relay.consume();
			let remote = consumer.follow("bench", None).unwrap().available().await.unwrap();
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
			crate::support::unwanted(track.demand()).await.unwrap();

			let mut group = track.append_group().unwrap();
			group.write_frame(ts(500), b"new".as_ref()).unwrap();
			group.finish().unwrap();

			pair.server_transport.split_unis();
			let mut sub = remote.track("video").unwrap().subscribe(None).await.unwrap();
			let early = kio::clock::timeout(Duration::from_secs(1), sub.recv_group()).await;
			assert!(
				early.is_err(),
				"{version}: the rejoining reader got the cached group {:?} first",
				early.map(|group| group.unwrap().unwrap().sequence)
			);

			pair.server_transport.release_split();
			let mut group = sub.recv_group().await.unwrap().unwrap();
			// A track that arrives untimed has no media age to skip the cached group by, so
			// its reader starts at the newest group cached and reads on from there.
			if sub.info().timescale.is_none() && group.sequence == 0 {
				group = sub.recv_group().await.unwrap().unwrap();
			}
			assert_eq!(group.sequence, 1, "{version}");
			assert_eq!(read_all(&mut group).await.unwrap(), [b"new".to_vec()], "{version}");
		})
		.await
		.unwrap_or_else(|_| panic!("{version}: timed out"));
	}
}

/// A reader leaving long after its last read leaves the relay holding the latest group.
///
/// A catalog's latest group can sit unread past the cache's idle expiry. A downstream
/// session that read it and then leaves must not take the relay's copy of that group with
/// it: a reader that joins afterwards needs it, and no newer group may ever come.
#[kio::test]
async fn leaving_after_the_cache_window_keeps_the_latest_group() {
	for version in [
		"moq-lite-05",
		"moq-lite-06",
		"moq-lite-07-wip",
		"moq-transport-18",
		"moq-transport-22",
	] {
		kio::clock::timeout(TEST_TIMEOUT + moq_net::cache::DEFAULT_EXPIRY * 3, async {
			let version: Version = version.parse().unwrap();
			let publisher = produce_origin(1);
			let relay = produce_origin(2);

			let broadcast = publisher.create_broadcast("bench").unwrap();
			let track = broadcast.create_track("catalog", timed()).unwrap();
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
					let remote = consumer.follow("bench", None).unwrap().available().await.unwrap();
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
			kio::clock::sleep(moq_net::cache::DEFAULT_EXPIRY * 2).await;
			drop((group, sub, leaver, leaver_session));

			// Right after the leave, and again once the leaver's front has lingered and let go
			// of the track (`IDLE_LINGER` is as long as the cache window).
			for (hop, after) in [
				(5, Duration::from_millis(100)),
				(6, moq_net::cache::DEFAULT_EXPIRY + Duration::from_secs(1)),
			] {
				kio::clock::sleep(after).await;
				let (later, _later) = session(hop).await;
				let mut sub = later.track("catalog").unwrap().subscribe(None).await.unwrap();
				let group = kio::clock::timeout(Duration::from_secs(1), sub.recv_group()).await;
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

/// A rejoin keeps the source's timestamps.
///
/// The returning subscription opts out of the track's properties from draft-22, since the
/// copy already learned them, so its units must come from that first answer rather than
/// the empty block the second one carries.
#[kio::test]
async fn rejoin_keeps_source_timestamps() {
	for version in ["moq-transport-18", "moq-transport-22"] {
		kio::clock::timeout(TEST_TIMEOUT, async {
			let publisher = produce_origin(1);
			let client = produce_origin(2);

			let broadcast = publisher.create_broadcast("bench").unwrap();
			let track = broadcast.create_track("video", timed()).unwrap();
			broadcast.announce(Default::default()).unwrap();

			let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(client.clone());
			let _pair = connect_mock(options).await;

			let consumer = client.consume();
			let remote = consumer.follow("bench", None).unwrap().available().await.unwrap();
			// Far from any arrival time, so a frame stamped on arrival cannot pass.
			let ts = |ms| Timestamp::from_millis(ms).unwrap();

			for (sequence, ms) in [(0, 123_456), (1, 654_321)] {
				let mut sub = remote.track("video").unwrap().subscribe(None).await.unwrap();
				crate::support::wanted(track.demand()).await.unwrap();
				let mut group = track.append_group().unwrap();
				group.write_frame(ts(ms), b"frame".as_ref()).unwrap();
				group.finish().unwrap();

				let mut group = loop {
					let group = sub.recv_group().await.unwrap().unwrap();
					if group.sequence == sequence {
						break group;
					}
				};
				let frame = group.read_frame().await.unwrap().unwrap();
				assert_eq!(
					frame.timestamp.map(Timestamp::as_millis),
					Some(u128::from(ms)),
					"{version} group {sequence}"
				);
				drop((group, sub));
				crate::support::unwanted(track.demand()).await.unwrap();
			}
		})
		.await
		.unwrap_or_else(|_| panic!("{version}: timed out"));
	}
}

/// A quiet track's latest group outlives the cache window while it stays the live edge.
///
/// A catalog is published once. A reader reads it and leaves, so the relay parks the
/// track, and the next reader re-splices the upstream copy after the cached group. The
/// group must stay protected while that reader holds the track open: a reader joining
/// after the cache window needs it, and no newer group may ever come. Once a newer group
/// supersedes it, it expires like any other.
#[kio::test]
async fn rejoined_quiet_catalog_survives_cache_expiry() {
	for version in [
		"moq-lite-05",
		"moq-lite-06",
		"moq-lite-07-wip",
		"moq-transport-18",
		"moq-transport-22",
	] {
		quiet_catalog_survives_cache_expiry(version.parse().unwrap(), false).await;
	}
}

/// A quiet track's latest group outlives the cache window across a publisher takeover.
///
/// The publisher reconnects while a reader holds the catalog. A late reader through the
/// new connection still needs the quiet catalog, even on a wire without epochs where
/// the old reader ends with its route.
#[kio::test]
async fn taken_over_quiet_catalog_survives_cache_expiry() {
	for version in [
		"moq-lite-05",
		"moq-lite-06",
		"moq-lite-07-wip",
		"moq-transport-18",
		"moq-transport-22",
	] {
		quiet_catalog_survives_cache_expiry(version.parse().unwrap(), true).await;
	}
}

async fn quiet_catalog_survives_cache_expiry(version: Version, takeover: bool) {
	kio::clock::timeout(TEST_TIMEOUT + moq_net::cache::DEFAULT_EXPIRY * 5, async {
		let publisher = produce_origin(1);
		let relay = produce_origin(2);
		let broadcast = publisher.create_broadcast("bench").unwrap();
		let track = broadcast.create_track("catalog", timed()).unwrap();
		broadcast
			.announce(moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint()))
			.unwrap();
		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, b"catalog".as_ref()).unwrap();
		group.finish().unwrap();

		let upstream = |cost| {
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(relay.clone());
			options.cost = Some(cost);
			connect_mock(options)
		};
		let mut upstream_session = upstream(2).await;

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
				let remote = consumer.follow("bench", None).unwrap().available().await.unwrap();
				let sub = remote.track("catalog").unwrap().subscribe(None).await.unwrap();
				(sub, remote, pair)
			}
		};

		let (_holder, _holder_remote, _holder_pair) = if takeover {
			// The publisher reconnects while the holder reads, and the old connection goes:
			// the relay takes the track over from the new one.
			let (mut holder, remote, pair) = session(3).await;
			assert_eq!(holder.recv_group().await.unwrap().unwrap().sequence, 0, "{version}");
			// The new connection is cheaper, so the relay reports its route taking over.
			let mut follow = relay.consume().announced();
			assert!(
				matches!(follow.next().await, Some(announce::Event::Start(_))),
				"{version}"
			);
			let replacement = upstream(1).await;
			let event = follow.next().await;
			assert!(
				matches!(
					event,
					Some(announce::Event::Update(_) | announce::Event::Restart(_) | announce::Event::Takeover { .. })
				),
				"{version}: {event:?}"
			);
			upstream_session = replacement;
			(holder, remote, pair)
		} else {
			// A reader leaves, so the relay parks the track; the holder re-splices it.
			let (mut first, remote, pair) = session(3).await;
			assert_eq!(first.recv_group().await.unwrap().unwrap().sequence, 0, "{version}");
			drop((first, remote, pair));
			crate::support::unwanted(track.demand()).await.unwrap();
			let (mut holder, remote, pair) = session(4).await;
			assert_eq!(holder.recv_group().await.unwrap().unwrap().sequence, 0, "{version}");
			(holder, remote, pair)
		};

		kio::clock::sleep(moq_net::cache::DEFAULT_EXPIRY * 2 + Duration::from_secs(2)).await;
		let (mut later, later_remote, _later_pair) = session(5).await;
		let mut group = kio::clock::timeout(Duration::from_secs(1), later.recv_group())
			.await
			.unwrap_or_else(|_| panic!("{version} takeover={takeover}: late reader lost the latest catalog"))
			.unwrap()
			.unwrap();
		assert_eq!(read_all(&mut group).await.unwrap(), [b"catalog"], "{version}");
		drop(group);

		// A newer catalog supersedes it, so the old one expires like any other group.
		let mut newer = track.append_group().unwrap();
		newer
			.write_frame(Timestamp::from_millis(1000).unwrap(), b"newer".as_ref())
			.unwrap();
		newer.finish().unwrap();
		// Epochless wires end the old holder on takeover. The late reader uses the
		// current route on every version, and keeps that copy live for the expiry check.
		assert_eq!(later.recv_group().await.unwrap().unwrap().sequence, 1, "{version}");
		kio::clock::sleep(moq_net::cache::DEFAULT_EXPIRY * 2 + Duration::from_secs(2)).await;
		let old = async {
			let mut group = later_remote.track("catalog").unwrap().fetch_group(0, None).await?;
			group.read_frame().await
		};
		assert!(
			kio::clock::timeout(Duration::from_secs(1), old).await.unwrap().is_err(),
			"{version} takeover={takeover}: the superseded catalog never expired"
		);
		drop(upstream_session);
	})
	.await
	.unwrap_or_else(|_| panic!("{version} takeover={takeover}: timed out"));
}
