//! Legacy audio read across a session, on wires that can and can't say the starts between two
//! groups were skipped.

use std::time::Duration;

use bytes::Bytes;
use moq_mux::catalog::hang::Container;
use moq_mux::container::{Consumer, Frame, Producer, Read};
use moq_mux::container::Container as _;

/// How long a step may take on the paused clock before the test fails.
const TIMEOUT: Duration = Duration::from_secs(10);

/// One 20 ms CELT packet, which states its own duration in its TOC byte.
const PACKET: [u8; 2] = [31 << 3, 0];

fn origin() -> moq_net::origin::Producer {
	let (origin, driver) = moq_net::origin::Producer::new(Default::default());
	tokio::spawn(moq_net::time::run(driver));
	origin
}

/// A publisher's origin and a subscriber's origin connected over one session on `version`.
async fn connect(
	version: moq_net::Version,
) -> (moq_net::origin::Producer, moq_net::origin::Producer, [moq_net::Session; 2]) {
	let publisher = origin();
	let subscriber = origin();
	let mut transport = moq_net::transport::memory::Config::default();
	transport.protocol = Some(version.alpn().to_string());
	let (client_transport, server_transport) = moq_net::transport::memory::pair(transport);
	let client = moq_net::Client::new()
		.with_versions(version.into())
		.with_subscriber(subscriber.clone());
	let server = moq_net::Server::new()
		.with_versions(version.into())
		.with_publisher(publisher.consume());
	let (client, server) = tokio::join!(
		async {
			let (session, driver) = client
				.connect(kio::clock::now(), client_transport)
				.await
				.expect("client session");
			tokio::spawn(moq_net::time::run(driver));
			session
		},
		async {
			let (session, driver) = server
				.accept(kio::clock::now(), server_transport)
				.await
				.expect("server session");
			tokio::spawn(moq_net::time::run(driver));
			session
		}
	);
	(publisher, subscriber, [client, server])
}

/// An encoder's one-packet groups follow each other at their starts, and the publisher
/// declares the starts between them skipped. moq-lite-07 says so; earlier wires report the
/// run as dropped, so only the packets' codec durations show nothing was lost.
#[tokio::test(start_paused = true)]
async fn one_packet_opus_groups_read_on_without_a_gap() {
	for version in ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"] {
		let (publisher, subscriber, _sessions) = connect(version.parse().unwrap()).await;
		let config = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		let route = publisher.route("live", Default::default()).expect("route");
		let track = route
			.broadcast()
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.expect("track");
		let mut producer = Producer::new(track, Container::try_from(&config).unwrap());

		let remote = kio::clock::timeout(TIMEOUT, subscriber.consume().follow("live", None).unwrap().available())
			.await
			.expect("announced")
			.expect("remote broadcast")
			.unwrap();
		let track = remote.track("audio").expect("track");
		let subscription = moq_net::track::Subscription::default()
			.with_delay(moq_net::track::Delay::default().with_max(Duration::from_secs(5)));
		let subscribed = kio::clock::timeout(TIMEOUT, track.subscribe(subscription))
			.await
			.expect("subscribed")
			.expect("subscription");
		let mut reader = Consumer::new(subscribed, Container::try_from(&config).unwrap());

		for n in 0..10u64 {
			let start = moq_net::Timestamp::from_micros(n * 20_000).unwrap();
			let end = moq_net::Timestamp::from_micros((n + 1) * 20_000).unwrap();
			producer
				.write(Frame {
					timestamp: start,
					payload: Bytes::from_static(&PACKET),
					keyframe: true,
					duration: None,
				})
				.unwrap();
			producer.cut(Some(end)).unwrap();
			let read = kio::clock::timeout(TIMEOUT, reader.read())
				.await
				.unwrap_or_else(|_| panic!("{version}: packet {n} never read"))
				.unwrap()
				.expect("a read");
			match read {
				Read::Frame { frame, .. } => assert_eq!(frame.timestamp, start, "{version}"),
				other => panic!("{version}: packet {n} read as {other:?}"),
			}
			kio::clock::sleep(Duration::from_millis(20)).await;
		}
	}
}

/// A run reported dropped, as moq-lite-05 and 06 report a skip, still waits for the group past
/// it to show where it starts: stepping before that group's first packet lands would call a
/// contiguous timeline a loss.
#[tokio::test(start_paused = true)]
async fn a_dropped_run_waits_for_the_next_groups_first_packet() {
	for version in ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"] {
		let (publisher, subscriber, _sessions) = connect(version.parse().unwrap()).await;
		let config = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		let container = Container::try_from(&config).unwrap();
		let route = publisher.route("live", Default::default()).expect("route");
		let track = route
			.broadcast()
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.expect("track");

		let remote = kio::clock::timeout(TIMEOUT, subscriber.consume().follow("live", None).unwrap().available())
			.await
			.expect("announced")
			.expect("remote broadcast")
			.unwrap();
		let subscription = moq_net::track::Subscription::default()
			.with_delay(moq_net::track::Delay::default().with_max(Duration::from_secs(5)));
		let subscribed = kio::clock::timeout(TIMEOUT, remote.track("audio").expect("track").subscribe(subscription))
			.await
			.expect("subscribed")
			.expect("subscription");
		let mut reader = Consumer::new(subscribed, Container::try_from(&config).unwrap());

		for n in 0..4u64 {
			let start = n * 20_000;
			// Appending declares every start since the previous group skipped, before the packet.
			let mut group = track.append_group(start).unwrap();
			let early = kio::clock::timeout(Duration::from_millis(50), reader.read()).await;
			assert!(early.is_err(), "{version}: read {early:?} before packet {n} landed");
			let packet = Frame {
				timestamp: moq_net::Timestamp::from_micros(start).unwrap(),
				payload: Bytes::from_static(&PACKET),
				keyframe: true,
				duration: None,
			};
			container.write(&mut group, &[packet]).unwrap();
			group.finish(start + 20_000).unwrap();
			let read = kio::clock::timeout(TIMEOUT, reader.read())
				.await
				.unwrap_or_else(|_| panic!("{version}: packet {n} never read"))
				.unwrap()
				.expect("a read");
			assert!(matches!(read, Read::Frame { .. }), "{version}: packet {n} read as {read:?}");
		}
	}
}
