#![cfg(feature = "async")]

//! A request bound ends delivery, without declaring the source group complete.

use crate::support::harness::{MockConnectOptions, connect_mock, spawn};
use moq_net::{Hop, Timescale, Timestamp, Version, origin, track};
use std::time::Duration;

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	origin
}

fn range(start: u64, end: u64) -> track::Subscription {
	track::Subscription::default().with_ranges([track::Range {
		start: track::Anchor::Absolute(track::Position { group: 5, frame: start }),
		end: Some(track::Anchor::Absolute(track::Position { group: 5, frame: end })),
	}])
}

/// How the subscribers ask for group 5, one after another.
struct Demand {
	/// Where each subscriber's range ends and the next one's starts; the last ends at 5.
	cuts: &'static [u64],
	/// Each subscriber leaves before the next one asks.
	leave: bool,
}

/// Subscribers read group 5 of a `frames`-frame source in consecutive ranges, through two
/// relays on `version` or locally on `None`, each getting exactly its own frames.
async fn bounded_tail(version: Option<Version>, compressed: bool, frames: u64, demand: Demand) {
	let source = origin(1);
	let relay = origin(2);
	let reader = origin(3);
	let broadcast = source.create_broadcast("bcast").unwrap();
	let producer = broadcast
		.create_track(
			"data",
			track::Info::default()
				.with_timescale(Timescale::MILLI)
				.with_compress(compressed),
		)
		.unwrap();
	broadcast.announce(Default::default()).unwrap();
	let mut links = Vec::new();
	let remote = if let Some(version) = version {
		for (upstream, downstream) in [(&source, &relay), (&relay, &reader)] {
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(upstream.consume());
			options.client_subscribe = Some(downstream.clone());
			links.push(connect_mock(options).await);
		}
		reader.consume().routed("bcast").await.unwrap();
		reader.consume().request_broadcast("bcast", None).await.unwrap()
	} else {
		broadcast.consume()
	};
	let track = remote.track("data").unwrap();

	let mut bounds = vec![0];
	bounds.extend_from_slice(demand.cuts);
	bounds.push(5);
	let mut held = Vec::new();
	for (index, pair) in bounds.windows(2).enumerate() {
		let (start, end) = (pair[0], pair[1]);
		let mut subscriber = track.subscribe(range(start, end)).await.unwrap();
		if index == 0 {
			crate::support::harness::subscribed(&producer).await;
			let mut group = producer.create_group(moq_net::group::Info { sequence: 5 }).unwrap();
			for frame in 0..frames {
				group
					.write_frame(Timestamp::from_millis(frame).unwrap(), vec![frame as u8; 100])
					.unwrap();
			}
			group.finish().unwrap();
		}
		let mut group = subscriber.recv_group().await.unwrap().unwrap();
		// IETF passthrough readers expose frame ordinals independently of Object IDs;
		// narrow this cursor too so that a stale prefix cannot hide a missing tail.
		group.set_frames(start..end);
		for frame in start..end.min(frames) {
			let read = group.read_frame().await.unwrap().expect("the requested frame");
			assert_eq!(read.payload, vec![frame as u8; 100]);
		}
		assert!(group.read_frame().await.unwrap().is_none());
		match demand.leave {
			true => drop((group, subscriber)),
			false => held.push((group, subscriber)),
		}
	}
	drop((held, links));
}

async fn run(version: Option<Version>, compressed: bool, frames: u64, demand: Demand) {
	moq_clock::timeout(
		Duration::from_secs(1),
		bounded_tail(version, compressed, frames, demand),
	)
	.await
	.expect("every subscriber reads its frames");
}

fn lite07() -> Option<Version> {
	Some("moq-lite-07-wip".parse().unwrap())
}

const TAIL: Demand = Demand {
	cuts: &[3],
	leave: false,
};

#[moq_clock::test]
async fn a_bounded_plain_copy_accepts_its_tail() {
	run(lite07(), false, 5, TAIL).await;
}

#[moq_clock::test]
async fn a_bounded_compressed_copy_accepts_its_tail() {
	run(lite07(), true, 5, TAIL).await;
}

#[moq_clock::test]
async fn local_bounded_readers_keep_the_same_content() {
	for compressed in [false, true] {
		run(None, compressed, 5, TAIL).await;
	}
}

#[moq_clock::test]
async fn a_group_ending_before_or_at_the_request_bound_ends_cleanly() {
	for compressed in [false, true] {
		for frames in [2, 3] {
			run(lite07(), compressed, frames, TAIL).await;
		}
	}
}

/// Each widening continues the copy where the last stream stopped.
#[moq_clock::test]
async fn repeated_widening_continues_the_copy() {
	for compressed in [false, true] {
		let demand = Demand {
			cuts: &[2, 4],
			leave: false,
		};
		run(lite07(), compressed, 5, demand).await;
	}
}

/// A copy whose subscription ended is not continued, and a later request still gets the
/// frames it asks for.
#[moq_clock::test]
async fn a_tail_after_the_subscription_ended_is_fetched_anew() {
	for compressed in [false, true] {
		let demand = Demand {
			cuts: &[3],
			leave: true,
		};
		run(lite07(), compressed, 5, demand).await;
	}
}
