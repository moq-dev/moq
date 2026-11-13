#![cfg(feature = "async")]

//! A peer granting few streams still gets everything asked of it, on each protocol: opens
//! wait their turn for credit rather than failing.

use std::time::Duration;

use moq_net::{Version, origin, track};

use crate::support::AppendNext;
use crate::support::AtStart;
use crate::support::harness::{MockConnectOptions, connect_mock, spawn};

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	spawn(driver);
	origin.with_proxy(true)
}

#[kio::test]
async fn one_stream_credit_serves_every_backlogged_group() {
	for version in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		let version: Version = version.parse().unwrap();
		let publisher = origin(1);
		let viewer = origin(2);
		let broadcast = publisher
			.route("live", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let info = track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_max_age(None);
		let tracks: Vec<_> = (0..2)
			.map(|i| {
				broadcast
					.broadcast()
					.create_track(format!("{i}"), info.clone())
					.unwrap()
			})
			.collect();
		broadcast.announce().unwrap();

		let mut options = MockConnectOptions::new(version);
		// Draft 22 keeps its SETUP stream open, so it needs one more for groups.
		options.transport.max_concurrent_uni_streams = Some(1 + usize::from(!version.is_lite()));
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(viewer.clone());
		let _pair = connect_mock(options).await;

		let remote = viewer
			.consume()
			.follow("live", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
		let prefs =
			track::Subscription::default().with_delay(track::Delay::default().with_max(Duration::from_secs(30)));
		let mut subscribers = Vec::new();
		for i in 0..tracks.len() {
			let track = remote.track(&format!("{i}")).unwrap();
			subscribers.push(track.subscribe(prefs.clone()).await.unwrap());
		}

		for _ in 0..4 {
			for track in &tracks {
				let mut group = track.append_next().unwrap();
				group.write_frame(group.at_start(), b"x".as_slice()).unwrap();
				group.close().unwrap();
			}
		}
		for subscriber in &mut subscribers {
			// Both tracks rank alike, so the order across them is the line's arrival order.
			let mut received = Vec::new();
			while received.len() < 4 {
				let group = kio::clock::timeout(Duration::from_secs(5), subscriber.recv_group())
					.await
					.unwrap_or_else(|_| panic!("{version}: only {received:?} arrived"))
					.unwrap()
					.unwrap();
				received.push(group.start);
			}
			received.sort();
			assert_eq!(received, [0, 1, 2, 3], "{version}");
		}
	}
}

/// Out of credit, a newer group resets an older one still being written rather than waiting
/// for it to finish, on each protocol.
#[kio::test]
async fn a_newer_group_resets_an_unfinished_older_one() {
	for version in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		let version: Version = version.parse().unwrap();
		let publisher = origin(1);
		let viewer = origin(2);
		let broadcast = publisher
			.route("live", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let info = track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_max_age(None);
		let track = broadcast.broadcast().create_track("0", info).unwrap();
		broadcast.announce().unwrap();

		let mut options = MockConnectOptions::new(version);
		// Draft 22 keeps its SETUP stream open, so it needs one more for groups.
		options.transport.max_concurrent_uni_streams = Some(1 + usize::from(!version.is_lite()));
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(viewer.clone());
		let _pair = connect_mock(options).await;

		let remote = viewer
			.consume()
			.follow("live", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
		let prefs = track::Subscription::default()
			.with_order(track::Order::Descending)
			.with_delay(track::Delay::default().with_max(Duration::from_secs(30)));
		let mut subscriber = remote.track("0").unwrap().subscribe(prefs).await.unwrap();

		// Never finished, so its stream holds the only credit.
		let mut older = track.append_next().unwrap();
		older.write_frame(older.at_start(), b"old".as_slice()).unwrap();
		let first = kio::clock::timeout(Duration::from_secs(5), subscriber.recv_group())
			.await
			.unwrap_or_else(|_| panic!("{version}: the older group arrives"))
			.unwrap()
			.unwrap();
		assert_eq!(first.start, 0, "{version}");

		let mut newer = track.append_next().unwrap();
		newer.write_frame(newer.at_start(), b"new".as_slice()).unwrap();
		newer.close().unwrap();
		let group = kio::clock::timeout(Duration::from_secs(5), subscriber.recv_group())
			.await
			.unwrap_or_else(|_| panic!("{version}: the newer group waits behind the older one"))
			.unwrap()
			.unwrap();
		assert_eq!(group.start, 1, "{version}");
		drop(older);
	}
}

/// On lite-07 a group whose stream a newer one reset to make room only pauses: once credit
/// returns, a new stream continues it, and its reader receives every frame without an error.
#[kio::test]
async fn a_reset_group_resumes_without_loss() {
	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let publisher = origin(1);
	let viewer = origin(2);
	let broadcast = publisher
		.route("live", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let info = track::Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_max_age(None);
	let track = broadcast.broadcast().create_track("0", info).unwrap();
	broadcast.announce().unwrap();

	let mut options = MockConnectOptions::new(version);
	options.transport.max_concurrent_uni_streams = Some(1);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(viewer.clone());
	let _pair = connect_mock(options).await;

	let remote = viewer
		.consume()
		.follow("live", None)
		.unwrap()
		.available()
		.await
		.unwrap()
		.unwrap();
	let prefs = track::Subscription::default()
		.with_order(track::Order::Descending)
		.with_delay(track::Delay::default().with_max(Duration::from_secs(30)));
	let mut subscriber = remote.track("0").unwrap().subscribe(prefs).await.unwrap();

	// Unfinished, so its stream holds the only credit until the newer group resets it.
	let mut older = track.append_next().unwrap();
	older.write_frame(older.at_start(), b"a".as_slice()).unwrap();
	let mut paused = kio::clock::timeout(Duration::from_secs(5), subscriber.recv_group())
		.await
		.expect("the older group arrives")
		.unwrap()
		.unwrap();
	assert_eq!(paused.start, 0);
	let first = kio::clock::timeout(Duration::from_secs(5), paused.read_frame())
		.await
		.expect("its first frame arrives")
		.unwrap();
	assert_eq!(first.map(|frame| frame.payload), Some(bytes::Bytes::from_static(b"a")));

	let mut newer = track.append_next().unwrap();
	newer.write_frame(newer.at_start(), b"new".as_slice()).unwrap();
	newer.close().unwrap();
	let group = kio::clock::timeout(Duration::from_secs(5), subscriber.recv_group())
		.await
		.expect("the newer group takes the older one's credit")
		.unwrap()
		.unwrap();
	assert_eq!(group.start, 1);

	// Written after the reset: a continuation carries them, behind the frame already held.
	older.write_frame(older.at_start(), b"b".as_slice()).unwrap();
	older.write_frame(older.at_start(), b"c".as_slice()).unwrap();
	older.close().unwrap();
	let mut rest = Vec::new();
	while let Some(frame) = kio::clock::timeout(Duration::from_secs(5), paused.read_frame())
		.await
		.expect("the paused group continues")
		.expect("the paused group is not given up")
	{
		rest.push(frame.payload);
	}
	assert_eq!(rest, [b"b".as_slice(), b"c".as_slice()]);
}

/// On lite-07 a group paused while it waited for its next frame, which then finishes with no
/// more frames, still only pauses: the continuation ends it cleanly rather than a drop giving
/// it up.
#[kio::test]
async fn a_reset_group_finishing_without_more_frames_ends_cleanly() {
	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let publisher = origin(1);
	let viewer = origin(2);
	let broadcast = publisher
		.route("live", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let info = track::Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_max_age(None);
	let track = broadcast.broadcast().create_track("0", info).unwrap();
	broadcast.announce().unwrap();

	let mut options = MockConnectOptions::new(version);
	options.transport.max_concurrent_uni_streams = Some(1);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(viewer.clone());
	let _pair = connect_mock(options).await;

	let remote = viewer
		.consume()
		.follow("live", None)
		.unwrap()
		.available()
		.await
		.unwrap()
		.unwrap();
	let prefs = track::Subscription::default()
		.with_order(track::Order::Descending)
		.with_delay(track::Delay::default().with_max(Duration::from_secs(30)));
	let mut subscriber = remote.track("0").unwrap().subscribe(prefs).await.unwrap();

	let mut older = track.append_next().unwrap();
	older.write_frame(older.at_start(), b"a".as_slice()).unwrap();
	let mut paused = kio::clock::timeout(Duration::from_secs(5), subscriber.recv_group())
		.await
		.expect("the older group arrives")
		.unwrap()
		.unwrap();
	let first = kio::clock::timeout(Duration::from_secs(5), paused.read_frame())
		.await
		.expect("its first frame arrives")
		.unwrap();
	assert_eq!(first.map(|frame| frame.payload), Some(bytes::Bytes::from_static(b"a")));

	let mut newer = track.append_next().unwrap();
	newer.write_frame(newer.at_start(), b"new".as_slice()).unwrap();
	newer.close().unwrap();
	let group = kio::clock::timeout(Duration::from_secs(5), subscriber.recv_group())
		.await
		.expect("the newer group takes the older one's credit")
		.unwrap()
		.unwrap();
	assert_eq!(group.start, 1);

	// No frame follows the reset: the group just ends.
	older.close().unwrap();
	let end = kio::clock::timeout(Duration::from_secs(5), paused.read_frame())
		.await
		.expect("the paused group continues");
	assert!(matches!(end, Ok(None)), "the paused group ends cleanly: {end:?}");
}

/// On lite-07 a paused group's continuation waiting for credit ranks below the stream it
/// lost to, so it never holds back a newer group that outranks that stream: the newer group
/// still resets it, and both paused groups then continue without loss.
#[kio::test]
async fn a_waiting_continuation_lets_a_newer_group_through() {
	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let publisher = origin(1);
	let viewer = origin(2);
	let broadcast = publisher
		.route("live", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let info = track::Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_max_age(None);
	let track = broadcast.broadcast().create_track("0", info).unwrap();
	broadcast.announce().unwrap();

	let mut options = MockConnectOptions::new(version);
	options.transport.max_concurrent_uni_streams = Some(1);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(viewer.clone());
	let _pair = connect_mock(options).await;

	let remote = viewer
		.consume()
		.follow("live", None)
		.unwrap()
		.available()
		.await
		.unwrap()
		.unwrap();
	let prefs = track::Subscription::default()
		.with_order(track::Order::Descending)
		.with_delay(track::Delay::default().with_max(Duration::from_secs(30)));
	let mut subscriber = remote.track("0").unwrap().subscribe(prefs).await.unwrap();
	macro_rules! next {
		() => {
			kio::clock::timeout(Duration::from_secs(5), subscriber.recv_group())
				.await
				.expect("a group arrives")
				.unwrap()
				.unwrap()
		};
	}

	// Groups 0 and 1 stay unfinished: 1 resets 0, whose continuation then waits for credit.
	let mut groups = Vec::new();
	let mut readers = Vec::new();
	for key in 0..2 {
		let mut group = track.append_next().unwrap();
		group.write_frame(group.at_start(), b"a".as_slice()).unwrap();
		let reader = next!();
		assert_eq!(reader.start, key);
		groups.push(group);
		readers.push(reader);
	}

	// The newest outranks group 1's stream, so it resets that one too.
	let mut newest = track.append_next().unwrap();
	newest.write_frame(newest.at_start(), b"new".as_slice()).unwrap();
	newest.close().unwrap();
	assert_eq!(next!().start, 2, "the newest group waits behind a continuation");

	// The paused groups continue highest-ranked first: newest first in descending order.
	for (mut group, mut reader) in groups.into_iter().zip(readers).rev() {
		group.write_frame(group.at_start(), b"b".as_slice()).unwrap();
		group.close().unwrap();
		let mut frames = Vec::new();
		while let Some(frame) = kio::clock::timeout(Duration::from_secs(5), reader.read_frame())
			.await
			.expect("the paused group continues")
			.expect("the paused group is not given up")
		{
			frames.push(frame.payload);
		}
		assert_eq!(frames, [b"a".as_slice(), b"b".as_slice()], "group {}", reader.start);
	}
}

/// A relay's upstream session out of request credit waits for it rather than refusing: a
/// subscription past the publisher's credit is served once an earlier one leaves.
#[kio::test]
async fn a_subscription_past_the_credit_is_served_once_credit_returns() {
	// lite-07 needs 4: session-long AUTH and ANNOUNCE plus TRACK held until SUBSCRIBE answers.
	// moq-transport needs 2: SUBSCRIBE_NAMESPACE and the first SUBSCRIBE. The second
	// subscription waits for the first's credit.
	for (version, credit) in [("moq-lite-07-wip", 4), ("moq-transport-22", 2)] {
		let version: Version = version.parse().unwrap();
		let publisher = origin(1);
		let viewer = origin(2);
		let broadcast = publisher
			.route("live", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let info = track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_max_age(None);
		let first = broadcast.broadcast().create_track("first", info.clone()).unwrap();
		let second = broadcast.broadcast().create_track("second", info).unwrap();
		broadcast.announce().unwrap();

		let mut options = MockConnectOptions::new(version);
		options.transport.max_concurrent_bi_streams = Some(credit);
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(viewer.clone());
		let _pair = connect_mock(options).await;

		let remote = viewer
			.consume()
			.follow("live", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
		let prefs =
			track::Subscription::default().with_delay(track::Delay::default().with_max(Duration::from_secs(30)));
		let held = remote.track("first").unwrap().subscribe(prefs.clone()).await.unwrap();
		let mut waiting = std::pin::pin!(remote.track("second").unwrap().subscribe(prefs));
		assert!(
			kio::clock::timeout(Duration::from_secs(5), waiting.as_mut())
				.await
				.is_err(),
			"{version}: the second subscription had no credit to wait for"
		);

		drop(held);
		drop(first);
		let mut subscriber = kio::clock::timeout(Duration::from_secs(60), waiting)
			.await
			.unwrap_or_else(|_| panic!("{version}: the waiting subscription never got credit"))
			.unwrap_or_else(|err| panic!("{version}: the waiting subscription failed: {err}"));
		let mut group = second.append_next().unwrap();
		group.write_frame(group.at_start(), b"x".as_slice()).unwrap();
		group.close().unwrap();
		let group = kio::clock::timeout(Duration::from_secs(5), subscriber.recv_group())
			.await
			.unwrap_or_else(|_| panic!("{version}: no group after credit returned"))
			.unwrap()
			.unwrap();
		assert_eq!(group.start, 0, "{version}");
	}
}

/// Abandoning setup returns credit without ending earlier publishers or their route.
#[kio::test]
async fn an_abandoned_setup_returns_credit_with_the_publisher_still_live() {
	// lite-07 needs 4: session-long AUTH and ANNOUNCE plus TRACK held until SUBSCRIBE answers.
	for (version, credit) in [("moq-lite-07-wip", 4)] {
		let version: Version = version.parse().unwrap();
		let publisher = origin(1);
		let viewer = origin(2);
		let broadcast = publisher
			.route("live", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let info = track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_max_age(None);
		let first = broadcast.broadcast().create_track("first", info.clone()).unwrap();
		let second = broadcast.broadcast().create_track("second", info.clone()).unwrap();
		let third = broadcast.broadcast().create_track("third", info).unwrap();
		broadcast.announce().unwrap();

		let mut options = MockConnectOptions::new(version);
		options.transport.max_concurrent_bi_streams = Some(credit);
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(viewer.clone());
		let _pair = connect_mock(options).await;

		let remote = viewer
			.consume()
			.follow("live", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
		let prefs =
			track::Subscription::default().with_delay(track::Delay::default().with_max(Duration::from_secs(30)));
		let held = remote.track("first").unwrap().subscribe(prefs.clone()).await.unwrap();
		let mut waiting = Box::pin(remote.track("second").unwrap().subscribe(prefs.clone()));
		assert!(
			kio::clock::timeout(Duration::from_secs(5), waiting.as_mut())
				.await
				.is_err(),
			"{version}: the second subscription had no credit to wait for"
		);

		drop(waiting);
		drop(held);
		let waiting = remote.track("third").unwrap().subscribe(prefs);
		let mut subscriber = kio::clock::timeout(Duration::from_secs(60), waiting)
			.await
			.unwrap_or_else(|_| panic!("{version}: the waiting subscription never got credit"))
			.unwrap_or_else(|err| panic!("{version}: the waiting subscription failed: {err}"));
		let mut group = third.append_next().unwrap();
		group.write_frame(group.at_start(), b"x".as_slice()).unwrap();
		group.close().unwrap();
		let group = kio::clock::timeout(Duration::from_secs(5), subscriber.recv_group())
			.await
			.unwrap_or_else(|_| panic!("{version}: no group after credit returned"))
			.unwrap()
			.unwrap();
		assert_eq!(group.start, 0, "{version}");
		drop(group);
		drop(subscriber);
		let mut resumed = remote.track("second").unwrap().subscribe(None).await.unwrap();
		let mut group = second.append_next().unwrap();
		group.write_frame(group.at_start(), b"still live".as_slice()).unwrap();
		group.close().unwrap();
		let group = kio::clock::timeout(Duration::from_secs(5), resumed.recv_group())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(group.start, 0);
		// Keep the first publisher alive through the resumed wire request too.
		drop(first);
	}
}

/// Out of credit, a late group that ranks below the stream holding it waits for credit
/// without holding back a newer group that outranks that stream: the newer group still
/// resets it, on each protocol.
#[kio::test]
async fn a_waiting_late_group_lets_a_newer_group_through() {
	for version in ["moq-lite-06", "moq-lite-07-wip"] {
		let version: Version = version.parse().unwrap();
		let publisher = origin(1);
		let viewer = origin(2);
		let broadcast = publisher
			.route("live", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let info = track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_max_age(None);
		let track = broadcast.broadcast().create_track("0", info).unwrap();
		broadcast.announce().unwrap();

		let mut options = MockConnectOptions::new(version);
		options.transport.max_concurrent_uni_streams = Some(1);
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(viewer.clone());
		let _pair = connect_mock(options).await;

		let remote = viewer
			.consume()
			.follow("live", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
		let prefs = track::Subscription::default()
			.with_order(track::Order::Descending)
			.with_delay(track::Delay::default().with_max(Duration::from_secs(30)));
		let mut subscriber = remote.track("0").unwrap().subscribe(prefs).await.unwrap();
		macro_rules! next {
			($what:literal) => {
				kio::clock::timeout(Duration::from_secs(5), subscriber.recv_group())
					.await
					.unwrap_or_else(|_| panic!("{version}: {}", $what))
					.unwrap()
					.unwrap()
			};
		}

		// Unfinished, so its stream holds the only credit.
		let mut held = track.create_group(moq_net::group::Info { start: 2 }).unwrap();
		held.write_frame(held.at_start(), b"held".as_slice()).unwrap();
		assert_eq!(next!("the held group arrives").start, 2, "{version}");

		// Late and below the held stream: it waits for credit it cannot make room for.
		let mut late = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		late.write_frame(late.at_start(), b"late".as_slice()).unwrap();
		late.close().unwrap();

		let mut newer = track.create_group(moq_net::group::Info { start: 3 }).unwrap();
		newer.write_frame(newer.at_start(), b"new".as_slice()).unwrap();
		newer.close().unwrap();
		assert_eq!(next!("the newer group waits behind the late one").start, 3, "{version}");
		drop(held);
	}
}

/// Newest first sheds the backlog: under a late group waiting for credit, a later late group
/// ranked below it is given up rather than holding back a newer group behind it, on each
/// protocol.
#[kio::test]
async fn a_shed_late_group_lets_a_newer_group_through() {
	for version in ["moq-lite-06", "moq-lite-07-wip"] {
		let version: Version = version.parse().unwrap();
		let publisher = origin(1);
		let viewer = origin(2);
		let broadcast = publisher
			.route("live", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let info = track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_max_age(None);
		let track = broadcast.broadcast().create_track("0", info).unwrap();
		broadcast.announce().unwrap();

		let mut options = MockConnectOptions::new(version);
		options.transport.max_concurrent_uni_streams = Some(1);
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(viewer.clone());
		let _pair = connect_mock(options).await;

		let remote = viewer
			.consume()
			.follow("live", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
		let prefs = track::Subscription::default()
			.with_order(track::Order::Descending)
			.with_delay(track::Delay::default().with_max(Duration::from_secs(30)));
		let mut subscriber = remote.track("0").unwrap().subscribe(prefs).await.unwrap();
		macro_rules! next {
			($what:literal) => {
				kio::clock::timeout(Duration::from_secs(5), subscriber.recv_group())
					.await
					.unwrap_or_else(|_| panic!("{version}: {}", $what))
					.unwrap()
					.unwrap()
			};
		}

		// Unfinished, so its stream holds the only credit.
		let mut held = track.create_group(moq_net::group::Info { start: 3 }).unwrap();
		held.write_frame(held.at_start(), b"held".as_slice()).unwrap();
		assert_eq!(next!("the held group arrives").start, 3, "{version}");

		// Group 1 waits for credit it cannot make room for, and group 0 ranks below it.
		for key in [1, 0] {
			let mut late = track.create_group(moq_net::group::Info { start: key }).unwrap();
			late.write_frame(late.at_start(), b"late".as_slice()).unwrap();
			late.close().unwrap();
		}

		let mut newer = track.create_group(moq_net::group::Info { start: 4 }).unwrap();
		newer.write_frame(newer.at_start(), b"new".as_slice()).unwrap();
		newer.close().unwrap();
		assert_eq!(
			next!("the newer group waits behind the late ones").start,
			4,
			"{version}"
		);
		drop(held);
	}
}

/// Oldest first, a backlog behind a group waiting for credit stays in the cache rather than
/// giving it up: a newer group ranks below the waiting one, so every group still arrives once
/// the stream holding the credit finishes, on each protocol.
#[kio::test]
async fn an_ascending_backlog_waits_behind_a_waiting_group() {
	for version in ["moq-lite-06", "moq-lite-07-wip"] {
		let version: Version = version.parse().unwrap();
		let publisher = origin(1);
		let viewer = origin(2);
		let broadcast = publisher
			.route("live", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let info = track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_max_age(None);
		let track = broadcast.broadcast().create_track("0", info).unwrap();
		broadcast.announce().unwrap();

		let mut options = MockConnectOptions::new(version);
		options.transport.max_concurrent_uni_streams = Some(1);
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(viewer.clone());
		let _pair = connect_mock(options).await;

		let remote = viewer
			.consume()
			.follow("live", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
		let prefs = track::Subscription::default()
			.with_order(track::Order::Ascending)
			.with_delay(track::Delay::default().with_max(Duration::from_secs(30)));
		let mut subscriber = remote.track("0").unwrap().subscribe(prefs).await.unwrap();
		macro_rules! next {
			($what:literal) => {
				kio::clock::timeout(Duration::from_secs(5), subscriber.recv_group())
					.await
					.unwrap_or_else(|_| panic!("{version}: {}", $what))
					.unwrap()
					.unwrap()
			};
		}

		// Unfinished, so its stream holds the only credit, and outranks every newer group.
		let mut held = track.append_next().unwrap();
		held.write_frame(held.at_start(), b"held".as_slice()).unwrap();
		assert_eq!(next!("the held group arrives").start, 0, "{version}");

		for _ in 0..3 {
			let mut group = track.append_next().unwrap();
			group.write_frame(group.at_start(), b"x".as_slice()).unwrap();
			group.close().unwrap();
		}
		// Let the publisher take what it would before credit returns.
		kio::clock::sleep(Duration::from_millis(100)).await;
		held.close().unwrap();

		let mut received = Vec::new();
		while received.len() < 3 {
			received.push(next!("the backlog arrives").start);
		}
		assert_eq!(received, [1, 2, 3], "{version}");
	}
}

/// On lite-07 a paused group waiting behind another paused group's continuation is still
/// given up once its publisher aborts it, rather than held until credit returns: the paused
/// groups are bounded by what the cache still holds, however long congestion lasts.
#[kio::test]
async fn a_paused_group_given_up_while_it_waits_is_dropped() {
	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let publisher = origin(1);
	let viewer = origin(2);
	let broadcast = publisher
		.route("live", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let info = track::Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_max_age(None);
	let track = broadcast.broadcast().create_track("0", info).unwrap();
	broadcast.announce().unwrap();

	let mut options = MockConnectOptions::new(version);
	options.transport.max_concurrent_uni_streams = Some(1);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(viewer.clone());
	let _pair = connect_mock(options).await;

	let remote = viewer
		.consume()
		.follow("live", None)
		.unwrap()
		.available()
		.await
		.unwrap()
		.unwrap();
	let prefs = track::Subscription::default()
		.with_order(track::Order::Descending)
		.with_delay(track::Delay::default().with_max(Duration::from_secs(30)));
	let mut subscriber = remote.track("0").unwrap().subscribe(prefs).await.unwrap();

	// Each unfinished group resets the one before: the newest holds the only credit, group
	// 1's continuation waits for it, and group 0 waits among the paused behind that.
	let mut groups = Vec::new();
	let mut readers = Vec::new();
	for start in 0..3 {
		let mut group = track.append_next().unwrap();
		group.write_frame(group.at_start(), b"a".as_slice()).unwrap();
		let mut reader = kio::clock::timeout(Duration::from_secs(5), subscriber.recv_group())
			.await
			.expect("a group arrives")
			.unwrap()
			.unwrap();
		assert_eq!(reader.start, start);
		let first = kio::clock::timeout(Duration::from_secs(5), reader.read_frame())
			.await
			.expect("its first frame arrives")
			.unwrap();
		assert_eq!(first.map(|frame| frame.payload), Some(bytes::Bytes::from_static(b"a")));
		groups.push(group);
		readers.push(reader);
	}

	groups.remove(0).abort(moq_net::Error::Cancel).unwrap();
	let end = kio::clock::timeout(Duration::from_secs(5), readers[0].read_frame())
		.await
		.expect("the paused group is dropped while the others hold the credit");
	assert!(end.is_err(), "the aborted paused group fails: {end:?}");
}

/// A finished stream still waiting for its acknowledgement holds credit that no group can
/// take over, so it doesn't count when ranking a waiting group: a late group ranked above it
/// but below the stream still serving waits, and a newer group resets that stream rather than
/// waiting behind the late one, on each protocol.
#[kio::test]
async fn a_closing_stream_does_not_rank_a_waiting_group() {
	const LATENCY: Duration = Duration::from_millis(100);
	const SETTLE: Duration = Duration::from_millis(10);

	for version in ["moq-lite-06", "moq-lite-07-wip"] {
		let version: Version = version.parse().unwrap();
		let publisher = origin(1);
		let viewer = origin(2);
		let broadcast = publisher
			.route("live", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let info = track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_max_age(None);
		let track = broadcast.broadcast().create_track("0", info).unwrap();
		broadcast.announce().unwrap();

		let mut options = MockConnectOptions::new(version);
		options.transport.max_concurrent_uni_streams = Some(2);
		options.latency = LATENCY;
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(viewer.clone());
		let _pair = connect_mock(options).await;

		let remote = viewer
			.consume()
			.follow("live", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
		let prefs = track::Subscription::default()
			.with_order(track::Order::Descending)
			.with_delay(track::Delay::default().with_max(Duration::from_secs(30)));
		let mut subscriber = remote.track("0").unwrap().subscribe(prefs).await.unwrap();

		// Group 7 finishes, so its stream waits for the acknowledgement; group 10 holds the other.
		let mut closing = track.create_group(moq_net::group::Info { start: 7 }).unwrap();
		closing.write_frame(closing.at_start(), b"closing".as_slice()).unwrap();
		closing.close().unwrap();
		let mut held = track.create_group(moq_net::group::Info { start: 10 }).unwrap();
		held.write_frame(held.at_start(), b"held".as_slice()).unwrap();
		kio::clock::sleep(SETTLE).await;

		// Ranked above group 7 but below group 10: it can only wait for group 7's credit.
		let mut late = track.create_group(moq_net::group::Info { start: 8 }).unwrap();
		late.write_frame(late.at_start(), b"late".as_slice()).unwrap();
		late.close().unwrap();
		kio::clock::sleep(SETTLE).await;

		let mut newer = track.create_group(moq_net::group::Info { start: 11 }).unwrap();
		newer.write_frame(newer.at_start(), b"new".as_slice()).unwrap();
		newer.close().unwrap();

		loop {
			let group = kio::clock::timeout(Duration::from_secs(5), subscriber.recv_group())
				.await
				.unwrap_or_else(|_| panic!("{version}: the newer group never arrives"))
				.unwrap()
				.unwrap();
			assert_ne!(group.start, 8, "{version}: the newer group waits behind the late one");
			if group.start == 11 {
				break;
			}
		}
		drop(held);
	}
}
