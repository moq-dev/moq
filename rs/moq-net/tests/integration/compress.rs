#![cfg(feature = "async")]

//! A compressed track crosses a relay to every kind of subscriber.
//!
//! Negotiated IETF and lite-07 hops keep DEFLATE slices as received. Peers that cannot
//! signal compression receive the plain frames the relay inflates once for all of them.

use crate::support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Negotiated IETF and lite-07 wires, plus versions that require plain fallback.
const VERSIONS: &[&str] = &[
	"moq-lite-07-wip",
	"moq-lite-06",
	"moq-lite-03",
	"moq-transport-18",
	"moq-transport-22",
];

fn compressed() -> moq_net::track::Info {
	moq_net::track::Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_compress(true)
}

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// A repetitive payload, which DEFLATE shrinks, distinct per frame.
fn payload(frame: u64) -> Vec<u8> {
	format!("{{\"frame\":{frame},\"pad\":\"{}\"}}", "abc".repeat(200)).into_bytes()
}

struct Fixture {
	track: moq_net::track::Producer,
	subscriber: moq_net::track::Subscriber,
	_broadcast: moq_net::origin::Publication,
	_hops: Vec<MockPair>,
	_origins: Vec<moq_net::origin::Producer>,
}

/// A track announced by the original publisher, before any relay carries it.
struct Published {
	publisher: moq_net::origin::Producer,
	broadcast: moq_net::origin::Publication,
	track: moq_net::track::Producer,
}

/// Announce one track described by `info`.
fn publish(info: moq_net::track::Info) -> Published {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast.create_track("data", info).unwrap();
	broadcast.announce(Default::default()).unwrap();
	Published {
		publisher,
		broadcast,
		track,
	}
}

/// A compressed track crosses a relay between the requested protocols.
async fn connect(upstream: &str, downstream: &str) -> Fixture {
	relay(
		publish(compressed()),
		[upstream, downstream],
		moq_net::track::Subscription::default(),
	)
	.await
}

/// `published` crosses a relay over the `[upstream, downstream]` protocols to an application
/// asking for `subscription`.
async fn relay(published: Published, hops: [&str; 2], subscription: moq_net::track::Subscription) -> Fixture {
	let Published {
		publisher,
		broadcast,
		track,
	} = published;
	let [upstream, downstream] = hops;

	let relay = produce_origin(2);
	let mut options = MockConnectOptions::new(upstream.parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let upstream = connect_mock(options).await;

	let origin = produce_origin(3);
	let mut options = MockConnectOptions::new(downstream.parse::<Version>().unwrap());
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(origin.clone());
	let downstream = connect_mock(options).await;

	let consumer = origin.consume();
	let remote = consumer.follow("bcast", None).unwrap().available().await.unwrap();
	let subscriber = remote.track("data").unwrap().subscribe(subscription).await.unwrap();
	support::harness::subscribed(&track).await;

	Fixture {
		track,
		subscriber,
		_broadcast: broadcast,
		_hops: vec![upstream, downstream],
		_origins: vec![publisher, relay, origin],
	}
}

/// Hops that carry a start group and negotiate compression: lite-07 and moq-transport.
const RANGED: &[&str] = &["moq-lite-07-wip", "moq-transport-18", "moq-transport-22"];

/// A subscription starting at a group delivers it whole from its first frame across every
/// pair of hops, compressed or not, whether the group began before it subscribed or after.
#[kio::test]
async fn a_start_group_arrives_whole() {
	use moq_net::track::{Anchor, Subscription};
	let write = |group: &mut moq_net::group::Producer, frames: std::ops::Range<u64>| {
		for index in frames {
			group
				.write_frame(Timestamp::from_millis(index).unwrap(), payload(index))
				.unwrap();
		}
	};
	for &upstream in RANGED {
		for &downstream in RANGED {
			let hops = [upstream, downstream];
			let lite = hops.iter().all(|hop| *hop == "moq-lite-07-wip");
			for compress in [true, false] {
				// Frames of the group written before the subscription, which a joining FETCH
				// or fill delivers.
				for head in [0, 3] {
					let case = format!("{upstream} -> {downstream}, compress {compress}, head {head}");
					kio::clock::timeout(TIMEOUT, async {
						let info = match compress {
							true => compressed(),
							false => moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI),
						};
						let published = publish(info);
						let mut group = published
							.track
							.create_group(moq_net::group::Info { sequence: 5 })
							.unwrap();
						write(&mut group, 0..head);
						let mut fixture = relay(published, hops, Subscription::default().with_range(5..)).await;
						if lite {
							assert_eq!(
								fixture.track.subscription_changed().await.unwrap().unwrap().ranges[0].start,
								Anchor::Absolute(5),
								"{case}: upstream asks for the start group"
							);
						}
						write(&mut group, head..5);
						group.finish().unwrap();
						let mut reading = fixture.subscriber.recv_group().await.unwrap().unwrap();
						assert_eq!(reading.sequence, 5, "{case}");
						for index in 0..5 {
							assert_eq!(
								reading.read_frame().await.unwrap().unwrap().payload,
								payload(index),
								"{case}"
							);
						}
						assert!(reading.read_frame().await.unwrap().is_none(), "{case}");
					})
					.await
					.unwrap_or_else(|_| panic!("{case}: the group never arrived whole"));
				}
			}
		}
	}
}

/// Whole and chunked frames on a compressed track read back as written on every version,
/// and only a peer that can signal it learns the track is compressed.
#[kio::test]
async fn relay_serves_every_version() {
	for &version in VERSIONS {
		kio::clock::timeout(TIMEOUT, async {
			let mut fixture = connect("moq-lite-07-wip", version).await;
			assert_eq!(
				fixture.subscriber.info().compress,
				!matches!(version, "moq-lite-06" | "moq-lite-03"),
				"{version}: compression declared"
			);

			let mut group = fixture.track.append_group().unwrap();
			group
				.write_frame(Timestamp::from_millis(0).unwrap(), payload(0))
				.unwrap();
			group
				.write_frame(Timestamp::from_millis(1).unwrap(), payload(1))
				.unwrap();
			// A chunked frame is compressed whole once its last chunk lands.
			let chunked = payload(2);
			let mut frame = group
				.create_frame(moq_net::frame::Info {
					size: chunked.len() as u64,
					timestamp: Some(Timestamp::from_millis(2).unwrap()),
				})
				.unwrap();
			for chunk in chunked.chunks(100) {
				frame.write(bytes::Bytes::copy_from_slice(chunk)).unwrap();
			}
			let mut group = frame.finish().unwrap();
			group
				.write_frame(Timestamp::from_millis(3).unwrap(), bytes::Bytes::new())
				.unwrap();
			group.finish().unwrap();

			let mut group = fixture.subscriber.recv_group().await.unwrap().expect("group");
			for index in 0..3 {
				let frame = group.read_frame().await.unwrap().expect("frame");
				assert_eq!(&frame.payload[..], &payload(index)[..], "{version}: frame {index}");
			}
			assert!(group.read_frame().await.unwrap().unwrap().payload.is_empty());
			assert!(group.read_frame().await.unwrap().is_none(), "{version}: extra frame");
		})
		.await
		.unwrap_or_else(|_| panic!("{version}: timed out"));
	}
}

/// A compressed track's datagrams read back as written, whether the subscriber takes them
/// compressed or inflated.
#[kio::test]
async fn datagrams_serve_every_version() {
	for &version in &["moq-lite-07-wip", "moq-lite-06", "moq-transport-18", "moq-transport-22"] {
		kio::clock::timeout(TIMEOUT, async {
			let mut fixture = connect("moq-lite-07-wip", version).await;
			fixture
				.track
				.insert_datagram(0, Timestamp::from_millis(0).unwrap(), payload(0))
				.unwrap();
			let datagram = fixture.subscriber.recv_datagram().await.unwrap().expect("datagram");
			assert_eq!(&datagram.payload[..], &payload(0)[..], "{version}");
		})
		.await
		.unwrap_or_else(|_| panic!("{version}: timed out"));
	}
}

/// What the sessions under `registry` metered as `role`.
fn traffic(registry: &moq_net::stats::Registry, role: moq_net::stats::Role) -> moq_net::stats::Traffic {
	registry
		.snapshot()
		.traffic()
		.into_iter()
		.find(|(_, row, _)| *row == role)
		.map(|(_, _, traffic)| traffic)
		.unwrap_or_default()
}

/// A relay meters a compressed track's datagram in the form each wire carries: compressed
/// on ingress and on moq-lite 07 egress, inflated to an older subscriber. Only `inflated`
/// counts the bytes the relay inflated.
#[kio::test]
async fn datagrams_meter_the_bytes_on_the_wire() {
	for &version in &["moq-lite-07-wip", "moq-lite-06"] {
		kio::clock::timeout(TIMEOUT, async {
			let publisher = produce_origin(1);
			let broadcast = publisher.create_broadcast("bcast").unwrap();
			let mut track = broadcast.create_track("data", compressed()).unwrap();
			broadcast.announce(Default::default()).unwrap();

			// The relay accepts both hops, so its sessions are the ones metered.
			let relay = produce_origin(2);
			let ingress = moq_net::stats::Registry::new(Default::default());
			let mut options = MockConnectOptions::new("moq-lite-07-wip".parse::<Version>().unwrap());
			options.client_publish = Some(publisher.consume());
			options.server_subscribe = Some(relay.clone());
			options.server_stats = ingress
				.tier(Default::default())
				.session(moq_net::Path::from_static("ingress"));
			let upstream = connect_mock(options).await;

			let origin = produce_origin(3);
			let egress = moq_net::stats::Registry::new(Default::default());
			let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
			options.server_publish = Some(relay.consume());
			options.client_subscribe = Some(origin.clone());
			options.server_stats = egress
				.tier(Default::default())
				.session(moq_net::Path::from_static("egress"));
			let downstream = connect_mock(options).await;

			let consumer = origin.consume();
			let remote = consumer.follow("bcast", None).unwrap().available().await.unwrap();
			let mut subscriber = remote.track("data").unwrap().subscribe(None).await.unwrap();
			support::harness::subscribed(&track).await;

			track
				.insert_datagram(0, Timestamp::from_millis(0).unwrap(), payload(0))
				.unwrap();
			let datagram = subscriber.recv_datagram().await.unwrap().expect("datagram");
			assert_eq!(&datagram.payload[..], &payload(0)[..], "{version}");

			let plain = payload(0).len() as u64;
			let received = traffic(&ingress, moq_net::stats::Role::Subscriber);
			assert_eq!(received.datagrams, 1, "{version}: ingress");
			assert!(received.bytes < plain, "{version}: ingress metered {}", received.bytes);
			assert_eq!(received.inflated, plain, "{version}: the relay inflated it once");

			let sent = traffic(&egress, moq_net::stats::Role::Publisher);
			assert_eq!(sent.datagrams, 1, "{version}: egress");
			let expected = match version {
				"moq-lite-07-wip" => received.bytes,
				_ => plain,
			};
			assert_eq!(sent.bytes, expected, "{version}: egress metered as sent");
			assert_eq!(sent.inflated, 0, "{version}: egress inflated nothing");
			drop((upstream, downstream, publisher, relay, origin));
		})
		.await
		.unwrap_or_else(|_| panic!("{version}: timed out"));
	}
}

/// Negotiated IETF ingress stays compressed across a relay to IETF or lite-07 egress.
#[kio::test]
async fn ietf_ingress_stays_compressed_across_protocols() {
	for upstream in ["moq-transport-18", "moq-transport-22"] {
		for downstream in ["moq-lite-07-wip", "moq-transport-22"] {
			kio::clock::timeout(TIMEOUT, async {
				let mut fixture = connect(upstream, downstream).await;
				assert!(fixture.subscriber.info().compress, "{upstream} -> {downstream}");
				let mut group = fixture.track.append_group().unwrap();
				for index in 0..3 {
					group
						.write_frame(Timestamp::from_millis(index).unwrap(), payload(index))
						.unwrap();
				}
				group.finish().unwrap();
				let mut received = fixture.subscriber.recv_group().await.unwrap().unwrap();
				for index in 0..3 {
					assert_eq!(
						received.read_frame().await.unwrap().unwrap().payload.as_ref(),
						payload(index)
					);
				}
				assert!(received.read_frame().await.unwrap().is_none());
				fixture
					.track
					.insert_datagram(1, Timestamp::from_millis(3).unwrap(), payload(3))
					.unwrap();
				assert_eq!(
					fixture
						.subscriber
						.recv_datagram()
						.await
						.unwrap()
						.unwrap()
						.payload
						.as_ref(),
					payload(3)
				);
			})
			.await
			.unwrap_or_else(|_| panic!("{upstream} -> {downstream}: timed out"));
		}
	}
}

/// A finished compressed track remains fetchable without subscribing to its live feed.
#[kio::test]
async fn ietf_fetch_keeps_the_stored_form() {
	for version in ["moq-transport-18", "moq-transport-22"] {
		kio::clock::timeout(TIMEOUT, async {
			let publisher = produce_origin(1);
			let broadcast = publisher.create_broadcast("bcast").unwrap();
			let track = broadcast.create_track("data", compressed()).unwrap();
			let mut group = track.append_group().unwrap();
			for index in 0..3 {
				group
					.write_frame(Timestamp::from_millis(index).unwrap(), payload(index))
					.unwrap();
			}
			group.finish().unwrap();
			track.finish().unwrap();
			broadcast.announce(Default::default()).unwrap();
			let origin = produce_origin(2);
			let mut options = MockConnectOptions::new(version.parse().unwrap());
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(origin.clone());
			let _pair = connect_mock(options).await;
			let remote = origin
				.consume()
				.follow("bcast", None)
				.unwrap()
				.available()
				.await
				.unwrap();
			let consumer = remote.track("data").unwrap();
			assert!(consumer.info().await.unwrap().compress, "{version}");
			let mut fetched = consumer.fetch_group(0, moq_net::group::Fetch::default()).await.unwrap();
			for index in 0..3 {
				assert_eq!(
					fetched.read_frame().await.unwrap().unwrap().payload.as_ref(),
					payload(index)
				);
			}
			assert!(fetched.read_frame().await.unwrap().is_none());
		})
		.await
		.unwrap_or_else(|_| panic!("{version}: timed out"));
	}
}
