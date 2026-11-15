#![cfg(feature = "async")]

//! A session counts egress payload as the transport delivers it, so a frame counts once its
//! subscriber has it, even while its group stays open with nothing more to send.

use crate::support;

use std::time::Duration;

use moq_net::track::{Info, Subscription};
use moq_net::{Timescale, Timestamp, Version, stats};
use support::harness::{MockConnectOptions, connect_mock, subscribed};

const TIMEOUT: Duration = Duration::from_secs(10);

const VERSIONS: &[&str] = &["moq-lite-06", "moq-lite-07-wip", "moq-transport-18", "moq-transport-22"];

const FRAME: usize = 1000;

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) =
		moq_net::origin::Producer::new(moq_net::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer.with_proxy(true).with_link(moq_net::origin::Link::Peer)
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

#[kio::test]
async fn an_open_group_s_last_frame_counts_once_delivered() {
	for &version in VERSIONS {
		let publisher = produce_origin(1);
		let downstream = produce_origin(2);
		// The server publishes, so its session meters what goes downstream.
		let registry = stats::Registry::new(Default::default());
		let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
		options.client_publish = Some(downstream.consume().with_hidden(true));
		options.client_subscribe = Some(downstream.clone());
		options.server_publish = Some(publisher.consume().with_hidden(true));
		options.server_subscribe = Some(publisher.clone());
		options.server_stats = registry
			.tier(stats::Tier::default())
			.session(stats::SessionInfo::new(moq_net::Path::from_static("")));
		let _pair = connect_mock(options).await;

		let broadcast = publisher
			.route("room", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let track = broadcast
			.broadcast()
			.create_track("data", Info::default().with_timescale(Timescale::MILLI))
			.unwrap();
		broadcast.announce().unwrap();

		let remote = kio::clock::timeout(TIMEOUT, downstream.consume().follow("room", None).unwrap().available())
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves")
			.unwrap();
		let mut live = kio::clock::timeout(
			TIMEOUT,
			remote.track("data").unwrap().subscribe(Subscription::default()),
		)
		.await
		.expect("subscribe timeout")
		.expect("subscribe");
		subscribed(&track).await;

		// One frame, in a group left open.
		let mut open = track.append_group(0).unwrap();
		open.write_frame(Timestamp::ZERO, vec![b'x'; FRAME]).unwrap();
		let mut group = kio::clock::timeout(TIMEOUT, live.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("group 0");
		assert!(group.read_frame().await.unwrap().is_some(), "{version}");

		kio::clock::sleep(Duration::from_secs(1)).await;
		assert_eq!(sent(&registry), (FRAME as u64, 1), "{version}");
	}
}
