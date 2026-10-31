#![cfg(feature = "async")]

//! A worker claiming a prefix answers each request by publishing the exact path under a
//! fresh epoch. On moq-lite-07 that announcement is a takeover of the claim, so the first
//! subscription through the claim keeps reading, across relays. Older wires carry it as
//! a start, and a takeover that another worker could have served is a start too.

use crate::support;

use std::{cell::RefCell, rc::Rc, time::Duration};

use moq_net::{Epoch, Hop, Timestamp, Version, announce, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

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

/// A worker claiming `claim`, answering each request with an output whose `video` track
/// carries the worker's name.
struct Worker {
	_claim: Rc<origin::Requests>,
	outputs: Rc<RefCell<Vec<(origin::Publication, track::Producer)>>>,
	origin: origin::Producer,
}

impl Worker {
	fn new(hop: u64, claim: &str, name: &'static str) -> Self {
		let origin = produce_origin(hop);
		let claim = Rc::new(origin.requests(claim).unwrap());
		let outputs = Rc::new(RefCell::new(Vec::new()));
		let (handler, served) = (claim.clone(), outputs.clone());
		drop(moq_clock::sim::spawn(async move {
			while let Ok(request) = handler.requested_broadcast().await {
				let prepared = request.prepare(Epoch::mint()).unwrap();
				let track = prepared
					.broadcast()
					.create_track(
						"video",
						track::Info::default().with_timescale(moq_net::Timescale::MILLI),
					)
					.unwrap();
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
}

/// Every announce event already delivered, as `(kind, prefix, predecessor)`.
async fn drain(announced: &mut announce::Consumer) -> Vec<(&'static str, String, Option<String>)> {
	let mut events = Vec::new();
	while let Ok(event) = moq_clock::timeout(Duration::from_secs(1), announced.next()).await {
		events.push(match event.expect("announce ended") {
			announce::Event::Start(a) => ("start", a.prefix.to_string(), None),
			announce::Event::Update(a) => ("update", a.prefix.to_string(), None),
			announce::Event::Restart(a) => ("restart", a.prefix.to_string(), None),
			announce::Event::Takeover { announce, predecessor } => {
				("takeover", announce.prefix.to_string(), Some(predecessor.to_string()))
			}
			announce::Event::End(a) => ("end", a.prefix.to_string(), None),
		});
	}
	events
}

/// The first frame of `video` at `path` through `origin`, on the first subscription.
async fn watch(origin: &origin::Producer, path: &str) -> String {
	let broadcast = origin.consume().request(path, None).await.unwrap();
	let mut subscription = broadcast.track("video").unwrap().subscribe(None).await.unwrap();
	let mut group = moq_clock::timeout(Duration::from_secs(10), subscription.recv_group())
		.await
		.expect("subscription hung")
		.expect("the first subscription was reset")
		.expect("a group");
	let frame = group.read_frame().await.unwrap().expect("a frame");
	String::from_utf8(frame.payload.to_vec()).unwrap()
}

fn lite07(version: Version) -> bool {
	version == "moq-lite-07-wip".parse().unwrap()
}

/// `W -> R -> E`: the edge reads through `W`'s claim on its first subscription, and each
/// hop announces the answer as a takeover of the claim on lite-07, a start before it.
async fn chain(version: &str, claim: &str) {
	let version: Version = version.parse().unwrap();
	let worker = Worker::new(1, claim, "w");
	let relay = produce_origin(2);
	let edge = produce_origin(3);
	let _upstream = link(version, &worker.origin, &relay, 1).await;
	let _downstream = link(version, &relay, &edge, 1).await;
	settle().await;

	let mut at_relay = relay.consume().announced();
	let mut at_edge = edge.consume().announced();
	let claimed = [("start", claim.to_string(), None)];
	assert_eq!(drain(&mut at_relay).await, claimed, "{version}");
	assert_eq!(drain(&mut at_edge).await, claimed, "{version}");

	if !lite07(version) {
		// An older wire resets the first subscription through the claim.
		let broadcast = edge.consume().request("pool/p", None).await.unwrap();
		drop(broadcast.track("video").unwrap().subscribe(None).await);
		settle().await;
	}
	assert_eq!(watch(&edge, "pool/p").await, "w", "{version}");
	assert_eq!(worker.outputs.borrow().len(), 1, "{version}");

	let answered = if !lite07(version) {
		// An ordinary announcement, or one replacing the claim at its own path.
		if claim == "pool/p" {
			vec![
				("end", "pool/p".to_string(), None),
				("start", "pool/p".to_string(), None),
			]
		} else {
			vec![("start", "pool/p".to_string(), None)]
		}
	} else {
		vec![("takeover", "pool/p".to_string(), Some(claim.to_string()))]
	};
	assert_eq!(drain(&mut at_relay).await, answered, "{version}: at the relay");
	assert_eq!(drain(&mut at_edge).await, answered, "{version}: at the edge");

	// A late snapshot starts what stands, replaying no takeover.
	let mut late = edge.consume().announced();
	let mut snapshot = vec![("start", "pool/p".to_string(), None)];
	if claim != "pool/p" {
		snapshot.insert(0, ("start", claim.to_string(), None));
	}
	assert_eq!(drain(&mut late).await, snapshot, "{version}: a late snapshot");
}

macro_rules! chain_tests {
	($($name:ident: $version:literal, $claim:literal,)*) => {
		$(
			#[moq_clock::test]
			async fn $name() {
				moq_clock::timeout(TEST_TIMEOUT, chain($version, $claim))
					.await
					.expect("timed out");
			}
		)*
	};
}

chain_tests! {
	beneath_the_claim_lite06: "moq-lite-06", "pool",
	beneath_the_claim_lite07: "moq-lite-07-wip", "pool",
	at_the_claim_lite06: "moq-lite-06", "pool/p",
	at_the_claim_lite07: "moq-lite-07-wip", "pool/p",
}

/// Two workers claim `pool` on `R`. Whichever answers, `R` cannot tell a reader of the
/// claim which worker it saw, so the answer is a start, while `R`'s own reader still
/// keeps reading from the worker it was routed to.
#[moq_clock::test]
async fn two_workers_announce_a_start() {
	moq_clock::timeout(TEST_TIMEOUT, async {
		let version: Version = "moq-lite-07-wip".parse().unwrap();
		let relay = produce_origin(3);
		let a = Worker::new(1, "pool", "a");
		let b = Worker::new(2, "pool", "b");
		let _a = link(version, &a.origin, &relay, 1).await;
		let _b = link(version, &b.origin, &relay, 10).await;
		settle().await;

		let mut announced = relay.consume().announced();
		assert_eq!(drain(&mut announced).await, [("start", "pool".to_string(), None)]);

		assert_eq!(watch(&relay, "pool/p").await, "a");
		assert_eq!(
			drain(&mut announced).await,
			[("start", "pool/p".to_string(), None)],
			"a takeover with another worker's claim visible"
		);
		assert!(b.outputs.borrow().is_empty());
	})
	.await
	.expect("timed out");
}

/// A broadcast published at the path without answering a request is no takeover, even
/// from the worker holding the claim, and a reader through the claim does not inherit it.
#[moq_clock::test]
async fn an_unrequested_publish_is_a_start() {
	moq_clock::timeout(TEST_TIMEOUT, async {
		let version: Version = "moq-lite-07-wip".parse().unwrap();
		let worker = Worker::new(1, "pool", "w");
		let relay = produce_origin(2);
		let _link = link(version, &worker.origin, &relay, 1).await;
		settle().await;

		let mut announced = relay.consume().announced();
		assert_eq!(drain(&mut announced).await, [("start", "pool".to_string(), None)]);

		let _direct = worker.origin.publish("pool/q", origin::Route::default()).unwrap();
		settle().await;
		assert_eq!(drain(&mut announced).await, [("start", "pool/q".to_string(), None)]);
	})
	.await
	.expect("timed out");
}

/// `W -> R` on lite-07 and `R -> E` on lite-06: the relay keeps its reader through the
/// takeover and forwards it to the older edge as a start.
#[moq_clock::test]
async fn an_older_downstream_gets_a_start() {
	moq_clock::timeout(TEST_TIMEOUT, async {
		let worker = Worker::new(1, "pool", "w");
		let relay = produce_origin(2);
		let edge = produce_origin(3);
		let _upstream = link("moq-lite-07-wip".parse().unwrap(), &worker.origin, &relay, 1).await;
		let _downstream = link("moq-lite-06".parse().unwrap(), &relay, &edge, 1).await;
		settle().await;

		let mut at_relay = relay.consume().announced();
		let mut at_edge = edge.consume().announced();
		drain(&mut at_relay).await;
		drain(&mut at_edge).await;

		assert_eq!(watch(&relay, "pool/p").await, "w");
		assert_eq!(
			drain(&mut at_relay).await,
			[("takeover", "pool/p".to_string(), Some("pool".to_string()))]
		);
		assert_eq!(drain(&mut at_edge).await, [("start", "pool/p".to_string(), None)]);
		assert_eq!(watch(&edge, "pool/p").await, "w");
		assert_eq!(worker.outputs.borrow().len(), 1, "the edge asked the worker again");
	})
	.await
	.expect("timed out");
}
