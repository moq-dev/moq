//! A FETCH nobody wants anymore is cancelled all the way to the publisher.
//!
//! Each relay on the path holds a FETCH stream and the group behind it. When the
//! reader gives up, every hop must let go, whether the publisher has yet to answer
//! (lite has no FETCH_OK, so one that never answers would pin them for good) or is
//! partway through the group. A group cut short is aborted, never cached as whole.

mod support;

use std::time::Duration;

use bytes::Bytes;
use moq_net::{Hop, Timestamp, Version, origin, track};
use support::harness::peer;

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// Where the reader gives up.
#[derive(Clone, Copy, Debug)]
enum Stage {
	/// Before the publisher answers the FETCH.
	Unanswered,
	/// After the first frame, with the rest of the group still to come.
	MidResponse,
	/// Once every frame is read, as the FIN arrives.
	Drained,
}

/// `relays` relays between the publisher and the reader.
async fn abandoned_fetch_reaches_the_publisher(version: &str, relays: u64, stage: Stage) {
	let version: Version = version.parse().unwrap();
	let ctx = format!("{version:?} over {relays} relays, {stage:?}");
	let nodes: Vec<_> = (1..=relays + 1).map(produce_origin).collect();
	let mut _pairs = Vec::new();
	for pair in nodes.windows(2) {
		_pairs.push(peer(version, &pair[0], &pair[1]).await);
	}

	let broadcast = nodes[0].create_broadcast("room").unwrap();
	let mut dynamic = broadcast.dynamic();
	broadcast.announce(Default::default()).unwrap();
	moq_net_sim::sleep(Duration::from_secs(1)).await;

	let consumer = nodes[relays as usize]
		.consume()
		.request_broadcast("room", None)
		.await
		.unwrap();
	let track = consumer.track("video").unwrap();
	let mut waiting = Box::pin(track.fetch_group(0, None));

	// Answer the TRACK_INFO, then take the FETCH.
	let request = match futures::future::select(std::pin::pin!(dynamic.requested_track()), &mut waiting).await {
		futures::future::Either::Left((request, _)) => request.expect("the track request reaches the publisher"),
		futures::future::Either::Right(_) => panic!("nothing answered the track"),
	};
	let groups = request.dynamic();
	let _track = request.accept(track::Info::default());
	let request = match futures::future::select(std::pin::pin!(groups.requested_group()), &mut waiting).await {
		futures::future::Either::Left((request, _)) => request.expect("the fetch reaches the publisher"),
		futures::future::Either::Right(_) => panic!("nothing answered the fetch"),
	};

	match stage {
		Stage::Unanswered => {
			drop(waiting);
			moq_net_sim::timeout(
				Duration::from_secs(5),
				kio::wait(|waiter| request.demand().poll_unused(waiter)),
			)
			.await
			.unwrap_or_else(|_| panic!("{ctx}: the publisher's fetch is still wanted"))
			.expect("the pending fetch is still open");
		}
		Stage::MidResponse => {
			let mut group = request.accept(None).unwrap();
			group.write_frame(Timestamp::ZERO, Bytes::from_static(b"head")).unwrap();

			let mut fetched = waiting.await.unwrap();
			let frame = fetched.read_frame().await.unwrap().unwrap();
			assert_eq!(&frame.payload[..], b"head", "{ctx}");
			drop(fetched);

			// The publisher stops serving the group: nobody reads it any more.
			moq_net_sim::timeout(Duration::from_secs(5), group.demand().unused())
				.await
				.unwrap_or_else(|_| panic!("{ctx}: the publisher still serves the fetch"))
				.unwrap();

			// No hop cached the head as the whole group: a fresh fetch reads it all.
			group.write_frame(Timestamp::ZERO, Bytes::from_static(b"tail")).unwrap();
			group.finish().unwrap();
			let mut fetched = moq_net_sim::timeout(Duration::from_secs(5), track.fetch_group(0, None))
				.await
				.unwrap_or_else(|_| panic!("{ctx}: the refetch stalled"))
				.unwrap();
			let mut payloads = Vec::new();
			while let Some(frame) = fetched.read_frame().await.unwrap() {
				payloads.push(frame.payload);
			}
			assert_eq!(payloads, [&b"head"[..], &b"tail"[..]], "{ctx}");
		}
		Stage::Drained => {
			let mut group = request.accept(None).unwrap();
			group.write_frame(Timestamp::ZERO, Bytes::from_static(b"head")).unwrap();
			group.write_frame(Timestamp::ZERO, Bytes::from_static(b"tail")).unwrap();

			let mut fetched = waiting.await.unwrap();
			for expected in [&b"head"[..], b"tail"] {
				let frame = fetched.read_frame().await.unwrap().unwrap();
				assert_eq!(&frame.payload[..], expected, "{ctx}");
			}

			// The FIN reaches the reader's node at the instant the reader leaves: the
			// group is whole, so it must be cached as such rather than cancelled.
			const LATENCY: Duration = Duration::from_millis(100);
			_pairs.last().unwrap().client_transport.set_latency(LATENCY);
			group.finish().unwrap();
			moq_net_sim::sleep(LATENCY).await;
			drop(fetched);
			moq_net_sim::sleep(Duration::from_secs(1)).await;

			// The publisher can no longer serve it, so only a cached copy answers.
			group.abort(moq_net::Error::Cancel).unwrap();
			let mut fetched = moq_net_sim::timeout(Duration::from_secs(5), track.fetch_group(0, None))
				.await
				.unwrap_or_else(|_| panic!("{ctx}: the refetch stalled"))
				.unwrap_or_else(|err| panic!("{ctx}: the whole group was not cached: {err}"));
			let mut payloads = Vec::new();
			while let Some(frame) = fetched.read_frame().await.unwrap() {
				payloads.push(frame.payload);
			}
			assert_eq!(payloads, [&b"head"[..], &b"tail"[..]], "{ctx}");
		}
	}
}

async fn abandoned_fetches_reach_the_publisher(version: &str) {
	for stage in [Stage::Unanswered, Stage::MidResponse, Stage::Drained] {
		for relays in [1, 2] {
			abandoned_fetch_reaches_the_publisher(version, relays, stage).await;
		}
	}
}

#[moq_net_sim::test]
async fn abandoned_fetch_reaches_the_publisher_lite05() {
	abandoned_fetches_reach_the_publisher("moq-lite-05").await;
}

#[moq_net_sim::test]
async fn abandoned_fetch_reaches_the_publisher_lite06() {
	abandoned_fetches_reach_the_publisher("moq-lite-06").await;
}

/// A group fetched through a relay is still there for the next fetch shortly after the
/// track went unread: the relay's copy lingers with its cache, so a segmented reader
/// (HLS fetching each segment) does not go upstream again for every request.
async fn a_refetch_within_the_linger_stays_on_the_relay(version: &str) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let _pair = peer(version, &publisher, &relay).await;

	let broadcast = publisher.create_broadcast("room").unwrap();
	let mut dynamic = broadcast.dynamic();
	broadcast.announce(Default::default()).unwrap();
	moq_net_sim::sleep(Duration::from_secs(1)).await;

	let consumer = relay.consume().request_broadcast("room", None).await.unwrap();
	let mut waiting = Box::pin(consumer.track("video").unwrap().fetch_group(0, None));
	let request = match futures::future::select(std::pin::pin!(dynamic.requested_track()), &mut waiting).await {
		futures::future::Either::Left((request, _)) => request.expect("the track request reaches the publisher"),
		futures::future::Either::Right(_) => panic!("nothing answered the track"),
	};
	let groups = request.dynamic();
	let _track = request.accept(track::Info::default());
	let request = match futures::future::select(std::pin::pin!(groups.requested_group()), &mut waiting).await {
		futures::future::Either::Left((request, _)) => request.expect("the fetch reaches the publisher"),
		futures::future::Either::Right(_) => panic!("nothing answered the fetch"),
	};
	let mut group = request.accept(None).unwrap();
	group
		.write_frame(Timestamp::ZERO, Bytes::from_static(b"segment"))
		.unwrap();
	group.finish().unwrap();
	let mut fetched = waiting.await.unwrap();
	assert_eq!(&fetched.read_frame().await.unwrap().unwrap().payload[..], b"segment");
	drop(fetched);
	// The publisher lets the group go, so only another FETCH would bring it back.
	group.abort(moq_net::Error::Cancel).unwrap();

	// The track goes unread between segments, well inside the linger.
	moq_net_sim::sleep(Duration::from_secs(5)).await;

	let mut refetch = Box::pin(consumer.track("video").unwrap().fetch_group(0, None));
	let track_request = std::pin::pin!(dynamic.requested_track());
	let group_request = std::pin::pin!(groups.requested_group());
	let upstream = futures::future::select(track_request, group_request);
	let mut fetched = match futures::future::select(&mut refetch, upstream).await {
		futures::future::Either::Left((fetched, _)) => fetched.expect("the relay serves the group again"),
		futures::future::Either::Right((futures::future::Either::Left(_), _)) => {
			panic!("{version}: the relay asked for the track again")
		}
		futures::future::Either::Right((futures::future::Either::Right(_), _)) => {
			panic!("{version}: the relay fetched the group again")
		}
	};
	assert_eq!(&fetched.read_frame().await.unwrap().unwrap().payload[..], b"segment");
}

#[moq_net_sim::test]
async fn a_refetch_within_the_linger_stays_on_the_relay_lite06() {
	a_refetch_within_the_linger_stays_on_the_relay("moq-lite-06").await;
}

#[moq_net_sim::test]
async fn a_refetch_within_the_linger_stays_on_the_relay_lite07() {
	a_refetch_within_the_linger_stays_on_the_relay("moq-lite-07-wip").await;
}

#[moq_net_sim::test]
async fn a_refetch_within_the_linger_stays_on_the_relay_ietf19() {
	a_refetch_within_the_linger_stays_on_the_relay("moq-transport-19").await;
}
