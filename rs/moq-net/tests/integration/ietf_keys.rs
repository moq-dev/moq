#![cfg(feature = "async")]

//! moq-transport carries group keys verbatim as Group IDs through a relay on every
//! supported draft.

use crate::WithMax as _;
use crate::support;

use std::time::Duration;

use moq_net::track::{Info, Subscription};
use moq_net::{Timescale, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

fn origin(hop: u64) -> moq_net::origin::Producer {
	let mut config = moq_net::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.proxy = true;
	let (producer, driver) = moq_net::origin::Producer::new(config);
	support::harness::spawn(driver);
	producer
}

/// Appended groups keep their keys, and the time they carry, through a relay. Each
/// carries a Prior Group ID Gap to its predecessor, which the unit tests read off the wire;
/// here it must only not disturb the keys around it.
#[kio::test]
async fn a_relay_keeps_keys_and_the_gaps_between_them() {
	for draft in [18, 20, 22] {
		let version: Version = format!("moq-transport-{draft}").parse().unwrap();
		kio::clock::timeout(TIMEOUT, async {
			let publisher = origin(1);
			let relay = origin(2);
			let client = origin(3);
			let broadcast = publisher
				.route("bcast", moq_net::origin::RouteOptions::hidden())
				.unwrap();
			let track = broadcast
				.broadcast()
				.create_track(
					"video",
					Info::default().with_timescale(Timescale::MILLI).with_max_age(None),
				)
				.unwrap();
			broadcast.announce().unwrap();

			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(relay.clone());
			let _upstream = connect_mock(options).await;
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(relay.consume());
			options.client_subscribe = Some(client.clone());
			let _downstream = connect_mock(options).await;

			let remote = client
				.consume()
				.follow("bcast", None)
				.unwrap()
				.available()
				.await
				.unwrap()
				.unwrap();
			let mut reader = remote
				.track("video")
				.unwrap()
				.subscribe(
					Subscription::default()
						.with_ranges([support::keys(2..=8)])
						.with_max(None)
						.with_order(moq_net::track::Order::Ascending),
				)
				.await
				.unwrap();
			support::wanted(track.demand()).await.unwrap();

			const KEYS: [u64; 3] = [2, 5, 8];
			for key in KEYS {
				let mut group = track.append_group(Timestamp::from_millis(key).unwrap()).unwrap();
				group
					.write_frame(Timestamp::from_millis(key).unwrap(), b"x".as_slice())
					.unwrap();
				group.finish().unwrap();
			}

			for key in KEYS {
				let mut group = reader.recv_group().await.unwrap().expect("a group");
				assert_eq!(group.start, key, "{version}");
				let frame = group.read_frame().await.unwrap().unwrap();
				assert_eq!(frame.timestamp, Timestamp::from_millis(key).unwrap(), "{version}");
			}
			assert!(reader.recv_group().await.unwrap().is_none(), "{version}");
		})
		.await
		.unwrap_or_else(|_| panic!("{version}: timed out"));
	}
}
