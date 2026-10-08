//! A relay's front for a path ends once unread, end to end over real sessions.
//!
//! Two workers each claim the `pool` prefix with `origin::Producer::dynamic` and
//! announce it to relay `R` on their own sessions, the way a transcode pool does. A
//! viewer on `R` reads `pool/p` from worker `A`, which then closes that output and
//! drains its claim. Nothing tells `R` the output closed, so only the front ending
//! once unread lets the next viewer reach worker `B`, which wins the path now, rather
//! than the drained worker.

mod support;

use std::{cell::RefCell, rc::Rc, time::Duration};

use moq_net::{Hop, Timestamp, Version, broadcast, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TEST_TIMEOUT: Duration = Duration::from_secs(600);

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

/// Let every task settle. Time is paused, so this returns once the runtime is idle.
async fn settle() {
	moq_net_sim::sleep(Duration::from_millis(500)).await;
}

/// A worker claiming `pool`, answering each request with an output whose `video`
/// track carries the worker's name.
struct Worker {
	claim: Rc<origin::Dynamic>,
	/// Each output with its track, which would end once its producer drops.
	outputs: Rc<RefCell<Vec<(broadcast::Producer, track::Producer)>>>,
	origin: origin::Producer,
}

impl Worker {
	fn new(hop: u64, name: &'static str, cost: u64) -> Self {
		let origin = produce_origin(hop);
		let claim = Rc::new(
			origin
				.dynamic("pool", origin::Route::default().with_cost(cost))
				.unwrap(),
		);
		let outputs = Rc::new(RefCell::new(Vec::new()));
		let (handler, served) = (claim.clone(), outputs.clone());
		drop(moq_net_sim::spawn(async move {
			while let Ok(request) = handler.requested_broadcast().await {
				let output = broadcast::Info::new().produce();
				let track = output.create_track("video", None).unwrap();
				let mut group = track.append_group().unwrap();
				group.write_frame(Timestamp::ZERO, name.as_bytes().to_vec()).unwrap();
				group.finish().unwrap();
				request.accept(&output);
				served.borrow_mut().push((output, track));
			}
		}));
		Self { claim, outputs, origin }
	}

	/// How many outputs the worker was asked for.
	fn served(&self) -> usize {
		self.outputs.borrow().len()
	}
}

/// Read the first frame of `video` at `pool/p` through `relay`.
async fn watch(relay: &origin::Producer) -> String {
	let broadcast = relay.consume().request_broadcast("pool/p", None).await.unwrap();
	let mut subscription = broadcast.track("video").unwrap().subscribe(None).await.unwrap();
	let mut group = subscription.recv_group().await.unwrap().expect("a group");
	let frame = group.read_frame().await.unwrap().expect("a frame");
	String::from_utf8(frame.payload.to_vec()).unwrap()
}

async fn drained_claim(version: &str) {
	let version: Version = version.parse().unwrap();
	let relay = produce_origin(3);
	let a = Worker::new(1, "a", 0);
	let b = Worker::new(2, "b", 10);
	let _a = link(version, &a.origin, &relay).await;
	let _b = link(version, &b.origin, &relay).await;
	settle().await;

	assert_eq!(watch(&relay).await, "a", "{version}");

	// A closes its output and drains its claim, so B wins the path from here on.
	a.outputs.borrow().iter().for_each(|(output, _)| output.close());
	a.claim.update(a.claim.route().with_cost(origin::Cost::DRAIN)).unwrap();
	// Past the linger, which is as long as the cache window, nothing reads the relay's
	// front for the path.
	moq_net_sim::sleep(moq_net::cache::DEFAULT_EXPIRY * 2).await;

	assert_eq!(
		watch(&relay).await,
		"b",
		"{version}: the relay kept the drained worker's front"
	);
	assert_eq!(a.served(), 1, "{version}: the drained worker was asked again");
	assert_eq!(b.served(), 1, "{version}");
}

#[moq_net_sim::test]
async fn a_drained_claims_front_ends_once_unread_lite06() {
	moq_net_sim::timeout(TEST_TIMEOUT, drained_claim("moq-lite-06"))
		.await
		.expect("timed out");
}

#[moq_net_sim::test]
async fn a_drained_claims_front_ends_once_unread_lite07() {
	moq_net_sim::timeout(TEST_TIMEOUT, drained_claim("moq-lite-07-wip"))
		.await
		.expect("timed out");
}
