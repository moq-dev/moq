#![cfg(feature = "async")]

//! A finished track whose only group starts well past 0, read through a relay by one reader
//! after another: each reader gets the group, however its subscription names the start and
//! whenever it joins as the reader before it lets go.

use crate::WithMax as _;
use crate::support;

use std::time::Duration;

use moq_net::Version;
use moq_net::track::Subscription;
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Where the publisher's only group starts, in microseconds.
const START: u64 = 407;
const FRAMES: u64 = 10;

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) =
		moq_net::origin::Producer::new(moq_net::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer.with_proxy(true)
}

/// Read every frame `subscription` delivers from the relay over a fresh `version` session.
async fn read(
	relay: &moq_net::origin::Producer,
	version: Version,
	subscription: Subscription,
) -> (Vec<u64>, Result<(), moq_net::Error>) {
	let subscriber = produce_origin(3);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(subscriber.clone());
	let session = connect_mock(options).await;
	let remote = kio::clock::timeout(TIMEOUT, subscriber.consume().follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
		.unwrap();
	let mut track = remote
		.track("video")
		.unwrap()
		.subscribe(subscription)
		.await
		.expect("subscribe");
	let mut frames = Vec::new();
	let end = loop {
		match kio::clock::timeout(TIMEOUT, track.recv_group())
			.await
			.expect("group timeout")
		{
			// A frame that fails to read is missing from what the caller compares.
			Ok(Some(mut group)) => {
				while let Ok(Some(_)) = group.read_frame().await {
					frames.push(group.start);
				}
			}
			Ok(None) => break Ok(()),
			Err(err) => break Err(err),
		}
	};
	drop((track, session, subscriber));
	(frames, end)
}

#[kio::test]
async fn each_late_reader_reads_the_finished_group() {
	for (first, second) in [
		("moq-lite-07-wip", "moq-lite-07-wip"),
		("moq-lite-07-wip", "moq-lite-06"),
		("moq-lite-06", "moq-lite-07-wip"),
		("moq-lite-06", "moq-lite-06"),
	] {
		for gap in [0, 1, 5, 50] {
			let publisher = produce_origin(1);
			let broadcast = publisher
				.route("bcast", moq_net::origin::RouteOptions::hidden())
				.unwrap();
			let track = broadcast
				.broadcast()
				.create_track(
					"video",
					moq_net::track::Info::default()
						.with_timescale(moq_net::Timescale::MICRO)
						.with_max_age(None),
				)
				.unwrap();
			let mut group = track
				.append_group(moq_net::Timestamp::from_micros(START).unwrap())
				.unwrap();
			for frame in 0..FRAMES {
				group
					.write_frame(moq_net::Timestamp::from_micros(START + frame).unwrap(), &b"frame"[..])
					.unwrap();
			}
			group.finish().unwrap();
			let finished = track.finish().unwrap();
			broadcast.announce().unwrap();

			let relay = produce_origin(2);
			let mut options = MockConnectOptions::new("moq-lite-07-wip".parse().unwrap());
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(relay.clone());
			let upstream = connect_mock(options).await;

			let budget = Duration::from_secs(60);
			let oldest = Subscription::default().with_max(budget);
			let from_zero = Subscription::default()
				.with_ranges([support::keys(0..)])
				.with_max(budget);

			let (frames, end) = read(&relay, first.parse().unwrap(), oldest.clone()).await;
			assert_eq!(
				(frames.len() as u64, end.is_ok()),
				(FRAMES, true),
				"{first} first, gap {gap}"
			);
			kio::clock::sleep(Duration::from_millis(gap)).await;
			for (version, subscription) in [(second, from_zero.clone()), (second, oldest.clone())] {
				let (frames, end) = read(&relay, version.parse().unwrap(), subscription).await;
				assert_eq!(
					(frames, end.is_ok()),
					(vec![START; FRAMES as usize], true),
					"{first} then {version}, gap {gap}"
				);
			}
			drop((finished, broadcast, upstream, publisher, relay));
		}
	}
}

/// Readers arrive together and a few at a time, as a fan of late viewers does, each on its
/// own session: every one reads the group.
#[kio::test]
async fn concurrent_late_readers_read_the_finished_group() {
	for stagger in [0u64, 1, 2, 3, 5, 8] {
		let publisher = produce_origin(1);
		let broadcast = publisher
			.route("bcast", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let track = broadcast
			.broadcast()
			.create_track(
				"video",
				moq_net::track::Info::default()
					.with_timescale(moq_net::Timescale::MICRO)
					.with_max_age(None),
			)
			.unwrap();
		let mut group = track
			.append_group(moq_net::Timestamp::from_micros(START).unwrap())
			.unwrap();
		for frame in 0..FRAMES {
			group
				.write_frame(moq_net::Timestamp::from_micros(START + frame).unwrap(), &b"frame"[..])
				.unwrap();
		}
		group.finish().unwrap();
		let finished = track.finish().unwrap();
		broadcast.announce().unwrap();

		let relay = produce_origin(2);
		let mut options = MockConnectOptions::new("moq-lite-07-wip".parse().unwrap());
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(relay.clone());
		let upstream = connect_mock(options).await;

		let mut readers = Vec::new();
		for index in 0..9u64 {
			let relay = relay.clone();
			let version = ["moq-lite-07-wip", "moq-lite-06"][(index % 2) as usize];
			let budget = Duration::from_secs([5, 60, 1][(index % 3) as usize]);
			let subscription = match index % 3 {
				0 => Subscription::default()
					.with_ranges([support::keys(0..)])
					.with_max(budget),
				_ => Subscription::default().with_max(budget),
			};
			readers.push(kio::clock::sim::spawn(async move {
				kio::clock::sleep(Duration::from_millis(stagger * index)).await;
				(
					index,
					version,
					read(&relay, version.parse().unwrap(), subscription).await,
				)
			}));
		}
		for reader in readers {
			let (index, version, (frames, end)) = reader.await.expect("reader panicked");
			assert_eq!(
				(frames, end.is_ok()),
				(vec![START; FRAMES as usize], true),
				"reader {index} on {version}, stagger {stagger}"
			);
		}
		drop((finished, broadcast, upstream, publisher, relay));
	}
}

/// A reader asking only for the history before the group, which holds nothing, leaves the
/// group itself to the readers that join meanwhile.
#[kio::test]
async fn an_empty_history_read_leaves_the_group_to_readers_joining_meanwhile() {
	for (history_version, version) in [
		("moq-lite-07-wip", "moq-lite-07-wip"),
		("moq-lite-07-wip", "moq-lite-06"),
	] {
		for stagger in [0u64, 1, 2, 5] {
			let publisher = produce_origin(1);
			let broadcast = publisher
				.route("bcast", moq_net::origin::RouteOptions::hidden())
				.unwrap();
			let track = broadcast
				.broadcast()
				.create_track(
					"video",
					moq_net::track::Info::default()
						.with_timescale(moq_net::Timescale::MICRO)
						.with_max_age(None),
				)
				.unwrap();
			let mut group = track
				.append_group(moq_net::Timestamp::from_micros(START).unwrap())
				.unwrap();
			for frame in 0..FRAMES {
				group
					.write_frame(moq_net::Timestamp::from_micros(START + frame).unwrap(), &b"frame"[..])
					.unwrap();
			}
			group.finish().unwrap();
			let finished = track.finish().unwrap();
			broadcast.announce().unwrap();

			let relay = produce_origin(2);
			let mut options = MockConnectOptions::new("moq-lite-07-wip".parse().unwrap());
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(relay.clone());
			let upstream = connect_mock(options).await;

			let budget = Duration::from_secs(60);
			// A live reader first, as the history reader's own live subscription is.
			let (frames, _) = read(
				&relay,
				version.parse().unwrap(),
				Subscription::default().with_max(budget),
			)
			.await;
			assert_eq!(frames.len() as u64, FRAMES);

			let history = {
				let relay = relay.clone();
				kio::clock::sim::spawn(async move {
					let subscription = Subscription::default()
						.with_max(budget)
						.with_order(moq_net::track::Order::Ascending)
						.with_ranges([support::keys(0..START)]);
					read(&relay, history_version.parse().unwrap(), subscription).await
				})
			};
			kio::clock::sleep(Duration::from_millis(stagger)).await;
			for subscription in [
				Subscription::default().with_max(budget),
				Subscription::default()
					.with_ranges([support::keys(0..)])
					.with_max(budget),
			] {
				let (frames, end) = read(&relay, version.parse().unwrap(), subscription).await;
				assert_eq!(
					(frames, end.is_ok()),
					(vec![START; FRAMES as usize], true),
					"{version} beside a {history_version} history read, stagger {stagger}"
				);
			}
			let (frames, end) = history.await.expect("history reader panicked");
			assert_eq!((frames, end.is_ok()), (vec![], true), "the history holds nothing");
			drop((finished, broadcast, upstream, publisher, relay));
		}
	}
}

/// A reader joining at the latest group of a finished untimed track whose publisher keeps no
/// superseded group reads that group through a relay, on every version: the latest group is
/// never stale, even once its track ends.
#[kio::test]
async fn a_latest_reader_reads_a_finished_untimed_group() {
	for version in ["moq-lite-07-wip", "moq-lite-06", "moq-lite-05"] {
		let publisher = produce_origin(1);
		let broadcast = publisher
			.route("bcast", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let track = broadcast
			.broadcast()
			.create_track("video", moq_net::track::Info::default())
			.unwrap();
		let start = moq_net::Timestamp::unscaled(0);
		let mut group = track.append_group(start).unwrap();
		group.write_frame(start, &b"frame"[..]).unwrap();
		group.finish().unwrap();
		let finished = track.finish().unwrap();
		broadcast.announce().unwrap();

		let relay = produce_origin(2);
		let mut options = MockConnectOptions::new("moq-lite-07-wip".parse().unwrap());
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(relay.clone());
		let upstream = connect_mock(options).await;

		let (frames, end) = read(&relay, version.parse().unwrap(), Subscription::default()).await;
		assert_eq!((frames, end.is_ok()), (vec![0], true), "{version}");
		drop((finished, broadcast, upstream, publisher, relay));
	}
}

/// Readers asking a lite-06 relay for every group still within their max of a finished
/// track, as a reader with no start does there, get its last group whatever order the relay's
/// copy learns the group and the end in: untimed, as a health track, and timed, as media.
#[kio::test]
async fn late_readers_read_a_finished_track_in_any_order() {
	use crate::WithMax as _;
	for round in 0..32u64 {
		for (timed, warm) in [(false, false), (true, false), (false, true), (true, true)] {
			for version in ["moq-lite-07-wip", "moq-lite-06"] {
				let publisher = produce_origin(1);
				let broadcast = publisher
					.route("bcast", moq_net::origin::RouteOptions::hidden())
					.unwrap();
				let info = match timed {
					true => moq_net::track::Info::default()
						.with_timescale(moq_net::Timescale::MICRO)
						.with_max_age(Duration::from_secs(30)),
					false => moq_net::track::Info::default(),
				};
				let track = broadcast.broadcast().create_track("video", info).unwrap();
				let frames = if timed { FRAMES } else { 1 };
				let at = |frame: u64| match timed {
					true => moq_net::Timestamp::from_micros(START + frame * 33_000).unwrap(),
					false => moq_net::Timestamp::unscaled(0),
				};
				let mut group = track.append_group(at(0)).unwrap();
				for frame in 0..frames {
					group
						.write_frame(at(if timed { frame } else { 0 }), &b"frame"[..])
						.unwrap();
				}
				group.finish().unwrap();
				let finished = track.finish().unwrap();
				broadcast.announce().unwrap();

				let relay = produce_origin(2);
				let mut options = MockConnectOptions::new("moq-lite-07-wip".parse().unwrap());
				options.server_publish = Some(publisher.consume());
				options.client_subscribe = Some(relay.clone());
				let upstream = connect_mock(options).await;
				kio::clock::sleep(Duration::from_millis(round % 4)).await;
				// A reader before it, whose copy the relay keeps once its upstream subscription ends.
				if warm {
					let (got, end) = read(&relay, "moq-lite-07-wip".parse().unwrap(), Subscription::default()).await;
					assert!(!got.is_empty() && end.is_ok(), "the first reader reads the track");
					kio::clock::sleep(Duration::from_millis(1 + round % 8)).await;
				}

				let subscription = match timed {
					true => Subscription::default().with_max(Duration::from_secs(20)),
					false => Subscription::default(),
				};
				let (got, end) = read(&relay, version.parse().unwrap(), subscription).await;
				let want = if timed { vec![START; FRAMES as usize] } else { vec![0] };
				assert_eq!(
					(got, end.is_ok()),
					(want, true),
					"{version}, timed {timed}, warm {warm}, round {round}"
				);
				drop((finished, broadcast, upstream, publisher, relay));
			}
		}
	}
}
