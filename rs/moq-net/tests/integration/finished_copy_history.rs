#![cfg(feature = "async")]

//! A relay copy whose publisher finished the track still answers a reader's range with every
//! group upstream holds, including ones the copy received and then evicted: the copy's tail
//! settled only what its subscription owed, not what a later reader asks for.

use crate::support;

use std::time::Duration;

use moq_net::track::Subscription;
use moq_net::{Timestamp, Version, origin, track};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

#[kio::test]
async fn a_finished_copy_reads_back_what_it_evicted() {
	for version in ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"] {
		for during_tail in [true, false] {
			read_back(version.parse().unwrap(), during_tail).await;
		}
	}
}

/// The copy receives groups 0 to 5, expires them, and the publisher finishes the track after
/// one more. A reader then asks for 1 to 3, and a FETCH of 4 follows: while the copy's tail
/// still waits for the last group (`during_tail`), or once the copy finished. The FETCH asks
/// outside the reader's run, so it joins no fill of the reader's. During the tail, the copy
/// ends and finishes while the reader's history is still on its way.
async fn read_back(version: Version, during_tail: bool) {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(1).unwrap());
	config.proxy = true;
	// The publisher holds its groups for good; the copy's cache expires them.
	config.pool = moq_net::cache::Pool::unbounded();
	let (publisher, driver) = origin::Producer::new(config);
	support::harness::spawn(driver);
	let broadcast = publisher.route("bcast", origin::RouteOptions::hidden()).unwrap();
	let track = broadcast
		.broadcast()
		.create_track(
			"video",
			track::Info::default()
				.with_timescale(moq_net::Timescale::SECOND)
				.with_max_age(None),
		)
		.unwrap();
	broadcast.announce().unwrap();
	let write = |start: u64| {
		let group = track
			.create_group(Timestamp::new(start, moq_net::Timescale::SECOND).unwrap(), &b"x"[..])
			.unwrap();
		group.finish().unwrap()
	};

	let mut config = origin::Config::new(std::num::NonZeroU64::new(2).unwrap());
	config.proxy = true;
	let (relay, driver) = origin::Producer::new(config);
	support::harness::spawn(driver);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let upstream = connect_mock(options).await;
	let remote = relay
		.consume()
		.follow("bcast", None)
		.unwrap()
		.available()
		.await
		.unwrap()
		.unwrap();
	let history = |ranges: track::Range| {
		let track = remote.track("video").unwrap();
		async move {
			track
				.subscribe(Subscription::default().with_ranges([ranges]))
				.await
				.unwrap()
		}
	};

	// The live subscription delivers every group, which the copy then expires while a reader
	// keeps it live.
	let mut first = history(track::Range::ALL).await;
	support::harness::subscribed(&track).await;
	let mut written: Vec<_> = (0..6).map(write).collect();
	for _ in 0..6 {
		kio::clock::timeout(TIMEOUT, first.recv_group())
			.await
			.unwrap_or_else(|_| panic!("{version}: group timeout"))
			.unwrap()
			.expect("a group");
	}
	kio::clock::sleep(moq_net::cache::DEFAULT_EXPIRY * 2 + Duration::from_secs(2)).await;
	// Lite-05/06 carry the name's datagrams on the copy's subscription, so they end as the
	// copy finishes.
	let mut datagrams = match version.as_str() != "moq-lite-07-wip" {
		true => {
			let datagrams = remote.datagrams("video").unwrap();
			kio::clock::timeout(TIMEOUT, datagrams.info())
				.await
				.expect("datagram info timeout")
				.expect("the datagram track resolves");
			Some(datagrams)
		}
		false => None,
	};

	// The publisher writes one more group and finishes the track. During the tail, that group's
	// stream is held, so the copy's subscription ends while its tail still waits for it.
	if during_tail {
		upstream.server_transport.hold_unis();
	}
	written.push(write(6));
	let _ended = track.finish().unwrap();
	if during_tail {
		kio::clock::sleep(Duration::from_millis(100)).await;
	} else {
		let last = kio::clock::timeout(TIMEOUT, first.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("the last group");
		assert_eq!(last.start, 6);
		let end = kio::clock::timeout(TIMEOUT, first.recv_group())
			.await
			.expect("end timeout");
		assert!(matches!(end, Ok(None)), "{version}: the copy finished");
	}

	let second = history(support::keys(1..4)).await;
	let read = kio::clock::sim::spawn(async move {
		let mut second = second;
		let mut keys = Vec::new();
		let end = loop {
			match second.recv_group().await {
				Ok(Some(group)) => keys.push(group.start),
				end => break end.map(|_| ()),
			}
		};
		keys.sort();
		(keys, end)
	});
	let fetch = kio::clock::sim::spawn({
		let track = remote.track("video").unwrap();
		async move {
			let mut group = track.fetch_group(4, None).await?;
			let frame = group.read_frame().await?;
			Ok::<_, moq_net::Error>(frame.map(|frame| frame.payload.to_vec()))
		}
	});
	if during_tail {
		// Only the last group's stream arrives: the reader's history, still held, keeps back
		// neither the copy's end nor its finish.
		kio::clock::sleep(Duration::from_millis(100)).await;
		assert!(
			upstream.server_transport.held_unis() > 1,
			"{version}: the reader's history is held behind the last group"
		);
		upstream.server_transport.release_oldest_uni();
		let last = kio::clock::timeout(TIMEOUT, first.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("the last group");
		assert_eq!(last.start, 6);
		let end = kio::clock::timeout(TIMEOUT, first.recv_group())
			.await
			.expect("end timeout");
		assert!(matches!(end, Ok(None)), "{version}: the copy ended");
		if let Some(datagrams) = &mut datagrams {
			// Well inside the recovery's one-second grace, past which it gives the held
			// history up.
			let end = kio::clock::timeout(Duration::from_millis(100), datagrams.recv())
				.await
				.unwrap_or_else(|_| panic!("{version}: the copy's finish waits on the reader's history"));
			assert!(matches!(end, Ok(None)), "{version}: {end:?}");
		}
		upstream.server_transport.release_unis();
	}

	let (keys, end) = kio::clock::timeout(TIMEOUT, read)
		.await
		.unwrap_or_else(|_| panic!("{version}, during tail {during_tail}: the reader reaches the end of its range"))
		.unwrap();
	assert_eq!(keys, [1, 2, 3], "{version}, during tail {during_tail}");
	assert!(end.is_ok(), "{version}, during tail {during_tail}: {end:?}");
	let fetched = kio::clock::timeout(TIMEOUT, fetch)
		.await
		.expect("the FETCH is answered")
		.unwrap();
	assert_eq!(
		fetched.expect("the FETCH gets its group"),
		Some(b"x".to_vec()),
		"{version}, during tail {during_tail}"
	);
	drop((broadcast, upstream, publisher, relay, written, first, datagrams));
}

/// A lite-07 copy whose publisher finished the track asks upstream for a reader's time start
/// it cannot place among the groups it holds, as a live copy does, rather than settle it as
/// though nothing could arrive: here the group containing the time was received and then
/// evicted.
#[kio::test]
async fn a_finished_copy_asks_for_a_time_it_cannot_place() {
	for ask in [Ask::DuringTail, Ask::Finished] {
		place_time(ask).await;
	}
}

/// Upstream refusing the range a finished copy asks to place a time fails the reader waiting
/// on it with upstream's error, rather than leaving it waiting, and the copy serves on.
#[kio::test]
async fn a_finished_copy_fails_a_time_upstream_refuses() {
	place_time(Ask::Refused).await;
}

/// Upstream's refusal reaches a reader of the finished copy over the wire too, rather than
/// leaving the range it asks waiting for a start.
#[kio::test]
async fn a_finished_copy_fails_a_time_upstream_refuses_downstream() {
	place_time(Ask::RefusedDownstream).await;
}

/// A finished copy lets go of the range placing a time once nobody reads the copy, even while
/// the group it placed is still arriving.
#[kio::test]
async fn a_finished_copy_lets_go_of_a_time_nobody_reads() {
	place_time(Ask::Abandoned).await;
}

/// When a reader asks the finished copy for a time it cannot place, and what upstream does.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Ask {
	/// While the copy's tail still waits for the last group.
	DuringTail,
	/// Once the copy finished.
	Finished,
	/// Once the copy finished and upstream no longer lists the track.
	Refused,
	/// As `Refused`, read through a session downstream of the copy.
	RefusedDownstream,
	/// Once the copy finished, leaving before the placed group's frames arrive.
	Abandoned,
}

/// The copy receives groups 0 to 20 s, five seconds apart, and expires all but the newest.
/// The publisher then finishes the track after one more, and a reader asks for time 18 s.
async fn place_time(ask: Ask) {
	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let mut config = origin::Config::new(std::num::NonZeroU64::new(1).unwrap());
	config.proxy = true;
	config.pool = moq_net::cache::Pool::unbounded();
	let (publisher, driver) = origin::Producer::new(config);
	support::harness::spawn(driver);
	let broadcast = publisher.route("bcast", origin::RouteOptions::hidden()).unwrap();
	let track = broadcast
		.broadcast()
		.create_track(
			"video",
			track::Info::default()
				.with_timescale(moq_net::Timescale::MILLI)
				.with_max_age(None),
		)
		.unwrap();
	broadcast.announce().unwrap();
	// Each group starts at its own media time, and appending skips the keys between.
	let write = |seconds: u64| {
		let start = Timestamp::from_millis(seconds * 1000).unwrap();
		let group = track.append_group(start, &b"x"[..]).unwrap();
		group.finish().unwrap()
	};

	let mut config = origin::Config::new(std::num::NonZeroU64::new(2).unwrap());
	config.proxy = true;
	let (relay, driver) = origin::Producer::new(config);
	support::harness::spawn(driver);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let upstream = connect_mock(options).await;
	let remote = relay
		.consume()
		.follow("bcast", None)
		.unwrap()
		.available()
		.await
		.unwrap()
		.unwrap();
	let history = |ranges: track::Range| {
		let track = remote.track("video").unwrap();
		async move {
			track
				.subscribe(Subscription::default().with_ranges([ranges]))
				.await
				.unwrap()
		}
	};

	// A live reader keeps the copy up without asking for history, which would fill the evicted
	// groups back in. The copy expires all but the newest group.
	let mut first = history(track::Range::from(track::Start::Live {
		min: Some(Duration::ZERO),
		max: None,
	}))
	.await;
	support::harness::subscribed(&track).await;
	let mut written: Vec<_> = (0..5).map(|n| write(n * 5)).collect();
	for _ in 0..5 {
		kio::clock::timeout(TIMEOUT, first.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("a group");
	}
	kio::clock::sleep(moq_net::cache::DEFAULT_EXPIRY * 2 + Duration::from_secs(2)).await;
	// During the tail, the last group's stream is held, so the reader asks before the copy finished.
	let during_tail = ask == Ask::DuringTail;
	if during_tail {
		upstream.server_transport.hold_unis();
	}
	written.push(write(25));
	let mut ended = Some(track.finish().unwrap());
	let mut second = None;
	if during_tail {
		kio::clock::sleep(Duration::from_millis(100)).await;
		second = Some(history(support::keys(18_000..=18_000)).await);
		kio::clock::sleep(Duration::from_millis(100)).await;
		upstream.server_transport.release_unis();
	}
	let last = kio::clock::timeout(TIMEOUT, first.recv_group())
		.await
		.expect("group timeout")
		.unwrap()
		.expect("the last group");
	assert_eq!(last.start, 25_000);
	let end = kio::clock::timeout(TIMEOUT, first.recv_group())
		.await
		.expect("end timeout");
	assert!(matches!(end, Ok(None)), "the copy finished");

	match ask {
		// The track leaves upstream's listing, so a range asking it is refused.
		Ask::Refused | Ask::RefusedDownstream => ended = None,
		// The placed group's header lands, and its frame is held.
		Ask::Abandoned => upstream.server_transport.split_unis(),
		Ask::DuringTail | Ask::Finished => {}
	}
	let mut downstream = None;
	let mut second = match second {
		Some(second) => second,
		None if ask == Ask::RefusedDownstream => {
			let mut config = origin::Config::new(std::num::NonZeroU64::new(3).unwrap());
			config.proxy = true;
			let (client, driver) = origin::Producer::new(config);
			support::harness::spawn(driver);
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(relay.consume());
			options.client_subscribe = Some(client.clone());
			downstream = Some(connect_mock(options).await);
			let remote = client
				.consume()
				.follow("bcast", None)
				.unwrap()
				.available()
				.await
				.unwrap()
				.unwrap();
			remote
				.track("video")
				.unwrap()
				.subscribe(Subscription::default().with_ranges([support::keys(18_000..=18_000)]))
				.await
				.unwrap()
		}
		None => history(support::keys(18_000..=18_000)).await,
	};
	let group = kio::clock::timeout(TIMEOUT, second.recv_group())
		.await
		.unwrap_or_else(|_| panic!("{ask:?}: the copy asks upstream for time 18 s"));
	if matches!(ask, Ask::Refused | Ask::RefusedDownstream) {
		let err = group.err();
		assert!(err.is_some(), "upstream's refusal fails the reader");
		// Only that reader: the copy still serves the group it holds.
		let mut held = history(support::keys(25_000..=25_000)).await;
		let group = kio::clock::timeout(TIMEOUT, held.recv_group())
			.await
			.expect("held group timeout")
			.unwrap()
			.expect("the held group");
		assert_eq!(group.start, 25_000);
		drop((broadcast, upstream, downstream, publisher, relay, written, first));
		return;
	}
	let mut group = group.unwrap().expect("the group containing it");
	assert_eq!(group.start, 15_000, "{ask:?}");
	if ask == Ask::Abandoned {
		// Every reader leaves. On lite-07 one-shot SUBSCRIBE + FIN, cancelling sends STOP_SENDING
		// on the receive half, so the publisher resets the delivering stream on its side.
		let client_resets = upstream.client_transport.resets().len();
		let server_resets = upstream.server_transport.resets().len();
		drop((group, second, first, last));
		kio::clock::sleep(Duration::from_millis(100)).await;
		assert!(
			upstream.client_transport.resets().len() > client_resets
				|| upstream.server_transport.resets().len() > server_resets,
			"the copy nobody reads cancels the range still delivering its group"
		);
		drop((broadcast, upstream, publisher, relay, written, ended));
		return;
	}
	// Whole: the probe that placed the reader outlives its ask until the group is read.
	let frame = kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("frame timeout")
		.unwrap()
		.expect("its frame");
	assert_eq!(&frame.payload[..], b"x");
	let end = kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("group end timeout");
	assert!(matches!(end, Ok(None)), "{ask:?}: {end:?}");
	drop((broadcast, upstream, publisher, relay, written, first, ended));
}
