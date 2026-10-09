//! A replaced broadcast restarts announce consumers across relays, end to end over
//! real sessions, while the subscriptions already on it stay.
//!
//! On moq-lite-07 the restart travels as ANNOUNCE_RESTART; older versions and
//! moq-transport carry it as an end and a fresh start. Either way a downstream relay
//! retires its copy of the old source, so a new subscriber never sees the old
//! instance's cached groups.

mod support;

use std::{cell::RefCell, rc::Rc, time::Duration};

use moq_net::{Hop, Timestamp, Version, announce, broadcast, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TEST_TIMEOUT: Duration = Duration::from_secs(60);

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// Have `to` pull everything `from` publishes.
async fn link(version: Version, from: &origin::Producer, to: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(from.consume());
	options.client_subscribe = Some(to.clone());
	connect_mock(options).await
}

/// Let every task settle, past the update hold. Time is paused, so this returns once
/// the runtime is idle.
async fn settle() {
	moq_net_sim::sleep(Duration::from_millis(500)).await;
}

/// The next announce event, failing on a hang.
async fn next_event(announced: &mut announce::Consumer) -> announce::Event {
	moq_net_sim::timeout(Duration::from_secs(10), announced.next())
		.await
		.expect("announce hung")
		.expect("announce ended")
}

/// Expect the restart of `prefix` as `version` carries it: one restart on lite-07, an
/// end then a start elsewhere.
async fn expect_restart(version: Version, announced: &mut announce::Consumer, prefix: &str) {
	let restarts = version == "moq-lite-07-wip".parse().unwrap();
	match next_event(announced).await {
		announce::Event::Restart(announce) if restarts => assert_eq!(announce.prefix.as_str(), prefix),
		announce::Event::End(announce) if !restarts => {
			assert_eq!(announce.prefix.as_str(), prefix);
			match next_event(announced).await {
				announce::Event::Start(announce) => assert_eq!(announce.prefix.as_str(), prefix),
				other => panic!("{version}: expected the start, got {other:?}"),
			}
		}
		other => panic!("{version}: expected a restart of {prefix}, got {other:?}"),
	}
}

/// Write one group of one frame tagged `tag`.
fn write(track: &track::Producer, tag: &str) {
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, tag.as_bytes().to_vec()).unwrap();
	group.finish().unwrap();
}

/// The next frame a subscription delivers, as text.
async fn read(subscription: &mut track::Subscriber) -> String {
	let mut group = moq_net_sim::timeout(Duration::from_secs(10), subscription.recv_group())
		.await
		.expect("subscription hung")
		.unwrap()
		.expect("subscription ended");
	let frame = group.read_frame().await.unwrap().expect("a frame");
	String::from_utf8(frame.payload.to_vec()).unwrap()
}

async fn subscribe(origin: &origin::Producer, path: &str) -> (broadcast::Consumer, track::Subscriber) {
	let broadcast = origin.consume().request_broadcast(path, None).await.unwrap();
	let subscription = broadcast.track("video").unwrap().subscribe(None).await.unwrap();
	(broadcast, subscription)
}

/// `P -> R -> V`, where a publisher at `P` without an epoch restarts while the old one
/// lingers: `V` restarts the path, its old subscription keeps the old instance, and a
/// re-request reaches the new one without any of the old instance's cached groups.
async fn relay_chain(version: &str) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let viewer = produce_origin(3);
	let _upstream = link(version, &publisher, &relay).await;
	let _downstream = link(version, &relay, &viewer).await;

	let old = publisher.publish("live", origin::Route::default()).unwrap();
	let old_track = old.create_track("video", None).unwrap();
	let mut announced = viewer.consume().announced();
	match next_event(&mut announced).await {
		announce::Event::Start(announce) => assert_eq!(announce.prefix.as_str(), "live"),
		other => panic!("{version}: expected the route, got {other:?}"),
	}

	let (stale, mut sticky) = subscribe(&viewer, "live").await;
	for group in 0..3 {
		write(&old_track, &format!("old:{group}"));
		assert_eq!(read(&mut sticky).await, format!("old:{group}"), "{version}");
	}

	// The publisher restarts without an epoch, its old session still open.
	let new = publisher.publish("live", origin::Route::default()).unwrap();
	let new_track = new.create_track("video", None).unwrap();
	expect_restart(version, &mut announced, "live").await;

	// The old subscription stays on the old instance.
	write(&old_track, "old:3");
	assert_eq!(read(&mut sticky).await, "old:3", "{version}");

	// A re-request reaches the new instance, never a group cached from the old one.
	let (fresh, mut subscription) = subscribe(&viewer, "live").await;
	assert!(!fresh.is_clone(&stale), "{version}: joined the replaced broadcast");
	write(&new_track, "new:0");
	assert_eq!(read(&mut subscription).await, "new:0", "{version}");
}

macro_rules! relay_chain_tests {
	($($name:ident: $version:literal,)*) => {
		$(
			#[moq_net_sim::test]
			async fn $name() {
				moq_net_sim::timeout(TEST_TIMEOUT, relay_chain($version))
					.await
					.expect("timed out");
			}
		)*
	};
}

relay_chain_tests! {
	relay_chain_lite06: "moq-lite-06",
	relay_chain_lite07: "moq-lite-07-wip",
	relay_chain_ietf19: "moq-transport-19",
	relay_chain_ietf22: "moq-transport-22",
}

/// A worker claiming `pool` without an epoch, answering each request with an output
/// whose `video` track carries the worker's name.
struct Worker {
	origin: origin::Producer,
	_claim: Rc<origin::Dynamic>,
	_outputs: Rc<RefCell<Vec<(broadcast::Producer, track::Producer)>>>,
}

impl Worker {
	fn new(hop: u64) -> Self {
		let origin = produce_origin(hop);
		let claim = Rc::new(origin.dynamic("pool", origin::Route::default()).unwrap());
		let outputs = Rc::new(RefCell::new(Vec::new()));
		let (handler, served) = (claim.clone(), outputs.clone());
		drop(moq_net_sim::spawn(async move {
			while let Ok(request) = handler.requested_broadcast().await {
				let output = broadcast::Info::new().produce();
				let track = output.create_track("video", None).unwrap();
				write(&track, &format!("{hop}"));
				request.accept(&output);
				served.borrow_mut().push((output, track));
			}
		}));
		Self {
			origin,
			_claim: claim,
			_outputs: outputs,
		}
	}
}

/// Workers `10` and `11` claim `pool` behind `U -> D`. Worker `12` joining moves
/// `pool/job-2` from `10` to itself while `11` keeps winning `pool` itself and
/// `pool/job-0`, and `10` keeps `pool/job-1` (pinned by the spread hash). `D` restarts
/// `pool`, and the next subscribes reach the right workers.
async fn prefix_pool(version: &str) {
	let version: Version = version.parse().unwrap();
	let upstream = produce_origin(2);
	let downstream = produce_origin(3);
	let _down = link(version, &upstream, &downstream).await;
	let workers: Vec<Worker> = [10, 11, 12].into_iter().map(Worker::new).collect();
	let mut links = Vec::new();
	for worker in &workers[..2] {
		links.push(link(version, &worker.origin, &upstream).await);
	}
	settle().await;

	let mut announced = downstream.consume().announced();
	match next_event(&mut announced).await {
		announce::Event::Start(announce) => assert_eq!(announce.prefix.as_str(), "pool"),
		other => panic!("{version}: expected the pool, got {other:?}"),
	}

	let mut held = Vec::new();
	for (path, worker) in [("pool/job-0", "11"), ("pool/job-1", "10"), ("pool/job-2", "10")] {
		let (broadcast, mut subscription) = subscribe(&downstream, path).await;
		assert_eq!(read(&mut subscription).await, worker, "{version}: {path}");
		held.push((broadcast, subscription));
	}

	links.push(link(version, &workers[2].origin, &upstream).await);
	expect_restart(version, &mut announced, "pool").await;

	for (path, worker) in [("pool/job-0", "11"), ("pool/job-1", "10"), ("pool/job-2", "12")] {
		let (_broadcast, mut subscription) = subscribe(&downstream, path).await;
		assert_eq!(read(&mut subscription).await, worker, "{version}: {path}");
	}
}

#[moq_net_sim::test]
async fn prefix_pool_lite06() {
	moq_net_sim::timeout(TEST_TIMEOUT, prefix_pool("moq-lite-06"))
		.await
		.expect("timed out");
}

#[moq_net_sim::test]
async fn prefix_pool_lite07() {
	moq_net_sim::timeout(TEST_TIMEOUT, prefix_pool("moq-lite-07-wip"))
		.await
		.expect("timed out");
}
