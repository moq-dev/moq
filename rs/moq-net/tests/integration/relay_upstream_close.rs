#![cfg(feature = "async")]

//! A finished track keeps reaching a relay's existing subscribers after the publisher's
//! session to the relay ends, cleanly or not.
//!
//! A broadcast has no finished state: a publisher ends its content with a track FIN and
//! goes offline by closing. So nothing keeps the upstream session up for a downstream
//! subscriber still behind: the relay has to serve the tail from its own copy.
//!
//! Deterministic: simulated time over the mock transport. The relay's group streams to
//! the subscriber are held, with a one-message stream buffer, so the relay still holds
//! most of the track when the upstream session ends.

use crate::support;

use std::time::Duration;

use moq_net::track::Subscription;
use moq_net::{Error, Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

const TIMEOUT: Duration = Duration::from_secs(10);
/// Lite03 and Lite04 declare no end, so each hop waits this long for a group still owed.
const MAX_DELAY: Duration = Duration::from_secs(1);
const GROUPS: u64 = 4;
const FRAMES: u64 = 8;

const VERSIONS: &[&str] = &[
	"moq-lite-03",
	"moq-lite-04",
	"moq-lite-05",
	"moq-lite-06",
	"moq-lite-07-wip",
	"moq-transport-14",
	"moq-transport-17",
	"moq-transport-22",
];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// How the publisher's session to the relay ends.
#[derive(Clone, Copy, Debug)]
enum End {
	/// `Session::close`, which drains the tail first.
	Close,
	/// `Session::abort`, once the relay has the whole track.
	Abort,
}

/// Publish a finished track through a relay, end the publisher's session while the
/// subscriber has read none of it, and return the frames it then reads and how it ended.
async fn round(name: &str, end: End) -> (Vec<(u64, u64)>, Result<(), Error>) {
	let version: Version = name.parse().unwrap();
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast.create_track("video", timed()).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let relay = produce_origin(2);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let upstream = connect_mock(options).await;

	let subscriber = produce_origin(3);
	let mut options = MockConnectOptions::new(version);
	options.transport.stream_buffer = 0;
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(subscriber.clone());
	let downstream = connect_mock(options).await;

	let consumer = subscriber.consume();
	moq_clock::timeout(TIMEOUT, consumer.routed("bcast"))
		.await
		.expect("announce timeout")
		.expect("routed");
	let remote = moq_clock::timeout(TIMEOUT, consumer.request_broadcast("bcast", None))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");

	let (ready, active) = futures::channel::oneshot::channel();
	let reader = moq_clock::sim::spawn(async move {
		let subscription = Subscription::default().with_max_delay(MAX_DELAY).with_range(0..);
		let mut sub = remote
			.track("video")
			.unwrap()
			.subscribe(subscription)
			.await
			.expect("subscribe");
		let _ = ready.send(());
		let mut got = Vec::new();
		loop {
			let mut group = match sub.recv_group().await {
				Ok(Some(group)) => group,
				Ok(None) => return (got, Ok(())),
				Err(err) => return (got, Err(err)),
			};
			loop {
				match group.read_frame().await {
					Ok(Some(frame)) => {
						let index: u64 = std::str::from_utf8(&frame.payload).unwrap().parse().unwrap();
						got.push((group.sequence, index));
					}
					Ok(None) => break,
					Err(err) => return (got, Err(err)),
				}
			}
		}
	});

	moq_clock::timeout(TIMEOUT, support::harness::subscribed(&track))
		.await
		.expect("no subscriber appeared");
	moq_clock::timeout(TIMEOUT, active)
		.await
		.expect("the subscription never became active")
		.expect("reader alive");

	// The subscriber now reads nothing until the upstream session is gone.
	downstream.server_transport.hold_unis();
	for sequence in 0..GROUPS {
		let mut group = track.append_group().unwrap();
		for index in 0..FRAMES {
			let at = Timestamp::from_millis(sequence * FRAMES + index).unwrap();
			group.write_frame(at, index.to_string().into_bytes()).unwrap();
		}
		group.finish().unwrap();
	}
	track.finish().unwrap();

	match end {
		End::Close => moq_clock::timeout(TIMEOUT, upstream.server.clone().close())
			.await
			.expect("close timed out")
			.expect("the close drains"),
		End::Abort => {
			// Simulated time advances only once every task is idle: the relay has read
			// everything the unheld upstream hop carried.
			moq_clock::sleep(Duration::from_millis(100)).await;
			upstream.server.abort(Error::Cancel);
		}
	}
	drop((track, broadcast, publisher, upstream));
	// The relay sees the session and its broadcast go before the subscriber reads on.
	moq_clock::sleep(Duration::from_millis(100)).await;
	downstream.server_transport.release_unis();

	let outcome = moq_clock::timeout(TIMEOUT, reader)
		.await
		.unwrap_or_else(|_| panic!("{name} {end:?}: the track never ended"))
		.expect("reader panicked");
	drop((downstream, relay, subscriber));
	outcome
}

/// Every frame, then a clean end. Lite03 and Lite04 declare no end, so the relay is still
/// waiting out its tail grace when the session goes.
///
/// Their close is left out: it drains on the transport's acknowledgement of the
/// subscription's FIN, which the relay may not have read when the session closes, and
/// without a declared end nothing else says the track is over.
#[moq_clock::test]
async fn relay_delivers_a_finished_track_after_upstream_closes() {
	let want: Vec<(u64, u64)> = (0..GROUPS)
		.flat_map(|group| (0..FRAMES).map(move |frame| (group, frame)))
		.collect();
	for version in VERSIONS {
		let ends: &[End] = match *version {
			"moq-lite-03" | "moq-lite-04" => &[End::Abort],
			_ => &[End::Close, End::Abort],
		};
		for &end in ends {
			let (mut got, result) = round(version, end).await;
			// Groups arrive in any order; each one's frames arrive in order.
			got.sort_by_key(|&(group, _)| group);
			assert_eq!(got, want, "{version} {end:?}: result={result:?}");
			assert!(result.is_ok(), "{version} {end:?}: ended with {result:?}");
		}
	}
}
