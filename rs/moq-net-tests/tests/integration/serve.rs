//! A held answer belongs to its prefix route; a published job has its own route.

use std::time::Duration;

use moq_net::{Epoch, Hop, Timestamp, Version, broadcast, origin, track};

use crate::support::harness::{MockConnectOptions, connect_mock, spawn};

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	origin
}

async fn first_subscribe(version: &str, publish: bool) {
	let version: Version = version.parse().unwrap();
	let server = origin(1);
	let client = origin(2);
	let requests = server.requests("").unwrap();
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(server.consume());
	options.client_subscribe = Some(client.clone());
	let _pair = connect_mock(options).await;

	let remote = client.consume().request("camera", None).await.unwrap();
	let read = async {
		let mut reader = remote.track("video")?.subscribe(None).await?;
		let mut group = reader.recv_group().await?.ok_or(moq_net::Error::Unroutable)?;
		let frame = group.read_frame().await?.ok_or(moq_net::Error::Unroutable)?;
		Ok::<_, moq_net::Error>(frame.payload)
	};
	// A held broadcast needs no origin or announcement of its own.
	let held = broadcast::Info::new().produce();
	let held_track = held.create_track("video", track::Info::default().with_timescale(moq_net::Timescale::MILLI)).unwrap();
	let mut group = held_track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, b"held".as_slice()).unwrap();
	group.finish().unwrap();
	let handler = async {
		if publish {
			let prepared = requests
				.requested_broadcast()
				.await
				.unwrap()
				.prepare(Epoch::mint())
				.unwrap();
			let track = prepared
				.broadcast()
				.create_track("video", track::Info::default().with_timescale(moq_net::Timescale::MILLI))
				.unwrap();
			let mut group = track.append_group().unwrap();
			group.write_frame(Timestamp::ZERO, b"held".as_slice()).unwrap();
			group.finish().unwrap();
			Some(prepared.publish().unwrap())
		} else {
			requests.requested_broadcast().await.unwrap().serve(&held);
			None
		}
	};
	let (result, _publication) = futures::join!(read, handler);
	if publish && version != "moq-lite-07-wip".parse().unwrap() {
		assert!(
			result.is_err(),
			"{version}: publishing an exact route still resets the first subscribe"
		);
	} else {
		assert_eq!(result.unwrap().as_ref(), b"held", "{version}");
	}
}

macro_rules! cases {
	($($name:ident: $version:literal, $publish:literal;)*) => {$(
		#[moq_clock::test]
		async fn $name() {
			moq_clock::timeout(Duration::from_secs(10), first_subscribe($version, $publish))
				.await.expect("first subscribe stalled");
		}
	)*};
}

cases! {
	serve_ietf18: "moq-transport-18", false;
	serve_ietf22: "moq-transport-22", false;
	serve_lite06: "moq-lite-06", false;
	serve_lite07: "moq-lite-07-wip", false;
	publish_ietf18: "moq-transport-18", true;
	publish_ietf22: "moq-transport-22", true;
	publish_lite06: "moq-lite-06", true;
	publish_lite07: "moq-lite-07-wip", true;
}
