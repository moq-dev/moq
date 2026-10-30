#![cfg(feature = "async")]

//! A relay's front for a path ends once unread, end to end over real sessions.
//!
//! Two workers each claim the `pool` prefix with `origin::Producer::requests` and
//! announce it to relay `R` on their own sessions, the way a transcode pool does. A
//! viewer on `R` reads `pool/p` from worker `A`, over the cheaper link, which then
//! closes that output and drains its session. Nothing tells `R` the output closed, so
//! only the front ending once unread lets the next viewer reach worker `B`, which wins
//! the path now, rather than the drained worker.

use crate::support;

use std::{cell::RefCell, rc::Rc, time::Duration};

use moq_net::{Epoch, Hop, Timestamp, Version, goaway::Goaway, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

const TEST_TIMEOUT: Duration = Duration::from_secs(600);

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// Have `to` pull everything `from` publishes, over a link priced at `cost`.
async fn link(version: Version, from: &origin::Producer, to: &origin::Producer, cost: u64) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(from.consume());
	options.client_subscribe = Some(to.clone());
	options.cost = Some(cost);
	connect_mock(options).await
}

/// Let every task settle. Time is paused, so this returns once the runtime is idle.
async fn settle() {
	moq_clock::sleep(Duration::from_millis(500)).await;
}

/// A worker claiming `pool`, answering each request with an output whose `video`
/// track carries the worker's name.
struct Worker {
	_claim: Rc<origin::Requests>,
	/// Each output with its track, which would end once its publication drops.
	outputs: Rc<RefCell<Vec<(origin::Publication, track::Producer)>>>,
	origin: origin::Producer,
}

impl Worker {
	fn new(hop: u64, name: &'static str) -> Self {
		let origin = produce_origin(hop);
		let claim = Rc::new(origin.requests("pool").unwrap());
		let outputs = Rc::new(RefCell::new(Vec::new()));
		let (handler, served) = (claim.clone(), outputs.clone());
		drop(moq_clock::sim::spawn(async move {
			while let Ok(request) = handler.requested_broadcast().await {
				let prepared = request.prepare(Epoch::mint()).unwrap();
				let track = prepared.broadcast().create_track("video", timed()).unwrap();
				let mut group = track.append_group().unwrap();
				group.write_frame(Timestamp::ZERO, name.as_bytes().to_vec()).unwrap();
				group.finish().unwrap();
				served.borrow_mut().push((prepared.publish().unwrap(), track));
			}
		}));
		Self {
			_claim: claim,
			outputs,
			origin,
		}
	}

	/// How many outputs the worker was asked for.
	fn served(&self) -> usize {
		self.outputs.borrow().len()
	}
}

/// Read the first frame of `video` at `pool/p` through `relay`. The first subscription
/// through a claim is reset once the answer's exact route arrives, the cold-start reset
/// accepted until announce takeover lands, so the read retries once.
async fn watch(relay: &origin::Producer) -> String {
	let cold = relay.consume().request("pool/p", None).await.unwrap();
	drop(cold.track("video").unwrap().subscribe(None).await);
	settle().await;
	let broadcast = relay.consume().request("pool/p", None).await.unwrap();
	let mut subscription = broadcast.track("video").unwrap().subscribe(None).await.unwrap();
	let mut group = subscription.recv_group().await.unwrap().expect("a group");
	let frame = group.read_frame().await.unwrap().expect("a frame");
	String::from_utf8(frame.payload.to_vec()).unwrap()
}

async fn drained_claim(version: &str) {
	let version: Version = version.parse().unwrap();
	let relay = produce_origin(3);
	let a = Worker::new(1, "a");
	let b = Worker::new(2, "b");
	let a_link = link(version, &a.origin, &relay, 1).await;
	let _b = link(version, &b.origin, &relay, 10).await;
	settle().await;

	assert_eq!(watch(&relay).await, "a", "{version}");

	// A closes its output and drains its session, so B wins the path from here on.
	a.outputs.borrow().iter().for_each(|(output, _)| output.close());
	a_link.server.drain().send(Goaway::new()).unwrap();
	// Past the linger, which is as long as the cache window, nothing reads the relay's
	// front for the path.
	moq_clock::sleep(moq_net::cache::DEFAULT_EXPIRY * 2).await;

	assert_eq!(
		watch(&relay).await,
		"b",
		"{version}: the relay kept the drained worker's front"
	);
	assert_eq!(a.served(), 1, "{version}: the drained worker was asked again");
	assert_eq!(b.served(), 1, "{version}");
}

#[moq_clock::test]
async fn a_drained_claims_front_ends_once_unread_lite06() {
	moq_clock::timeout(TEST_TIMEOUT, drained_claim("moq-lite-06"))
		.await
		.expect("timed out");
}

#[moq_clock::test]
async fn a_drained_claims_front_ends_once_unread_lite07() {
	moq_clock::timeout(TEST_TIMEOUT, drained_claim("moq-lite-07-wip"))
		.await
		.expect("timed out");
}
