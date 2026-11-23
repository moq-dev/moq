#![cfg(feature = "async")]

//! A relay copy whose publisher finished the track still answers a reader's range with every
//! group upstream holds, including ones the copy received and then evicted: the copy's tail
//! settled only what its subscription owed, not what a later reader asks for.

use crate::support;

use std::time::Duration;

use moq_net::track::Subscription;
use moq_net::{Timestamp, Version, group, origin, track};
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
/// outside the reader's run, so it joins no fill of the reader's.
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
				.with_timescale(moq_net::Timescale::MILLI)
				.with_max_age(None),
		)
		.unwrap();
	broadcast.announce().unwrap();
	let write = |start: u64| {
		let mut group = track.create_group(group::Info { start }).unwrap();
		group
			.write_frame(Timestamp::from_millis(start * 1000).unwrap(), &b"x"[..])
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
		kio::clock::sleep(Duration::from_millis(100)).await;
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
	drop((broadcast, upstream, publisher, relay, written, first));
}
