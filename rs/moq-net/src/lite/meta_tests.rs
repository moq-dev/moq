//! A metadata subscription crosses each relay hop as a metadata subscription where the hop
//! carries the mode (lite-07), so no payload crosses a hop no reader needs it on, and as a
//! payload subscription whose payloads the relay drops only where it does not.
//!
//! Read through the public [`crate::meta`] API. Deterministic: simulated time over the in-memory
//! transport. Payload is measured as each publishing session bills it, which counts payload
//! bytes and frames sent and nothing for a metadata FRAME.

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

/// The track subscriptions `registry`'s sessions served as publisher: started, and still open.
fn subscriptions(registry: &stats::Registry) -> (u64, u64) {
	registry
		.snapshot()
		.traffic()
		.into_iter()
		.filter(|(_, role, _)| *role == stats::Role::Publisher)
		.fold((0, 0), |(started, open), (_, _, traffic)| {
			(
				started + traffic.subscriptions_started,
				open + traffic.subscriptions_started - traffic.subscriptions_ended,
			)
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
	chain_with(upstream, downstream, timed()).await
}

async fn chain_with(upstream: &str, downstream: &str, info: crate::track::Info) -> Chain {
	let publisher = origin(1);
	let broadcast = publisher.route("bcast", crate::origin::RouteOptions::hidden()).unwrap();
	let track = broadcast.broadcast().create_track("video", info).unwrap();
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
async fn read_headers(sub: &mut crate::meta::Subscriber) -> (u64, Vec<(u64, u64)>) {
	let mut group = recv(sub).await;
	let mut frames = Vec::new();
	while let Some(header) = kio::clock::timeout(TIMEOUT, group.next_header())
		.await
		.expect("header timeout")
		.expect("header")
	{
		frames.push((header.size, header.timestamp.as_millis().unwrap() as u64));
	}
	(group.start, frames)
}

async fn recv(sub: &mut crate::meta::Subscriber) -> crate::meta::Group {
	kio::clock::timeout(TIMEOUT, sub.recv_group())
		.await
		.expect("recv timeout")
		.expect("recv")
		.expect("the track ended")
}

async fn meta(
	broadcast: &crate::broadcast::Consumer,
	subscription: impl Into<Option<crate::track::Subscription>>,
) -> crate::meta::Subscriber {
	kio::clock::timeout(TIMEOUT, broadcast.meta("video").unwrap().subscribe(subscription))
		.await
		.expect("subscribe timeout")
		.expect("subscribe")
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
	let mut sub = meta(&chain.remote, None).await;
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
	let mut sub = meta(&chain.remote, None).await;
	assert_eq!(read_headers(&mut sub).await, (0, vec![(BIG as u64, 0), (0, 1)]));
	assert_eq!(
		sent(&chain.upstream),
		(BIG as u64, 2),
		"the older hop carried the payload"
	);
	assert_eq!(sent(&chain.downstream), (0, 0), "the relay sent payload");
}

/// Over a lite-06 hop, which cannot carry the mode, a payload reader and a metadata reader of
/// one track share the one payload subscription and copy: the payload crosses that hop once.
#[kio::test]
async fn metadata_shares_the_payload_subscription_across_an_older_hop() {
	let chain = chain("moq-lite-06", "moq-lite-07-wip").await;
	let mut payload = kio::clock::timeout(TIMEOUT, chain.remote.track("video").unwrap().subscribe(None))
		.await
		.expect("subscribe timeout")
		.expect("subscribe");
	let mut meta = meta(&chain.remote, None).await;
	write_group(&chain.track, 0);

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
	settle().await;
	assert_eq!(
		subscriptions(&chain.upstream).0,
		1,
		"the older hop carried a second subscription"
	);
	assert_eq!(
		sent(&chain.upstream),
		(BIG as u64, 2),
		"the payload crossed the older hop twice"
	);
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
	let mut meta = meta(&chain.remote, None).await;

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

/// An end reader whose own session is older than lite-07 asks for metadata the way a relay does
/// over such a hop: one payload subscription, whose payload it drops. Only that hop's need pulls
/// the payload over the hop above it.
#[kio::test]
async fn metadata_over_an_older_session_reads_headers() {
	let chain = chain("moq-lite-07-wip", "moq-lite-06").await;
	write_group(&chain.track, 0);
	let mut sub = meta(&chain.remote, None).await;
	assert_eq!(read_headers(&mut sub).await, (0, vec![(BIG as u64, 0), (0, 1)]));
	settle().await;
	assert_eq!(
		sent(&chain.downstream),
		(BIG as u64, 2),
		"the older session carried the payload"
	);
	assert_eq!(subscriptions(&chain.downstream).0, 1);
}

/// A metadata reader that wants the payload after all opens a payload subscription at the group
/// it reached and skips to its first missing frame, while the metadata subscription reads on.
#[kio::test]
async fn payload_subscription_continues_from_a_metadata_position() {
	let chain = chain("moq-lite-07-wip", "moq-lite-07-wip").await;
	let mut sub = meta(&chain.remote, None).await;
	let mut group = chain.track.append_next().unwrap();
	for at in 0..2 {
		group
			.write_frame(Timestamp::from_millis(at).unwrap(), vec![b'a' + at as u8; BIG])
			.unwrap();
	}

	let mut headers = recv(&mut sub).await;
	for _ in 0..2 {
		kio::clock::timeout(TIMEOUT, headers.next_header())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
	}
	let (key, next) = (headers.start, headers.index());
	assert_eq!((key, next), (0, 2));
	assert_eq!(sent(&chain.upstream), (0, 0), "metadata pulled payload");

	let mut payload = kio::clock::timeout(
		TIMEOUT,
		chain.remote.track("video").unwrap().subscribe(
			crate::track::Subscription::default()
				.with_ranges([crate::track::Range::from(crate::track::Start::Time(key))]),
		),
	)
	.await
	.expect("subscribe timeout")
	.expect("subscribe");
	group
		.write_frame(Timestamp::from_millis(2).unwrap(), vec![b'c'; BIG])
		.unwrap();
	group.finish().unwrap();

	let mut reading = kio::clock::timeout(TIMEOUT, payload.recv_group())
		.await
		.expect("recv timeout")
		.expect("recv")
		.expect("the track ended");
	assert_eq!(reading.start, key);
	reading.skip_to(next);
	let frame = kio::clock::timeout(TIMEOUT, reading.read_frame())
		.await
		.expect("frame timeout")
		.expect("frame")
		.expect("a frame");
	assert_eq!(
		frame.payload,
		vec![b'c'; BIG],
		"the payload reader resumes at the first missing frame"
	);

	let header = kio::clock::timeout(TIMEOUT, headers.next_header())
		.await
		.unwrap()
		.unwrap()
		.unwrap();
	assert_eq!((header.size, header.timestamp.as_millis().unwrap()), (BIG as u64, 2));
	assert!(
		kio::clock::timeout(TIMEOUT, headers.next_header())
			.await
			.unwrap()
			.unwrap()
			.is_none()
	);
}

/// A history range while only metadata demand stands fills its groups as metadata: every header
/// arrives, oldest first, and no payload crosses either hop.
#[kio::test]
async fn metadata_history_fill_carries_no_payload() {
	let info = timed().with_max_age(Duration::from_secs(60));
	let chain = chain_with("moq-lite-07-wip", "moq-lite-07-wip", info).await;
	for at in [0, 10, 20] {
		write_group(&chain.track, at);
	}
	let mut sub = meta(
		&chain.remote,
		crate::track::Subscription::default()
			.with_ranges([crate::track::Range::from(crate::track::Start::Oldest)])
			.with_order(crate::track::Order::Ascending),
	)
	.await;
	for (key, at) in [(0, 0), (1, 10), (2, 20)] {
		assert_eq!(read_headers(&mut sub).await, (key, vec![(BIG as u64, at), (0, at + 1)]));
	}
	assert_eq!(sent(&chain.upstream), (0, 0), "the publisher sent payload");
	assert_eq!(sent(&chain.downstream), (0, 0), "the relay sent payload");
}

/// A compressed track's headers name each frame's plain size, read from a local payload copy
/// and from a lite-07 metadata copy alike.
#[kio::test]
async fn metadata_names_a_compressed_frame_by_its_plain_size() {
	let info = timed().with_compression(crate::track::Compression::Deflate);
	let chain = chain_with("moq-lite-07-wip", "moq-lite-07-wip", info.clone()).await;
	let local = crate::broadcast::Info::new().produce();
	let track = local.create_track("video", info).unwrap();
	let mut remote = meta(&chain.remote, None).await;
	let mut near = meta(&local.consume(), None).await;
	for producer in [&chain.track, &track] {
		let mut group = producer.append_next().unwrap();
		group
			.write_frame(Timestamp::from_millis(0).unwrap(), vec![b'z'; BIG])
			.unwrap();
		group.finish().unwrap();
	}
	assert_eq!(read_headers(&mut remote).await, (0, vec![(BIG as u64, 0)]));
	assert_eq!(read_headers(&mut near).await, (0, vec![(BIG as u64, 0)]));
	drop(track.finish());
}

/// Dropping the last metadata reader ends its subscription on every hop.
#[kio::test]
async fn dropping_a_metadata_reader_closes_its_subscriptions() {
	let chain = chain("moq-lite-07-wip", "moq-lite-07-wip").await;
	write_group(&chain.track, 0);
	let mut sub = meta(&chain.remote, None).await;
	read_headers(&mut sub).await;
	assert_eq!(subscriptions(&chain.upstream).1, 1);
	assert_eq!(subscriptions(&chain.downstream).1, 1);

	drop(sub);
	settle().await;
	assert_eq!(
		subscriptions(&chain.downstream).1,
		0,
		"the relay still serves the reader"
	);
	assert_eq!(
		subscriptions(&chain.upstream).1,
		0,
		"the publisher still serves the relay"
	);
}
