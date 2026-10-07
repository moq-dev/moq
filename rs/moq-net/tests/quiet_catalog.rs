//! A catalog publishes one finished group and then stays quiet. Media hides a
//! subscription that starts past a group, because the next group arrives; a
//! catalog does not produce one, so a reader that never received the snapshot
//! must still get it.

mod support;

use std::{future::Future, time::Duration};

use moq_net::{Error, Hop, Timestamp, Version, broadcast, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(5);

const VERSIONS: &[&str] = &["moq-lite-06", "moq-lite-07-wip", "moq-transport-16", "moq-transport-22"];

/// The versions that forward a resume's floor upstream; the IETF relay subscribes without one.
const LITE: &[&str] = &["moq-lite-06", "moq-lite-07-wip"];

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

async fn link(version: Version, from: &origin::Producer, to: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(from.consume());
	options.client_subscribe = Some(to.clone());
	connect_mock(options).await
}

fn abort(pair: MockPair) {
	pair.server.abort(Error::Cancel);
	pair.client.abort(Error::Cancel);
}

async fn settle() {
	moq_net_sim::sleep(Duration::from_millis(500)).await;
}

/// Announce `live/catalog.json` holding one finished snapshot group.
fn publish(origin: &origin::Producer) -> (broadcast::Producer, track::Producer) {
	let broadcast = origin.create_broadcast("live").unwrap();
	let track = broadcast.create_track("catalog.json", None).unwrap();
	broadcast.announce(Default::default()).unwrap();
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, b"snapshot").unwrap();
	group.finish().unwrap();
	(broadcast, track)
}

async fn request(origin: &origin::Producer) -> broadcast::Consumer {
	let consumer = origin.consume();
	consumer.routed("live").await.unwrap();
	consumer.request_broadcast("live").await.unwrap()
}

/// Subscribe without a floor and expect the snapshot as group 0.
async fn read_snapshot(remote: &broadcast::Consumer) -> Result<track::Subscriber, String> {
	let subscribe = remote
		.track("catalog.json")
		.map_err(|err| format!("track: {err}"))?
		.subscribe(None);
	let mut sub = moq_net_sim::timeout(TIMEOUT, subscribe)
		.await
		.map_err(|_| "subscribe never resolved")?
		.map_err(|err| format!("subscribe: {err}"))?;
	let mut group = moq_net_sim::timeout(TIMEOUT, sub.recv_group())
		.await
		.map_err(|_| "subscribe resolved but no group arrived")?
		.map_err(|err| format!("recv: {err}"))?
		.ok_or("track ended with no group")?;
	let frame = moq_net_sim::timeout(TIMEOUT, group.read_frame())
		.await
		.map_err(|_| "group had no frame")?
		.map_err(|err| format!("read: {err}"))?
		.ok_or("group ended empty")?;
	match (group.sequence, frame.payload.as_ref()) {
		(0, b"snapshot") => Ok(sub),
		(sequence, payload) => Err(format!("got group {sequence} {payload:?}")),
	}
}

/// Run `scenario` for each of `versions` and report all failures together.
async fn each_version<F: Future<Output = Result<(), String>>>(versions: &[&str], scenario: impl Fn(Version) -> F) {
	let mut failures = Vec::new();
	for version in versions {
		let version: Version = version.parse().unwrap();
		match moq_net_sim::timeout(Duration::from_secs(30), scenario(version)).await {
			Ok(Ok(())) => {}
			Ok(Err(err)) => failures.push(format!("{version}: {err}")),
			Err(_) => failures.push(format!("{version}: scenario timed out")),
		}
	}
	assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// `P -> A -> R` is serving a finished catalog and `P -> B -> R` is standing by.
/// `A` dies. A new reader on `R` has to receive the snapshot `P` still holds.
#[moq_net_sim::test]
async fn a_quiet_catalog_reaches_a_late_reader_after_its_route_dies() {
	each_version(VERSIONS, |version| async move {
		let publisher = produce_origin(1);
		let relay_a = produce_origin(2);
		let relay_b = produce_origin(3);
		let subscriber = produce_origin(4);
		let _published = publish(&publisher);

		let p_a = link(version, &publisher, &relay_a).await;
		let _p_b = link(version, &publisher, &relay_b).await;
		let a_r = link(version, &relay_a, &subscriber).await;

		let remote = request(&subscriber).await;
		// Stays subscribed, so the relay resumes rather than parking an idle cache.
		let _early = read_snapshot(&remote)
			.await
			.map_err(|err| format!("early reader: {err}"))?;

		let _b_r = link(version, &relay_b, &subscriber).await;
		settle().await;
		abort(p_a);
		abort(a_r);
		settle().await;

		// An untagged route's broadcast ends with that route. A new reader
		// resolves the surviving route instead of reusing the closed handle.
		let remote = request(&subscriber).await;
		read_snapshot(&remote)
			.await
			.map_err(|err| format!("late reader: {err}"))?;
		Ok(())
	})
	.await;
}

/// A peer resumes one group past a finished catalog before anyone else has
/// fetched it, so the relay's upstream subscription is created past the only
/// group. A fresh reader must still receive that group.
#[moq_net_sim::test]
async fn a_quiet_catalog_reaches_a_fresh_reader_when_a_peer_resumes_past_it() {
	each_version(LITE, |version| async move {
		let publisher = produce_origin(1);
		let relay = produce_origin(2);
		let resuming = produce_origin(3);
		let fresh = produce_origin(4);
		let (_broadcast, mut track) = publish(&publisher);

		let _upstream = link(version, &publisher, &relay).await;
		let _resume_link = link(version, &relay, &resuming).await;
		let _fresh_link = link(version, &relay, &fresh).await;

		let resume_remote = request(&resuming).await;
		// Held until the scenario returns. Dropping it would cancel the upstream
		// subscription and let the fresh reader open a new one at the live edge.
		let _resume = moq_net_sim::spawn(async move {
			let _sub = resume_remote
				.track("catalog.json")
				.unwrap()
				.subscribe(track::Subscription::default().with_start(track::Position::group(1)))
				.await
				.expect("resume subscribe");
			moq_net_sim::sleep(Duration::from_secs(60)).await;
		});

		// Wait for the resume to reach the publisher, so the fresh reader widens it
		// rather than opening its own floorless subscription.
		let resumed = Some(track::Position::group(1));
		moq_net_sim::timeout(TIMEOUT, async {
			while track.subscription().map(|sub| sub.start) != Some(resumed) {
				track.subscription_changed().await.unwrap();
			}
		})
		.await
		.map_err(|_| "the resume never reached the publisher")?;

		let remote = request(&fresh).await;
		read_snapshot(&remote)
			.await
			.map_err(|err| format!("fresh reader: {err}"))?;
		Ok(())
	})
	.await;
}
