#![cfg(feature = "async")]

//! An untimed track reaches every subscriber untimed, through a relay too, on every wire
//! that can say so, and a timed track keeps its timestamps.
//!
//! No receiver fills in arrival time. Lite-05 and lite-06 cannot encode absence, so their
//! encoder writes each frame's group key instead, which their receivers pass through as the
//! track's time.

use crate::support;
use support::AppendNext;

use std::time::Duration;

use bytes::Bytes;
use moq_net::{Timescale, Timestamp, Version, datagram, origin, track};
use support::harness::peer;

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

const TIMEOUT: Duration = Duration::from_secs(10);

/// What a subscriber reads for a timed and an untimed track.
#[derive(Debug, PartialEq, Eq)]
enum Arrives {
	/// Both keep what the publisher wrote.
	Faithful,
	/// The timed track keeps its timestamps, but the untimed one arrives timed in
	/// milliseconds with its group keys.
	Keys,
	/// The wire carries no timescale, so even the timed track arrives untimed.
	Untimed,
}

fn expected(version: &str) -> Arrives {
	match version {
		"moq-lite-03" | "moq-lite-04" => Arrives::Untimed,
		"moq-lite-05" | "moq-lite-06" => Arrives::Keys,
		_ => Arrives::Faithful,
	}
}

const VERSIONS: &[&str] = &[
	"moq-lite-03",
	"moq-lite-04",
	"moq-lite-05",
	"moq-lite-06",
	"moq-lite-07-wip",
	"moq-transport-18",
	"moq-transport-22",
];

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer.with_proxy(true)
}

fn millis(ms: u64) -> Timestamp {
	Timestamp::from_millis(ms).unwrap()
}

/// An untimed track with no publisher max age, so superseded groups stay readable.
fn untimed() -> track::Info {
	track::Info::default().with_timescale(None).with_max_age(None)
}

/// Origins chained by one session per hop, each hop on its own version.
struct Chain {
	nodes: Vec<origin::Producer>,
	_pairs: Vec<support::harness::MockPair>,
}

impl Chain {
	async fn new(hops: &[&str]) -> Self {
		let nodes: Vec<_> = (1..=hops.len() as u64 + 1).map(produce_origin).collect();
		let mut pairs = Vec::new();
		for (pair, version) in nodes.windows(2).zip(hops) {
			pairs.push(peer(version.parse::<Version>().unwrap(), &pair[0], &pair[1]).await);
		}
		Self { nodes, _pairs: pairs }
	}

	async fn subscribe(&self, track: &str) -> track::Subscriber {
		let consumer = self.nodes.last().unwrap().consume();
		let broadcast = kio::clock::timeout(TIMEOUT, consumer.request("room", None))
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves");
		kio::clock::timeout(TIMEOUT, broadcast.track(track).unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.expect("subscribe")
	}
}

/// The first frame's timestamp of the next group.
async fn next_frame(subscriber: &mut track::Subscriber) -> Timestamp {
	let mut group = kio::clock::timeout(TIMEOUT, subscriber.recv_group())
		.await
		.expect("group timeout")
		.unwrap()
		.expect("a group");
	group.read_frame().await.unwrap().expect("a frame").timestamp
}

/// Publish a single-frame group at key 7 on a track with `info` through `hops`, returning
/// what the last node reads for it and the track info it received.
async fn frame_through(hops: &[&str], info: track::Info) -> (Timestamp, track::Info) {
	let timed = info.timescale.is_some();
	let chain = Chain::new(hops).await;
	let broadcast = chain.nodes[0]
		.route("room", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let mut producer = broadcast.broadcast().create_track("data", info).unwrap();
	broadcast.announce().unwrap();
	kio::clock::sleep(Duration::from_secs(1)).await;

	let mut subscriber = chain.subscribe("data").await;
	match timed {
		true => producer.write_frame(millis(7), &b"frame"[..]).unwrap(),
		false => {
			let mut group = producer.append_group(7).unwrap();
			group
				.write_frame(moq_net::Timestamp::unscaled(group.start), &b"frame"[..])
				.unwrap();
			group.close().unwrap();
		}
	}
	let received = next_frame(&mut subscriber).await;
	let received = match received.scale() {
		Some(_) => received.convert(Timescale::MILLI).unwrap(),
		None => received,
	};
	(received, subscriber.info().clone())
}

/// The key [`frame_through`] publishes its group at, which a frame with no time carries.
const KEY: moq_net::Timestamp = moq_net::Timestamp::unscaled(7);

#[kio::test]
async fn frames_arrive_as_published() {
	for version in VERSIONS {
		let (timed, timed_info) = frame_through(&[version], timed()).await;
		let (untimed, untimed_info) = frame_through(&[version], untimed()).await;
		match expected(version) {
			Arrives::Faithful => {
				assert_eq!(timed, millis(7), "{version}: the timestamp survives");
				assert_eq!(untimed, KEY, "{version}: absence survives");
				assert_eq!(untimed_info.timescale, None, "{version}: no timeline is declared");
			}
			Arrives::Keys => {
				assert_eq!(timed, millis(7), "{version}: the timestamp survives");
				assert_eq!(untimed, millis(7), "{version}: the group key arrives");
				assert_eq!(untimed_info.timescale, Some(Timescale::MILLI), "{version}");
			}
			Arrives::Untimed => {
				assert_eq!(timed, KEY, "{version}: no units, so no timestamp");
				assert_eq!(timed_info.timescale, None, "{version}: no timeline is declared");
				assert_eq!(untimed, KEY, "{version}: absence survives");
			}
		}
	}
}

/// A relay forwards an untimed track untimed, and a track it received without a timeline
/// does not gain one downstream.
#[kio::test]
async fn a_relay_forwards_an_untimed_track_untimed() {
	for hops in [
		&["moq-transport-18", "moq-transport-22"][..],
		&["moq-lite-07-wip", "moq-lite-07-wip"][..],
	] {
		let (frame, info) = frame_through(hops, untimed()).await;
		assert_eq!(frame, KEY, "{hops:?}: the relay did not stamp it");
		assert_eq!(info.timescale, None, "{hops:?}: the relay must not claim a timeline");
	}

	let hops = ["moq-lite-03", "moq-transport-18"];
	let (frame, info) = frame_through(&hops, timed()).await;
	assert_eq!(frame, KEY, "{hops:?}: the first hop has no timestamps to forward");
	assert_eq!(info.timescale, None, "{hops:?}: the relay must not claim a timeline");
}

/// An explicit start on an untimed track is honored the same way on lite and
/// moq-transport: only an unfloored subscriber jumps to the latest group.
///
/// The subscription asks for history the way a resume does, with a budget covering it.
/// moq-transport before draft 22 can only join a past group through a joining FETCH,
/// which our publisher refuses for groups before the live one, so draft 22 stands in for it.
#[kio::test]
async fn an_explicit_start_holds_on_an_untimed_track() {
	for version in ["moq-lite-03", "moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		let chain = Chain::new(&[version]).await;
		let broadcast = chain.nodes[0]
			.route("room", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let producer = broadcast.broadcast().create_track("data", untimed()).unwrap();
		broadcast.announce().unwrap();
		for _ in 0..5 {
			let mut group = producer.append_next().unwrap();
			group
				.write_frame(moq_net::Timestamp::unscaled(group.start), &b"untimed"[..])
				.unwrap();
			group.close().unwrap();
		}
		kio::clock::sleep(Duration::from_secs(1)).await;

		let consumer = chain.nodes[1].consume().request("room", None).await.unwrap();
		let subscription = track::Subscription::default()
			.with_ranges([support::keys(2..)])
			.with_delay(track::Delay::default().with_max(Duration::from_secs(30)));
		let mut subscriber = kio::clock::timeout(TIMEOUT, consumer.track("data").unwrap().subscribe(subscription))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");

		let mut got = Vec::new();
		while got.len() < 3 {
			let Ok(next) = kio::clock::timeout(TIMEOUT, subscriber.recv_group()).await else {
				break;
			};
			got.push(next.unwrap().expect("a group").start);
		}
		got.sort();
		assert_eq!(got, vec![2, 3, 4], "{version}");
	}
}

/// An untimed datagram track's datagram arrives untimed, except that lite-05 and lite-06
/// carry its sequence on a millisecond track.
#[kio::test]
async fn datagrams_arrive_as_published() {
	for version in ["moq-lite-05", "moq-lite-07-wip"] {
		let chain = Chain::new(&[version]).await;
		let broadcast = chain.nodes[0]
			.route("room", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let mut producer = broadcast
			.broadcast()
			.create_datagrams("data", datagram::Info::default())
			.unwrap();
		broadcast.announce().unwrap();

		let consumer = kio::clock::timeout(TIMEOUT, chain.nodes[1].consume().request("room", None))
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves");
		let mut consumer = consumer.datagrams("data").unwrap();
		// Not the default, which the lite-05 TRACK lookup ahead of the SUBSCRIBE holds too.
		consumer.set_priority(1);
		kio::clock::timeout(TIMEOUT, support::datagram_priority(&producer, Some(1)))
			.await
			.expect("the request never reached the publisher");
		// Datagrams count once the answer is back.
		let timescale = consumer.info().await.unwrap().timescale;

		producer
			.append(moq_net::Timestamp::unscaled(0), &b"untimed"[..])
			.unwrap();
		let datagram = kio::clock::timeout(TIMEOUT, consumer.recv())
			.await
			.expect("datagram timeout")
			.unwrap()
			.expect("a datagram");
		let keyed = expected(version) == Arrives::Keys;
		// A scale-less datagram's time is its sequence; lite-05/06 declare milliseconds.
		let expected = match keyed {
			true => millis(datagram.sequence),
			false => moq_net::Timestamp::unscaled(datagram.sequence),
		};
		assert_eq!(datagram.timestamp, expected, "{version}");
		assert_eq!(timescale, keyed.then_some(Timescale::MILLI), "{version}");
	}
}

/// A standalone FETCH of an untimed track answers with untimed objects, except that lite-05
/// and lite-06 carry each one's group key.
#[kio::test]
async fn a_fetched_untimed_frame_arrives_untimed() {
	for version in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-18"] {
		let chain = Chain::new(&[version]).await;
		let broadcast = chain.nodes[0]
			.route("room", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let mut dynamic = broadcast.broadcast().requests();
		broadcast.announce().unwrap();
		kio::clock::sleep(Duration::from_secs(1)).await;

		let consumer = chain.nodes[1].consume().request("room", None).await.unwrap();
		let mut waiting = Box::pin(consumer.track("video").unwrap().fetch_group(3, None));
		let request = match futures::future::select(std::pin::pin!(dynamic.requested_track()), &mut waiting).await {
			futures::future::Either::Left((request, _)) => request.expect("the track request reaches the publisher"),
			futures::future::Either::Right(_) => panic!("nothing answered the track"),
		};
		let groups = request.requests();
		let _track = request.accept(untimed());
		let request = match futures::future::select(std::pin::pin!(groups.requested_fill()), &mut waiting).await {
			futures::future::Either::Left((request, _)) => request.expect("the fetch reaches the publisher"),
			futures::future::Either::Right(_) => panic!("nothing answered the fetch"),
		};
		let mut group = request.accept(3, timed()).unwrap();
		group
			.write_frame(
				moq_net::Timestamp::unscaled(group.start),
				Bytes::from_static(b"segment"),
			)
			.unwrap();
		group.close().unwrap();

		let mut fetched = waiting.await.unwrap();
		let frame = fetched.read_frame().await.unwrap().unwrap();
		let keyed = expected(version) == Arrives::Keys;
		let expected = match keyed {
			true => millis(3),
			false => moq_net::Timestamp::unscaled(3),
		};
		assert_eq!(frame.timestamp, expected, "{version}");
	}
}
