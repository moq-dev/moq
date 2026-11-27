//! A metadata subscription crosses each relay hop as a metadata subscription where the hop
//! carries the mode (lite-07), so no payload crosses a hop no reader needs it on, and as a
//! payload subscription whose payloads the relay drops only where it does not.
//!
//! Deterministic: simulated time over the in-memory transport. Payload is measured as each
//! publishing session bills it, which counts payload bytes and frames sent and nothing for a
//! metadata FRAME.

use std::time::Duration;

use crate::model::ProduceTest;
use crate::{Timestamp, Version, stats};

const TIMEOUT: Duration = Duration::from_secs(10);

/// A frame big enough that its payload crossing a hop shows in the bill.
const BIG: usize = 1000;

fn timed() -> crate::track::Info {
	crate::track::Info::default().with_timescale(crate::Timescale::MILLI)
}

fn origin(hop: u64) -> crate::origin::Producer {
	let mut config = crate::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.proxy = true;
	config.produce()
}

/// The payload bytes and frames `registry`'s sessions sent as publisher.
fn sent(registry: &stats::Registry) -> (u64, u64) {
	registry
		.snapshot()
		.traffic()
		.into_iter()
		.filter(|(_, role, _)| *role == stats::Role::Publisher)
		.fold((0, 0), |(bytes, frames), (_, _, traffic)| {
			(bytes + traffic.bytes, frames + traffic.frames)
		})
}

/// Connect a client publishing `publish` to a server subscribing into `subscribe` over
/// `version`, billing what the client sends to `registry`.
async fn hop(
	version: Version,
	publish: &crate::origin::Producer,
	subscribe: &crate::origin::Producer,
	registry: &stats::Registry,
) -> (crate::Session, crate::Session) {
	let (client, server) = crate::transport::memory::pair(crate::transport::memory::Config {
		protocol: Some(version.alpn().to_string()),
		..Default::default()
	});
	let billed = registry
		.tier(stats::Tier::default())
		.session(stats::SessionInfo::new(crate::Path::from_static("")));
	let dial = crate::Client::new()
		.with_versions(version.into())
		.with_publisher(publish.consume())
		.with_stats(billed);
	let serve = crate::Server::new()
		.with_versions(version.into())
		.with_subscriber(subscribe.clone());
	futures::join!(
		async {
			let (session, driver) = dial.connect(kio::clock::now(), client).await.unwrap();
			drop(kio::clock::sim::spawn(crate::time::run(driver)));
			session
		},
		async {
			let (session, driver) = serve.accept(kio::clock::now(), server).await.unwrap();
			drop(kio::clock::sim::spawn(crate::time::run(driver)));
			session
		},
	)
}

/// A publisher, a relay, and an end reader, joined by two hops.
struct Chain {
	track: crate::track::Producer,
	/// The broadcast as the end reader resolves it, through the relay.
	remote: crate::broadcast::Consumer,
	/// What the publisher's session billed for sending to the relay.
	upstream: stats::Registry,
	/// What the relay's session billed for sending to the end reader.
	downstream: stats::Registry,
	_keep: Vec<Box<dyn std::any::Any>>,
}

async fn chain(upstream: &str, downstream: &str) -> Chain {
	let publisher = origin(1);
	let broadcast = publisher.route("bcast", crate::origin::RouteOptions::hidden()).unwrap();
	let track = broadcast.broadcast().create_track("video", timed()).unwrap();
	broadcast.announce().unwrap();

	let relay = origin(2);
	let end = origin(3);
	let (upstream_bill, downstream_bill) = (
		stats::Registry::new(Default::default()),
		stats::Registry::new(Default::default()),
	);
	let first = hop(upstream.parse().unwrap(), &publisher, &relay, &upstream_bill).await;
	let second = hop(downstream.parse().unwrap(), &relay, &end, &downstream_bill).await;

	let remote = kio::clock::timeout(TIMEOUT, end.consume().follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
		.unwrap();
	Chain {
		track,
		remote,
		upstream: upstream_bill,
		downstream: downstream_bill,
		_keep: vec![
			Box::new(broadcast),
			Box::new(publisher),
			Box::new(relay),
			Box::new(end),
			Box::new(first),
			Box::new(second),
		],
	}
}

/// Write a group of a big frame at `at` ms and an empty one after it.
fn write_group(track: &crate::track::Producer, at: u64) {
	let mut group = track.append_next().unwrap();
	group
		.write_frame(Timestamp::from_millis(at).unwrap(), vec![b'x'; BIG])
		.unwrap();
	group
		.write_frame(Timestamp::from_millis(at + 1).unwrap(), b"".as_slice())
		.unwrap();
	group.finish().unwrap();
}

/// The next group's key and each frame's declared size and timestamp in ms, read as a
/// metadata reader reads them: headers alone.
async fn read_headers(sub: &mut crate::track::Subscriber) -> (u64, Vec<(u64, u64)>) {
	let mut group = kio::clock::timeout(TIMEOUT, sub.recv_group())
		.await
		.expect("recv timeout")
		.expect("recv")
		.expect("the track ended");
	let mut frames = Vec::new();
	while let Some(frame) = kio::clock::timeout(TIMEOUT, group.next_frame())
		.await
		.expect("frame timeout")
		.expect("frame")
	{
		let size = frame.inflated.unwrap_or(frame.size);
		frames.push((size, frame.timestamp.as_millis().unwrap() as u64));
	}
	(group.start, frames)
}

/// Let every session settle what the last change asked of it.
async fn settle() {
	kio::clock::sleep(Duration::from_secs(1)).await;
}

/// Over lite-07 hops a metadata reader's demand travels upstream as a metadata subscription:
/// the reader gets every header, and neither hop carries a payload.
#[kio::test]
async fn metadata_crosses_lite07_hops_without_payload() {
	let chain = chain("moq-lite-07-wip", "moq-lite-07-wip").await;
	write_group(&chain.track, 0);
	let mut sub = kio::clock::timeout(TIMEOUT, chain.remote.track_meta("video").unwrap().subscribe(None))
		.await
		.expect("subscribe timeout")
		.expect("subscribe");
	assert_eq!(read_headers(&mut sub).await, (0, vec![(BIG as u64, 0), (0, 1)]));

	write_group(&chain.track, 10);
	assert_eq!(read_headers(&mut sub).await, (1, vec![(BIG as u64, 10), (0, 11)]));
	assert_eq!(sent(&chain.upstream), (0, 0), "the publisher sent payload");
	assert_eq!(sent(&chain.downstream), (0, 0), "the relay sent payload");
}

/// Below a lite-06 hop, which cannot carry the mode, the relay pulls the payload across that
/// hop alone and serves its lite-07 reader the headers.
#[kio::test]
async fn metadata_pulls_payload_only_across_an_older_hop() {
	let chain = chain("moq-lite-06", "moq-lite-07-wip").await;
	write_group(&chain.track, 0);
	let mut sub = kio::clock::timeout(TIMEOUT, chain.remote.track_meta("video").unwrap().subscribe(None))
		.await
		.expect("subscribe timeout")
		.expect("subscribe");
	assert_eq!(read_headers(&mut sub).await, (0, vec![(BIG as u64, 0), (0, 1)]));
	assert_eq!(
		sent(&chain.upstream),
		(BIG as u64, 2),
		"the older hop carried the payload"
	);
	assert_eq!(sent(&chain.downstream), (0, 0), "the relay sent payload");
}

/// A payload reader and a metadata reader of one track each get their own: the payload goes to
/// the payload reader alone, and once it leaves, the metadata reader still gets every header
/// while no payload crosses any hop.
#[kio::test]
async fn metadata_outlives_the_payload_reader_without_payload() {
	let chain = chain("moq-lite-07-wip", "moq-lite-07-wip").await;
	write_group(&chain.track, 0);
	let mut payload = kio::clock::timeout(TIMEOUT, chain.remote.track("video").unwrap().subscribe(None))
		.await
		.expect("subscribe timeout")
		.expect("subscribe");
	let mut meta = kio::clock::timeout(TIMEOUT, chain.remote.track_meta("video").unwrap().subscribe(None))
		.await
		.expect("subscribe timeout")
		.expect("subscribe");

	let mut group = kio::clock::timeout(TIMEOUT, payload.recv_group())
		.await
		.expect("recv timeout")
		.expect("recv")
		.expect("the track ended");
	let frame = kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("frame timeout")
		.expect("frame")
		.expect("a frame");
	assert_eq!(frame.payload.len(), BIG, "the payload reader gets the payload");
	assert_eq!(read_headers(&mut meta).await, (0, vec![(BIG as u64, 0), (0, 1)]));
	assert_eq!(sent(&chain.upstream).0, BIG as u64, "the payload crossed once");

	drop((group, payload));
	settle().await;
	let before = (sent(&chain.upstream), sent(&chain.downstream));
	write_group(&chain.track, 10);
	assert_eq!(read_headers(&mut meta).await, (1, vec![(BIG as u64, 10), (0, 11)]));
	settle().await;
	assert_eq!(
		(sent(&chain.upstream), sent(&chain.downstream)),
		before,
		"payload crossed with no payload reader"
	);
}
