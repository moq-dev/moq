#![cfg(feature = "async")]

//! FETCH caps are charged per peer before shared group requests coalesce.

use crate::support;

use std::time::Duration;

use moq_net::{Hop, Version, session::Limits};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(5);

fn origin(hop: u64) -> moq_net::origin::Producer {
	let (origin, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	origin
}

#[moq_clock::test]
async fn an_over_cap_peer_cannot_cancel_another_peers_held_fetch() {
	for version in [
		"moq-lite-05",
		"moq-lite-06",
		"moq-lite-07-wip",
		"moq-transport-14",
		"moq-transport-16",
		"moq-transport-22",
	] {
		let version: Version = version.parse().unwrap();
		let publisher = origin(1);
		let broadcast = publisher.create_broadcast("room").unwrap();
		let info = moq_net::track::Info::default().with_timescale(None);
		let track = broadcast.create_track("video", info.clone()).unwrap();
		let groups = track.dynamic();
		broadcast.announce(Default::default()).unwrap();
		let mut peers = Vec::new();
		for hop in [2, 3] {
			let remote = origin(hop);
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(remote.clone());
			let mut limits = Limits::default();
			limits.fetches = 1;
			options.server_limits = Some(limits);
			let pair = connect_mock(options).await;
			let view = moq_clock::timeout(TIMEOUT, remote.consume().routed_broadcast("room"))
				.await
				.unwrap()
				.unwrap();
			peers.push((remote, pair, view));
		}
		let mut a = Box::pin(peers[0].2.track("video").unwrap().fetch_group(7, None));
		let request_a = match moq_clock::timeout(
			TIMEOUT,
			futures::future::select(std::pin::pin!(groups.requested_group()), &mut a),
		)
		.await
		.unwrap()
		{
			futures::future::Either::Left((request, _)) => request.unwrap(),
			futures::future::Either::Right(_) => panic!("{version}: fetch answered before the publisher"),
		};
		let mut b = Box::pin(peers[1].2.track("video").unwrap().fetch_group(8, None));
		let request_b = match moq_clock::timeout(
			TIMEOUT,
			futures::future::select(std::pin::pin!(groups.requested_group()), &mut b),
		)
		.await
		.unwrap()
		{
			futures::future::Either::Left((request, _)) => request.unwrap(),
			futures::future::Either::Right(_) => panic!("{version}: fetch answered before the publisher"),
		};
		assert_eq!(request_a.sequence(), 7);
		assert_eq!(request_b.sequence(), 8);
		let extra = peers[0].2.track("video").unwrap().fetch_group(9, None);
		let (err, refused) = moq_clock::timeout(TIMEOUT, futures::future::join(peers[0].1.client.closed(), extra))
			.await
			.unwrap();
		assert!(refused.is_err());
		assert!(
			matches!(err, moq_net::Error::Session(moq_net::SessionError::TooManyRequests)),
			"{version}: {err}"
		);
		assert!(
			request_b.demand().is_used(),
			"{version}: the other peer still holds its request"
		);
		let mut group = request_b.accept(info).unwrap();
		group.write_frame(None, &b"other peer"[..]).unwrap();
		group.finish().unwrap();
		let mut served = moq_clock::timeout(TIMEOUT, b).await.unwrap().unwrap();
		assert_eq!(
			served.read_frame().await.unwrap().unwrap().payload.as_ref(),
			b"other peer"
		);
		assert!(moq_clock::timeout(TIMEOUT, a).await.unwrap().is_err());
	}
}
