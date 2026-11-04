#![cfg(feature = "async")]

//! A frame larger than a storage segment, written in pieces so its group keeps it as a
//! chain of segments, crosses a session intact on each protocol, read whole and streamed.

use std::time::Duration;

use bytes::Bytes;
use moq_net::{Hop, Timestamp, Version, frame, origin, track};

use crate::support::harness::{MockConnectOptions, connect_mock, spawn, subscribed};

const TIMEOUT: Duration = Duration::from_secs(10);

const VERSIONS: &[&str] = &["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"];

/// Past two segments, so the chain has a full middle and a short tail.
const SIZE: usize = 5 * 1024 * 1024 / 2;

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	origin
}

#[kio::test]
async fn a_segmented_frame_crosses_whole() {
	let payload = Bytes::from((0..SIZE).map(|i| (i % 251) as u8).collect::<Vec<_>>());
	for version in VERSIONS {
		let version: Version = version.parse().unwrap();
		let publisher = origin(1);
		let viewer = origin(2);
		let broadcast = publisher.create_broadcast("room").unwrap();
		let info = track::Info::default().with_timescale(moq_net::Timescale::MILLI);
		let track = broadcast.create_track("file", info).unwrap();
		broadcast.announce(Default::default()).unwrap();

		let mut options = MockConnectOptions::new(version);
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(viewer.clone());
		let _pair = connect_mock(options).await;

		let remote = kio::clock::timeout(TIMEOUT, viewer.consume().follow("room", None).unwrap().available())
			.await
			.expect("broadcast timeout")
			.unwrap();
		let mut sub = remote.track("file").unwrap().subscribe(None).await.unwrap();
		subscribed(&track).await;

		// Two frames: one read whole, one streamed.
		let mut group = track.append_group().unwrap();
		for _ in 0..2 {
			let mut frame = group
				.create_frame(frame::Info {
					size: SIZE as u64,
					timestamp: Some(Timestamp::ZERO),
				})
				.unwrap();
			for chunk in payload.chunks(300_000) {
				frame.write(Bytes::copy_from_slice(chunk)).unwrap();
			}
			group = frame.finish().unwrap();
		}
		group.finish().unwrap();

		let mut group = kio::clock::timeout(TIMEOUT, sub.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("a group");
		let whole = group.read_frame().await.unwrap().expect("a frame");
		assert_eq!(whole.payload, payload, "{version}");

		let mut streamed = group.next_frame().await.unwrap().expect("a frame");
		let mut read = Vec::new();
		while let Some(chunk) = streamed.read_chunk().await.unwrap() {
			read.extend_from_slice(&chunk);
		}
		assert_eq!(read, payload, "{version}");
	}
}
