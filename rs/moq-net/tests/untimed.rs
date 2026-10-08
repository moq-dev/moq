//! An untimed track reaches every subscriber untimed, through a relay too, on every wire
//! that can say so, and a timed track keeps its timestamps.
//!
//! No receiver fills in arrival time. Until lite-07 encodes absence, a lite-05+ encoder
//! writes its send time instead, so those hops are the one place an untimed track gains a
//! timeline.

mod support;

use std::time::Duration;

use bytes::Bytes;
use moq_net::{Hop, Timescale, Timestamp, Version, origin, track};
use support::harness::peer;

const TIMEOUT: Duration = Duration::from_secs(10);

/// What a subscriber reads for a timed and an untimed track.
#[derive(Debug, PartialEq, Eq)]
enum Arrives {
	/// Both keep what the publisher wrote.
	Faithful,
	/// The untimed track carries the encoder's send times (lite-05+ before lite-07).
	SendTime,
	/// The wire declares no timescale, so even the timed track arrives untimed.
	Untimed,
}

fn expected(version: &str) -> Arrives {
	match version {
		"moq-lite-03" | "moq-lite-04" => Arrives::Untimed,
		"moq-lite-05" | "moq-lite-06" | "moq-lite-07-wip" => Arrives::SendTime,
		// Drafts 14 to 16 can't carry TIMESCALE.
		"moq-transport-14" | "moq-transport-16" => Arrives::Untimed,
		_ => Arrives::Faithful,
	}
}

const VERSIONS: &[&str] = &[
	"moq-lite-03",
	"moq-lite-04",
	"moq-lite-05",
	"moq-lite-06",
	"moq-lite-07-wip",
	"moq-transport-14",
	"moq-transport-16",
	"moq-transport-17",
	"moq-transport-20",
];

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

fn millis(ms: u64) -> Timestamp {
	Timestamp::from_millis(ms).unwrap()
}

fn untimed() -> track::Info {
	track::Info::default().with_timescale(None)
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
		let broadcast = moq_net_sim::timeout(TIMEOUT, consumer.request_broadcast("room"))
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves");
		moq_net_sim::timeout(TIMEOUT, broadcast.track(track).unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.expect("subscribe")
	}
}

/// The first frame's timestamp of the next group.
async fn next_frame(subscriber: &mut track::Subscriber) -> Option<Timestamp> {
	let mut group = moq_net_sim::timeout(TIMEOUT, subscriber.recv_group())
		.await
		.expect("group timeout")
		.unwrap()
		.expect("a group");
	group.read_frame().await.unwrap().expect("a frame").timestamp
}

/// Publish a single-frame group on a track with `info` through `hops`, returning what the
/// last node reads for it and the track info it received.
async fn frame_through(hops: &[&str], info: track::Info) -> (Option<Timestamp>, track::Info) {
	let timestamp = info.timescale.map(|_| millis(7));
	let chain = Chain::new(hops).await;
	let broadcast = chain.nodes[0].create_broadcast("room").unwrap();
	let mut producer = broadcast.create_track("data", info).unwrap();
	broadcast.announce(Default::default()).unwrap();
	moq_net_sim::sleep(Duration::from_secs(1)).await;

	let mut subscriber = chain.subscribe("data").await;
	producer.write_frame(timestamp, &b"frame"[..]).unwrap();
	let received = next_frame(&mut subscriber).await;
	let received = received.map(|timestamp| timestamp.convert(Timescale::MILLI).unwrap());
	(received, subscriber.info().clone())
}

#[moq_net_sim::test]
async fn frames_arrive_as_published() {
	for version in VERSIONS {
		let (timed, timed_info) = frame_through(&[version], track::Info::default()).await;
		let (untimed, untimed_info) = frame_through(&[version], untimed()).await;
		match expected(version) {
			Arrives::Faithful => {
				assert_eq!(timed, Some(millis(7)), "{version}: the timestamp survives");
				assert_eq!(untimed, None, "{version}: absence survives");
				assert_eq!(untimed_info.timescale, None, "{version}: no timeline is declared");
			}
			Arrives::SendTime => {
				assert_eq!(timed, Some(millis(7)), "{version}: the timestamp survives");
				assert!(untimed.is_some(), "{version}: a lite encoder writes its send time");
			}
			Arrives::Untimed => {
				assert_eq!(timed, None, "{version}: no units, so no timestamp");
				assert_eq!(timed_info.timescale, None, "{version}: no timeline is declared");
				assert_eq!(untimed, None, "{version}: absence survives");
			}
		}
	}
}

/// A relay forwards an untimed track untimed, and a track it received without a timeline
/// does not gain one downstream.
#[moq_net_sim::test]
async fn a_relay_forwards_an_untimed_track_untimed() {
	let (frame, info) = frame_through(&["moq-transport-17", "moq-transport-20"], untimed()).await;
	assert_eq!(frame, None, "the relay did not stamp it");
	assert_eq!(info.timescale, None, "the relay must not claim a timeline");

	for hops in [
		&["moq-lite-03", "moq-transport-17"][..],
		&["moq-transport-16", "moq-transport-20"][..],
	] {
		let (frame, info) = frame_through(hops, track::Info::default()).await;
		assert_eq!(frame, None, "{hops:?}: the first hop has no timestamps to forward");
		assert_eq!(info.timescale, None, "{hops:?}: the relay must not claim a timeline");
	}
}

/// An explicit start on an untimed track is honored the same way on lite and
/// moq-transport: only an unfloored subscriber jumps to the latest group.
///
/// The subscription asks for history the way a resume does, with a budget covering it:
/// lite-05+ carries send times, so downstream the groups are timed and a zero budget would
/// skip the older ones there, as it would on any timed track. moq-transport before draft
/// 20 can only join a past group through a joining FETCH, which our publisher refuses for
/// groups before the live one, so draft 20 stands in for it.
#[moq_net_sim::test]
async fn an_explicit_start_holds_on_an_untimed_track() {
	for version in ["moq-lite-03", "moq-lite-06", "moq-lite-07-wip", "moq-transport-20"] {
		let chain = Chain::new(&[version]).await;
		let broadcast = chain.nodes[0].create_broadcast("room").unwrap();
		let mut producer = broadcast.create_track("data", untimed()).unwrap();
		broadcast.announce(Default::default()).unwrap();
		for _ in 0..5 {
			producer.write_frame(None, &b"untimed"[..]).unwrap();
		}
		moq_net_sim::sleep(Duration::from_secs(1)).await;

		let consumer = chain.nodes[1].consume().request_broadcast("room").await.unwrap();
		let subscription = track::Subscription::default()
			.with_start(track::Position::group(2))
			.with_max_delay(Duration::from_secs(30));
		let mut subscriber = moq_net_sim::timeout(TIMEOUT, consumer.track("data").unwrap().subscribe(subscription))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");

		let mut got = Vec::new();
		while got.len() < 3 {
			let Ok(next) = moq_net_sim::timeout(TIMEOUT, subscriber.recv_group()).await else {
				break;
			};
			got.push(next.unwrap().expect("a group").sequence);
		}
		got.sort();
		assert_eq!(got, vec![2, 3, 4], "{version}");
	}
}

/// An untimed track's datagram arrives untimed on moq-transport, and with a send time on
/// lite.
#[moq_net_sim::test]
async fn datagrams_arrive_as_published() {
	for version in ["moq-lite-05", "moq-lite-07-wip", "moq-transport-16", "moq-transport-20"] {
		let chain = Chain::new(&[version]).await;
		let broadcast = chain.nodes[0].create_broadcast("room").unwrap();
		let mut producer = broadcast.create_track("data", untimed()).unwrap();
		broadcast.announce(Default::default()).unwrap();
		moq_net_sim::sleep(Duration::from_secs(1)).await;

		let mut subscriber = chain.subscribe("data").await;
		// A group first, so the subscription is bound before any datagram lands.
		producer.write_frame(None, &b"before"[..]).unwrap();
		next_frame(&mut subscriber).await;

		producer.append_datagram(None, &b"untimed"[..]).unwrap();
		let datagram = moq_net_sim::timeout(TIMEOUT, subscriber.recv_datagram())
			.await
			.expect("datagram timeout")
			.unwrap()
			.expect("a datagram");
		let lite = version.starts_with("moq-lite");
		assert_eq!(datagram.timestamp.is_some(), lite, "{version}");
	}
}

/// A standalone FETCH of an untimed track answers with untimed objects.
#[moq_net_sim::test]
async fn a_fetched_untimed_frame_arrives_untimed() {
	for version in ["moq-lite-06", "moq-transport-19"] {
		let chain = Chain::new(&[version]).await;
		let broadcast = chain.nodes[0].create_broadcast("room").unwrap();
		let mut dynamic = broadcast.dynamic();
		broadcast.announce(Default::default()).unwrap();
		moq_net_sim::sleep(Duration::from_secs(1)).await;

		let consumer = chain.nodes[1].consume().request_broadcast("room").await.unwrap();
		let mut waiting = Box::pin(consumer.track("video").unwrap().fetch_group(0, None));
		let request = match futures::future::select(std::pin::pin!(dynamic.requested_track()), &mut waiting).await {
			futures::future::Either::Left((request, _)) => request.expect("the track request reaches the publisher"),
			futures::future::Either::Right(_) => panic!("nothing answered the track"),
		};
		let groups = request.dynamic();
		let _track = request.accept(untimed());
		let request = match futures::future::select(std::pin::pin!(groups.requested_group()), &mut waiting).await {
			futures::future::Either::Left((request, _)) => request.expect("the fetch reaches the publisher"),
			futures::future::Either::Right(_) => panic!("nothing answered the fetch"),
		};
		let mut group = request.accept(None).unwrap();
		group.write_frame(None, Bytes::from_static(b"segment")).unwrap();
		group.finish().unwrap();

		let mut fetched = waiting.await.unwrap();
		let frame = fetched.read_frame().await.unwrap().unwrap();
		let lite = version.starts_with("moq-lite");
		assert_eq!(frame.timestamp.is_some(), lite, "{version}");
	}
}
