#![cfg(feature = "async")]

//! Incomplete messages hold the receiver's connection credit, and when they fill the
//! allowance the receiver cancels the least useful stream and keeps going.
//!
//! Deterministic: paused time over the mock transport, whose finite receive allowance
//! stands in for QUIC connection flow control. Without recovery each of these stalls:
//! the sender has spent every byte of credit on messages that can never complete.

use crate::support;

use std::time::Duration;

use bytes::Bytes;
use moq_net::transport::poll::{SendStream as _, Session as _};
use moq_net::{Hop, Timestamp, Version, frame, origin, track::Subscription};
use support::harness::{MockConnectOptions, MockPair, connect_mock, subscribed};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Each side's receive allowance.
const WINDOW: usize = 64 * 1024;

const VERSIONS: &[&str] = &[
	"moq-lite-03",
	"moq-lite-06",
	"moq-transport-14",
	"moq-transport-16",
	"moq-transport-17",
	"moq-transport-22",
];

fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// A client publishing broadcast `room` to a server that subscribes to it, over links
/// that each accept [`WINDOW`] bytes of credit.
struct Setup {
	pair: MockPair,
	broadcast: moq_net::broadcast::Producer,
	remote: moq_net::broadcast::Consumer,
}

async fn setup(version: Version) -> Setup {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("room").unwrap();
	broadcast.announce(Default::default()).unwrap();

	let subscriber = produce_origin(2);
	let mut options = MockConnectOptions::new(version);
	options.client_publish = Some(publisher.consume());
	options.server_subscribe = Some(subscriber.clone());
	options.transport.receive_window = Some(WINDOW);
	let pair = connect_mock(options).await;

	let consumer = subscriber.consume();
	moq_clock::timeout(TIMEOUT, consumer.routed("room"))
		.await
		.expect("announce timeout")
		.expect("routed");
	let remote = moq_clock::timeout(TIMEOUT, consumer.request_broadcast("room", None))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");

	Setup {
		pair,
		broadcast,
		remote,
	}
}

impl Setup {
	/// Subscribe to a new track at `priority`, once the publisher sees the demand.
	async fn subscribe(&self, name: &str, priority: u8) -> (moq_net::track::Producer, moq_net::track::Subscriber) {
		let track = self.broadcast.create_track(name, timed()).unwrap();
		let sub = moq_clock::timeout(
			TIMEOUT,
			self.remote
				.track(name)
				.unwrap()
				.subscribe(Subscription::default().with_priority(priority)),
		)
		.await
		.expect("subscribe timeout")
		.expect("subscribe");
		subscribed(&track).await;
		(track, sub)
	}
}

/// The next frame's payload, which must arrive in time.
async fn next_frame(sub: &mut moq_net::track::Subscriber) -> moq_net::Result<Bytes> {
	let mut group = moq_clock::timeout(TIMEOUT, sub.recv_group())
		.await
		.expect("group timed out")?
		.expect("track ended");
	let frame = moq_clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("frame timed out")?
		.expect("group ended");
	Ok(frame.payload)
}

/// Two partial frames that together exceed the allowance: the lower-priority one is
/// cancelled, and the higher-priority one completes on the credit it returns.
#[moq_clock::test]
async fn the_less_urgent_partial_frame_gives_way() {
	for version in VERSIONS {
		let setup = setup(version.parse().unwrap()).await;
		let (low, mut low_sub) = setup.subscribe("low", 1).await;
		let (high, mut high_sub) = setup.subscribe("high", 200).await;

		// Three quarters of the allowance, held by a frame that never finishes.
		let mut low_group = low.append_group().unwrap();
		let mut low_frame = low_group
			.create_frame(frame::Info {
				size: WINDOW as u64,
				timestamp: Some(Timestamp::ZERO),
			})
			.unwrap();
		low_frame.write(vec![1u8; WINDOW * 3 / 4]).unwrap();

		// Half the allowance, which only fits once the low frame lets go.
		let payload = Bytes::from(vec![2u8; WINDOW / 2]);
		let mut high_group = high.append_group().unwrap();
		high_group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		high_group.finish().unwrap();

		let got = next_frame(&mut high_sub).await.expect("high frame");
		assert_eq!(got, payload, "{version}");

		// The session survives, and the low track keeps delivering what fits. Its
		// cancelled group never completes, so the next frame the subscriber reads is this.
		drop(low_frame);
		drop(low_group);
		let mut group = low.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, &b"after"[..]).unwrap();
		group.finish().unwrap();
		let got = next_frame(&mut low_sub).await.expect("low frame");
		assert_eq!(got, &b"after"[..], "{version}");
		drop(setup.pair);
	}
}

/// Write `bytes` on a new uni stream from the client, in the background: it pends once
/// the allowance runs out.
fn raw_uni(pair: &MockPair, bytes: Vec<u8>) {
	let mut session = pair.client_transport.clone();
	moq_clock::sim::spawn(async move {
		let Ok(mut send) = session.open_uni().await else { return };
		let _ = send.write_chunk(Bytes::from(bytes)).await;
		// Hold the stream open, so only the receiver can end it.
		std::future::pending::<()>().await;
	});
}

/// The prefix of a lite GROUP stream whose header claims `size` bytes and carries `body`
/// of them, so it never identifies itself.
fn partial_group_header(size: u32, body: usize) -> Vec<u8> {
	assert!(body < size as usize);
	let mut bytes = vec![0x00]; // GROUP
	bytes.extend_from_slice(&(0x8000_0000 | size).to_be_bytes()); // A 4-byte varint size.
	bytes.resize(bytes.len() + body, 0);
	bytes
}

/// Headers that never complete fill the allowance: one is cancelled, and a subscription
/// that arrives afterwards still delivers.
#[moq_clock::test]
async fn unidentified_headers_are_cancelled() {
	let setup = setup("moq-lite-06".parse().unwrap()).await;

	// Together more than the allowance, and each short of its declared size.
	raw_uni(&setup.pair, partial_group_header(u16::MAX as u32, 40_000));
	raw_uni(&setup.pair, partial_group_header(u16::MAX as u32, 40_000));

	let (track, mut sub) = setup.subscribe("video", 1).await;
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, vec![3u8; 4096]).unwrap();
	group.finish().unwrap();
	assert_eq!(next_frame(&mut sub).await.expect("frame").len(), 4096);
}

/// Control requests that never complete fill the allowance: one is cancelled with its
/// stream, and the session carries on.
#[moq_clock::test]
async fn partial_control_requests_are_cancelled() {
	let setup = setup("moq-lite-06".parse().unwrap()).await;

	for _ in 0..2 {
		let mut session = setup.pair.client_transport.clone();
		moq_clock::sim::spawn(async move {
			let Ok((mut send, _recv)) = session.open_bi().await else {
				return;
			};
			// ANNOUNCE_PLEASE, with a body that claims more than arrives.
			let mut bytes = vec![0x01];
			bytes.extend_from_slice(&(0x8000_0000u32 | u16::MAX as u32).to_be_bytes());
			bytes.resize(bytes.len() + 40_000, 0);
			let _ = send.write_chunk(Bytes::from(bytes)).await;
			// Hold the stream open, so only the receiver can end it.
			std::future::pending::<()>().await;
		});
	}

	let (track, mut sub) = setup.subscribe("video", 1).await;
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, vec![3u8; 4096]).unwrap();
	group.finish().unwrap();
	assert_eq!(next_frame(&mut sub).await.expect("frame").len(), 4096);
}

/// A frame as large as the allowance allows completes while partial frames of a less
/// urgent track keep arriving, each cancelled as it gets in the way.
#[moq_clock::test]
async fn the_largest_unit_completes_under_pressure() {
	for version in VERSIONS {
		let setup = setup(version.parse().unwrap()).await;
		let (low, _low_sub) = setup.subscribe("low", 1).await;
		let (high, mut high_sub) = setup.subscribe("high", 200).await;

		// Several groups' worth of frames that never finish, opened before and after.
		let mut partial = Vec::new();
		let mut open_partial = || {
			// Leaked, so the frame can borrow its group for the rest of the test.
			let group = Box::leak(Box::new(low.append_group().unwrap()));
			let mut frame = group
				.create_frame(frame::Info {
					size: WINDOW as u64,
					timestamp: Some(Timestamp::ZERO),
				})
				.unwrap();
			frame.write(vec![1u8; WINDOW / 4]).unwrap();
			partial.push(frame);
		};
		open_partial();
		open_partial();

		// Room for the stream's headers and nothing else.
		let payload = Bytes::from(vec![2u8; WINDOW - 64]);
		let mut group = high.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		group.finish().unwrap();
		open_partial();

		let got = next_frame(&mut high_sub).await.expect("high frame");
		assert_eq!(got.len(), payload.len(), "{version}");
		drop(setup.pair);
	}
}
