#![cfg(feature = "async")]

//! Datagram tracks end to end over the mock transport: what a publisher's datagram track
//! delivers to a remote consumer on each version, and what the consumer's priority and
//! demand do to the publisher.
//!
//! The mock delivers datagrams in order and loses none, so every wait is on an observable
//! event. A remote consumer takes datagrams once its request reached the publisher, which
//! sees it as a priority, and the answer came back, which the consumer sees as its info, so
//! each test waits for both before writing. A datagram and the track's end take different
//! paths, so each test reads its datagrams before ending the track.

use crate::support;

use std::time::Duration;

use moq_net::{Error, Hop, StreamError, Timescale, Timestamp, Version, broadcast, datagram, origin};
use support::datagram_priority;
use support::harness::{MockPair, peer_with_latency};

const TIMEOUT: Duration = Duration::from_secs(10);

const TRACK: &str = "audio";

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

fn millis(ms: u64) -> Timestamp {
	Timestamp::from_millis(ms).unwrap()
}

fn timed() -> datagram::Info {
	datagram::Info::default().with_timescale(Timescale::MILLI)
}

/// A broadcast published at the first of a chain of origins, one session per hop, each hop
/// on its own version.
struct Chain {
	nodes: Vec<origin::Producer>,
	pairs: Vec<MockPair>,
	publication: origin::Publication,
}

impl Chain {
	async fn new(hops: &[&str]) -> Self {
		Self::with_latency(hops, Duration::ZERO).await
	}

	/// A chain whose every link delays stream data by `latency` each way.
	async fn with_latency(hops: &[&str], latency: Duration) -> Self {
		let nodes: Vec<_> = (1..=hops.len() as u64 + 1).map(produce_origin).collect();
		let mut pairs = Vec::new();
		for (pair, version) in nodes.windows(2).zip(hops) {
			let version = version.parse::<Version>().unwrap();
			pairs.push(peer_with_latency(version, &pair[0], &pair[1], latency).await);
		}
		let publication = nodes[0].create_broadcast("room").unwrap();
		publication.announce(Default::default()).unwrap();
		Self {
			nodes,
			pairs,
			publication,
		}
	}

	/// The broadcast as the last node sees it, once announced there: versions before
	/// lite-05 can't ask a peer about a path it has not announced.
	async fn remote(&self) -> broadcast::Consumer {
		let consumer = self.nodes.last().unwrap().consume();
		kio::clock::timeout(TIMEOUT, support::announced(&consumer, "room"))
			.await
			.expect("announce timeout");
		kio::clock::timeout(TIMEOUT, consumer.request("room", None))
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves")
	}

	/// A remote consumer of `producer`'s track that takes its datagrams.
	///
	/// The consumer takes `priority` first, so the publisher seeing it can't be anything
	/// but this consumer's request.
	async fn consume(&self, producer: &datagram::Producer, priority: i8) -> datagram::Consumer {
		let mut consumer = self.remote().await.datagrams(producer.name()).unwrap();
		consumer.set_priority(priority);
		kio::clock::timeout(TIMEOUT, datagram_priority(producer, Some(priority)))
			.await
			.expect("the request never reached the publisher");
		kio::clock::timeout(TIMEOUT, consumer.info())
			.await
			.expect("info timeout")
			.expect("the publisher answered");
		consumer
	}
}

async fn recv(consumer: &mut datagram::Consumer) -> Result<Option<moq_net::Datagram>, Error> {
	kio::clock::timeout(TIMEOUT, consumer.recv())
		.await
		.expect("recv timeout")
}

/// A datagram track reaches a remote consumer with each datagram's sequence, timestamp and
/// payload, from when the consumer joined, and the publisher's finish ends it cleanly.
#[kio::test]
async fn a_datagram_track_crosses_a_session() {
	for timescale in [Some(Timescale::MILLI), None] {
		let chain = Chain::new(&["moq-lite-07-wip"]).await;
		let info = datagram::Info::default().with_timescale(timescale).with_priority(3);
		let mut producer = chain.publication.create_datagrams(TRACK, info).unwrap();
		let timestamp = |ms| timescale.map(|_| millis(ms));
		// Nobody reads it yet, so nobody ever will.
		assert_eq!(producer.append(timestamp(1), &b"early"[..]).unwrap(), 0);

		let mut consumer = chain.consume(&producer, 0).await;
		assert_eq!(consumer.info().await.unwrap(), info, "{timescale:?}");
		assert_eq!(producer.append(timestamp(2), &b"live"[..]).unwrap(), 1);
		let datagram = recv(&mut consumer).await.unwrap().expect("a datagram");
		assert_eq!(datagram.sequence, 1, "{timescale:?}");
		assert_eq!(datagram.timestamp, timestamp(2), "{timescale:?}");
		assert_eq!(&datagram.payload[..], b"live", "{timescale:?}");

		producer.finish();
		assert!(recv(&mut consumer).await.unwrap().is_none(), "{timescale:?}");
	}
}

/// A publisher's abort reaches the remote consumer as the error.
#[kio::test]
async fn an_aborted_datagram_track_ends_with_its_error() {
	let chain = Chain::new(&["moq-lite-07-wip"]).await;
	let producer = chain.publication.create_datagrams(TRACK, timed()).unwrap();
	let mut consumer = chain.consume(&producer, 0).await;
	producer.abort(Error::App(7));
	let err = recv(&mut consumer).await.unwrap_err();
	assert!(matches!(err, Error::Stream(StreamError::App(7))), "{err:?}");
}

/// A consumer's priority, raised or lowered, is what the publisher sees, including a change
/// made after the request reached the publisher but before its answer came back.
#[kio::test]
async fn a_consumer_priority_reaches_the_publisher() {
	let chain = Chain::with_latency(&["moq-lite-07-wip"], Duration::from_millis(10)).await;
	let producer = chain.publication.create_datagrams(TRACK, timed()).unwrap();
	let mut consumer = chain.remote().await.datagrams(TRACK).unwrap();
	kio::clock::timeout(TIMEOUT, datagram_priority(&producer, Some(0)))
		.await
		.expect("the request never reached the publisher");
	for priority in [5, -3] {
		consumer.set_priority(priority);
		kio::clock::timeout(TIMEOUT, datagram_priority(&producer, Some(priority)))
			.await
			.unwrap_or_else(|_| panic!("priority {priority} never arrived"));
	}
}

/// Dropping the last remote consumer ends the subscription upstream.
#[kio::test]
async fn the_last_consumer_leaving_ends_the_subscription() {
	let chain = Chain::new(&["moq-lite-07-wip"]).await;
	let producer = chain.publication.create_datagrams(TRACK, timed()).unwrap();
	let consumer = chain.consume(&producer, 0).await;
	drop(consumer);
	kio::clock::timeout(TIMEOUT, datagram_priority(&producer, None))
		.await
		.expect("the publisher still serves it");
}

/// A relay carries datagrams, priority, and the end across both of its lite-07 hops.
#[kio::test]
async fn a_relay_carries_a_datagram_track() {
	let chain = Chain::new(&["moq-lite-07-wip", "moq-lite-07-wip"]).await;
	let mut producer = chain.publication.create_datagrams(TRACK, timed()).unwrap();
	let mut consumer = chain.consume(&producer, 2).await;

	assert_eq!(producer.append(millis(5), &b"relayed"[..]).unwrap(), 0);
	let datagram = recv(&mut consumer).await.unwrap().expect("a datagram");
	assert_eq!(datagram.sequence, 0);
	assert_eq!(datagram.timestamp, Some(millis(5)));
	assert_eq!(&datagram.payload[..], b"relayed");

	consumer.set_priority(4);
	kio::clock::timeout(TIMEOUT, datagram_priority(&producer, Some(4)))
		.await
		.expect("the priority never crossed the relay");

	producer.finish();
	assert!(recv(&mut consumer).await.unwrap().is_none());
}

/// A stream track and a datagram track of the same name are separate tracks: each carries
/// only its own data and ends on its own.
#[kio::test]
async fn a_stream_track_and_a_datagram_track_share_a_name_independently() {
	let chain = Chain::new(&["moq-lite-07-wip"]).await;
	let mut track = chain
		.publication
		.create_track(TRACK, moq_net::track::Info::default().with_timescale(Timescale::MILLI))
		.unwrap();
	let mut datagrams = chain.publication.create_datagrams(TRACK, timed()).unwrap();
	let remote = chain.remote().await;
	let mut subscriber = kio::clock::timeout(TIMEOUT, remote.track(TRACK).unwrap().subscribe(None))
		.await
		.expect("subscribe timeout")
		.expect("subscribe");
	support::harness::subscribed(&track).await;
	let mut consumer = chain.consume(&datagrams, 0).await;

	datagrams.append(millis(1), &b"datagram"[..]).unwrap();
	track.write_frame(millis(2), &b"frame"[..]).unwrap();
	let datagram = recv(&mut consumer).await.unwrap().expect("a datagram");
	assert_eq!(datagram.sequence, 0);
	assert_eq!(&datagram.payload[..], b"datagram");
	datagrams.finish();
	assert!(recv(&mut consumer).await.unwrap().is_none());

	// The datagram track's end leaves the stream track live, with only its own groups.
	let mut group = kio::clock::timeout(TIMEOUT, subscriber.recv_group())
		.await
		.expect("group timeout")
		.unwrap()
		.expect("a group");
	assert_eq!(group.sequence, 0);
	assert_eq!(
		&group.read_frame().await.unwrap().expect("a frame").payload[..],
		b"frame"
	);
	track.write_frame(millis(3), &b"after"[..]).unwrap();
	let group = kio::clock::timeout(TIMEOUT, subscriber.recv_group())
		.await
		.expect("group timeout")
		.unwrap()
		.expect("a group");
	assert_eq!(group.sequence, 1);
}

/// On lite-07 a name that has only a datagram track has no stream track to subscribe to.
#[kio::test]
async fn a_lite07_stream_subscription_never_takes_datagrams() {
	let chain = Chain::new(&["moq-lite-07-wip"]).await;
	let _datagrams = chain.publication.create_datagrams(TRACK, timed()).unwrap();
	let remote = chain.remote().await;
	let result = kio::clock::timeout(TIMEOUT, remote.track(TRACK).unwrap().subscribe(None))
		.await
		.expect("subscribe timeout");
	let Err(err) = result else {
		panic!("subscribed to a stream track that does not exist");
	};
	assert!(
		matches!(err, Error::NotFound | Error::Stream(StreamError::NotFound)),
		"{err:?}"
	);
}

/// Lite-05/06 carry a datagram track as a SUBSCRIBE of its name, keeping only the group
/// datagrams. Both ends run this crate, so this covers asking an old publisher for a
/// datagram track and serving an old subscriber's SUBSCRIBE from one.
#[kio::test]
async fn lite05_and_lite06_map_a_datagram_track_onto_a_subscription() {
	for version in ["moq-lite-05", "moq-lite-06"] {
		let chain = Chain::new(&[version]).await;
		let mut producer = chain.publication.create_datagrams(TRACK, timed()).unwrap();
		producer.append(millis(1), &b"early"[..]).unwrap();
		// A priority other than the default, which the TRACK lookup ahead of the SUBSCRIBE
		// holds while it is answered.
		let mut consumer = chain.consume(&producer, 3).await;
		assert_eq!(
			consumer.info().await.unwrap().timescale,
			Some(Timescale::MILLI),
			"{version}"
		);

		assert_eq!(producer.append(millis(9), &b"mapped"[..]).unwrap(), 1);
		let datagram = recv(&mut consumer).await.unwrap().expect("a datagram");
		assert_eq!(datagram.sequence, 1, "{version}");
		assert_eq!(datagram.timestamp, Some(millis(9)), "{version}");
		assert_eq!(&datagram.payload[..], b"mapped", "{version}");

		consumer.set_priority(6);
		kio::clock::timeout(TIMEOUT, datagram_priority(&producer, Some(6)))
			.await
			.unwrap_or_else(|_| panic!("{version}: the priority never arrived"));

		producer.finish();
		assert!(recv(&mut consumer).await.unwrap().is_none(), "{version}");
	}
}

/// Lite-05/06 carry a name's stream track and datagram track on one subscription, as
/// moq-transport does, so a name with both delivers each to its own reader, directly and
/// through a relay whose hops are both old. The datagrams ride the subscription, at its
/// priority.
#[kio::test]
async fn lite05_and_lite06_carry_both_kinds_of_a_name() {
	for hops in [&["moq-lite-05"][..], &["moq-lite-06"], &["moq-lite-06", "moq-lite-06"]] {
		let chain = Chain::new(hops).await;
		let mut track = chain
			.publication
			.create_track(TRACK, moq_net::track::Info::default().with_timescale(Timescale::MILLI))
			.unwrap();
		// Another scale than the stream track's, whose TRACK_INFO declares the one both
		// kinds are stamped in on the wire.
		let info = datagram::Info::default().with_timescale(Timescale::MICRO);
		let mut producer = chain.publication.create_datagrams(TRACK, info).unwrap();
		let remote = chain.remote().await;
		let mut subscriber = kio::clock::timeout(TIMEOUT, remote.track(TRACK).unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.unwrap_or_else(|err| panic!("{hops:?}: {err:?}"));
		// The publisher sends the datagrams on the stream subscription.
		kio::clock::timeout(TIMEOUT, datagram_priority(&producer, Some(0)))
			.await
			.unwrap_or_else(|_| panic!("{hops:?}: the subscription never carried the datagrams"));
		let mut consumer = remote.datagrams(TRACK).unwrap();
		kio::clock::timeout(TIMEOUT, consumer.info())
			.await
			.expect("info timeout")
			.unwrap_or_else(|err| panic!("{hops:?}: {err:?}"));

		producer.append(millis(1), &b"datagram"[..]).unwrap();
		let datagram = recv(&mut consumer).await.unwrap().expect("a datagram");
		assert_eq!(&datagram.payload[..], b"datagram", "{hops:?}");
		let timestamp = datagram.timestamp.expect("a timestamp");
		assert_eq!(timestamp.convert(Timescale::MILLI).unwrap(), millis(1), "{hops:?}");

		track.write_frame(millis(2), &b"frame"[..]).unwrap();
		let mut group = kio::clock::timeout(TIMEOUT, subscriber.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("a group");
		assert_eq!(
			&group.read_frame().await.unwrap().expect("a frame").payload[..],
			b"frame",
			"{hops:?}"
		);

		let subscription = subscriber.subscription().with_priority(6);
		subscriber.update(subscription).unwrap();
		kio::clock::timeout(TIMEOUT, datagram_priority(&producer, Some(6)))
			.await
			.unwrap_or_else(|_| panic!("{hops:?}: the priority never arrived"));
	}
}

/// On lite-05/06 the datagrams a stream track's subscription carries end as it does: a
/// stream track that aborts ends its datagram reader with the error, not a finish.
#[kio::test]
async fn lite06_shared_datagrams_end_with_their_subscription() {
	let chain = Chain::new(&["moq-lite-06"]).await;
	let track = chain
		.publication
		.create_track(TRACK, moq_net::track::Info::default().with_timescale(Timescale::MILLI))
		.unwrap();
	let producer = chain.publication.create_datagrams(TRACK, timed()).unwrap();
	let remote = chain.remote().await;
	let _subscriber = kio::clock::timeout(TIMEOUT, remote.track(TRACK).unwrap().subscribe(None))
		.await
		.expect("subscribe timeout")
		.expect("subscribe");
	kio::clock::timeout(TIMEOUT, datagram_priority(&producer, Some(0)))
		.await
		.expect("the subscription never carried the datagrams");
	let mut consumer = remote.datagrams(TRACK).unwrap();
	kio::clock::timeout(TIMEOUT, consumer.info())
		.await
		.expect("info timeout")
		.expect("info");

	track.abort(Error::App(7)).unwrap();
	let end = recv(&mut consumer).await;
	assert!(end.is_err(), "{end:?}");
}

/// A lite-05/06 subscriber reads a name as a stream track, as one predating datagram tracks
/// does, so the datagram fallback ends with SUBSCRIBE_FINISH ahead of its FIN: a FIN alone
/// is a protocol violation there.
#[kio::test]
async fn lite05_and_lite06_finish_a_datagram_track_as_a_stream_subscriber_expects() {
	for version in ["moq-lite-05", "moq-lite-06"] {
		let chain = Chain::new(&[version]).await;
		let mut producer = chain.publication.create_datagrams(TRACK, timed()).unwrap();
		let remote = chain.remote().await;
		let mut subscriber = kio::clock::timeout(TIMEOUT, remote.track(TRACK).unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.unwrap_or_else(|err| panic!("{version}: {err:?}"));
		// Only the SUBSCRIBE carries an update, so the publisher seeing it serves the
		// subscription from the datagram track.
		let subscription = subscriber.subscription().with_priority(5);
		subscriber.update(subscription).unwrap();
		kio::clock::timeout(TIMEOUT, datagram_priority(&producer, Some(5)))
			.await
			.unwrap_or_else(|_| panic!("{version}: the subscription never reached the publisher"));
		producer.append(millis(1), &b"datagram"[..]).unwrap();
		producer.finish();
		let end = kio::clock::timeout(TIMEOUT, subscriber.recv_group())
			.await
			.expect("end timeout");
		match end {
			Ok(None) => {}
			Ok(Some(group)) => panic!("{version}: a group {}", group.sequence),
			Err(err) => panic!("{version}: {err:?}"),
		}
	}
}

/// A datagram track can't be asked for over a version or transport without datagrams.
#[kio::test]
async fn a_datagram_track_needs_datagrams() {
	for (version, transport) in [("moq-lite-03", true), ("moq-lite-04", true), ("moq-lite-07-wip", false)] {
		let chain = Chain::new(&[version]).await;
		if !transport {
			// The second node subscribes through the session it accepted.
			chain.pairs[0].server_transport.disable_datagrams();
		}
		let _producer = chain.publication.create_datagrams(TRACK, timed()).unwrap();
		let mut consumer = chain.remote().await.datagrams(TRACK).unwrap();
		let err = recv(&mut consumer).await.unwrap_err();
		assert!(matches!(err, Error::Unsupported), "{version}: {err:?}");
	}
}

/// moq-transport carries no datagram tracks: asking for one finds nothing, and the stream
/// track of the same name is served as usual.
#[kio::test]
async fn moq_transport_has_no_datagram_tracks() {
	let chain = Chain::new(&["moq-transport-22"]).await;
	let mut track = chain
		.publication
		.create_track(TRACK, moq_net::track::Info::default().with_timescale(Timescale::MILLI))
		.unwrap();
	let _datagrams = chain.publication.create_datagrams(TRACK, timed()).unwrap();
	let remote = chain.remote().await;
	assert!(matches!(remote.datagrams(TRACK), Err(Error::NotFound)));

	let mut subscriber = kio::clock::timeout(TIMEOUT, remote.track(TRACK).unwrap().subscribe(None))
		.await
		.expect("subscribe timeout")
		.expect("subscribe");
	support::harness::subscribed(&track).await;
	track.write_frame(millis(1), &b"frame"[..]).unwrap();
	let mut group = kio::clock::timeout(TIMEOUT, subscriber.recv_group())
		.await
		.expect("group timeout")
		.unwrap()
		.expect("a group");
	assert_eq!(
		&group.read_frame().await.unwrap().expect("a frame").payload[..],
		b"frame"
	);
}
