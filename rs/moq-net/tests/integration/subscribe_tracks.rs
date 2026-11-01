#![cfg(feature = "async")]

//! A SUBSCRIBE_TRACKS is refused on its own stream, and the session keeps delivering.
//!
//! Deterministic: paused time and a single-threaded runtime over the mock transport.

use crate::support;

use std::time::Duration;

use futures::StreamExt;

use moq_net::transport::poll::{RecvStream as _, SendStream as _, Session as _};
use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

const TIMEOUT: Duration = Duration::from_secs(10);

/// SUBSCRIBE_TRACKS for prefix `room`, with no parameters. Every value fits a one-byte
/// varint, so the frame is hand-rolled.
#[rustfmt::skip]
const SUBSCRIBE_TRACKS: &[u8] = &[
	0x51, // SUBSCRIBE_TRACKS
	0x00, 0x08, // Length
	0x3f, // Fresh server-parity Request ID, after the existing subscription.
	0x01, 0x04, b'r', b'o', b'o', b'm', // Track Namespace Prefix
	0x00, // Number of Parameters
];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// A client publishing one track to a server subscribed to it, plus the payloads the
/// server has read so far.
struct Setup {
	pair: MockPair,
	track: moq_net::track::Producer,
	frames: futures::channel::mpsc::UnboundedReceiver<Vec<u8>>,
	_broadcast: moq_net::origin::Publication,
}

async fn setup(version: Version) -> Setup {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("room").unwrap();
	let track = broadcast.create_track("video", timed()).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let subscriber = produce_origin(2);
	let mut options = MockConnectOptions::new(version);
	options.client_publish = Some(publisher.consume());
	options.server_subscribe = Some(subscriber.clone());
	let pair = connect_mock(options).await;

	let consumer = subscriber.consume();
	let remote = moq_clock::timeout(TIMEOUT, consumer.follow("room", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");

	let (tx, frames) = futures::channel::mpsc::unbounded();
	moq_clock::sim::spawn(async move {
		let mut sub = remote.track("video").unwrap().subscribe(None).await.expect("subscribe");
		while let Ok(Some(mut group)) = sub.recv_group().await {
			while let Ok(Some(frame)) = group.read_frame().await {
				let _ = tx.unbounded_send(frame.payload.to_vec());
			}
		}
	});

	moq_clock::timeout(TIMEOUT, crate::support::wanted(track.demand()))
		.await
		.expect("no subscriber appeared")
		.unwrap();

	Setup {
		pair,
		track,
		frames,
		_broadcast: broadcast,
	}
}

async fn deliver(setup: &mut Setup, payload: &'static [u8]) {
	let mut group = setup.track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, payload).unwrap();
	group.finish().unwrap();
	let got = moq_clock::timeout(TIMEOUT, setup.frames.next())
		.await
		.expect("frame timed out")
		.expect("the subscription ended");
	assert_eq!(got, payload);
}

/// Send SUBSCRIBE_TRACKS from the server to the publishing client, returning every byte
/// of the reply once the client ends the stream.
async fn subscribe_tracks(pair: &MockPair) -> Vec<u8> {
	let (mut send, mut recv) = pair
		.server_transport
		.clone()
		.open_bi(Default::default())
		.await
		.expect("open_bi");
	send.write(SUBSCRIBE_TRACKS).await.expect("write");

	let mut reply = Vec::new();
	while let Some(chunk) = moq_clock::timeout(TIMEOUT, recv.read_chunk(usize::MAX))
		.await
		.expect("reply timed out")
		.expect("read")
	{
		reply.extend_from_slice(&chunk);
	}
	reply
}

/// We list no tracks by PUBLISH, so a SUBSCRIBE_TRACKS gets NOT_SUPPORTED on its own
/// stream, and the subscription already on the session keeps going.
#[moq_clock::test]
async fn subscribe_tracks_is_refused_per_request() {
	for version in ["moq-transport-18", "moq-transport-20", "moq-transport-22"] {
		let mut setup = setup(version.parse().unwrap()).await;
		deliver(&mut setup, b"before").await;

		let reply = subscribe_tracks(&setup.pair).await;
		// REQUEST_ERROR (0x05), a two-byte length, then the error code: NOT_SUPPORTED (0x3).
		assert_eq!(reply.first(), Some(&0x05), "{version}: not a REQUEST_ERROR: {reply:?}");
		assert_eq!(reply.get(3), Some(&0x03), "{version}: not NOT_SUPPORTED: {reply:?}");

		deliver(&mut setup, b"after").await;
	}
}
