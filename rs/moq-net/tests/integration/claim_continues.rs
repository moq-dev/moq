#![cfg(feature = "async")]

//! A worker claiming a prefix answers each request by publishing the exact path. The exact
//! route carries the claim's instance while it lives, so on moq-lite-07, which carries
//! instances end to end, the first subscription through the claim continues on it, across
//! relays. Once the job ends the claim moves to a fresh instance, so a restarted job is
//! another stream. Older wires mint an instance per announcement, so their first
//! subscription through the claim sees another broadcast rather than a splice.

use crate::support;
use support::AtStart;
use support::AppendNext;

use std::{cell::RefCell, rc::Rc, time::Duration};

use moq_net::{Error, Instance, Version, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TEST_TIMEOUT: Duration = Duration::from_secs(600);

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer.with_proxy(true)
}

fn lite07() -> Version {
	"moq-lite-07-wip".parse().unwrap()
}

fn lite06() -> Version {
	"moq-lite-06".parse().unwrap()
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
	kio::clock::sleep(Duration::from_millis(500)).await;
}

/// A worker claiming `claim`, answering each request with an output whose `video` track
/// carries the worker's name and how many outputs it made before.
struct Worker {
	claim: Rc<origin::Claim>,
	outputs: Rc<RefCell<Vec<(origin::Route, track::Producer)>>>,
	origin: origin::Producer,
}

impl Worker {
	fn new(hop: u64, claim: &str, name: &'static str) -> Self {
		let origin = produce_origin(hop);
		let claim = Rc::new(origin.claim(claim, origin::RouteOptions::default()).unwrap());
		let outputs = Rc::new(RefCell::new(Vec::new()));
		let (handler, served) = (claim.clone(), outputs.clone());
		drop(kio::clock::sim::spawn(async move {
			while let Some(request) = handler.requested().await {
				let prepared = request.route(origin::RouteOptions::hidden()).unwrap();
				let track = prepared
					.broadcast()
					.create_track(
						"video",
						track::Info::default().with_timescale(moq_net::Timescale::MILLI),
					)
					.unwrap();
				let tag = format!("{name}{}", served.borrow().len());
				let mut group = track.append_next().unwrap();
				group.write_frame(group.at_start(), tag.into_bytes()).unwrap();
				group.close().unwrap();
				prepared.announce().unwrap();
				served.borrow_mut().push((prepared, track));
			}
		}));
		Self { claim, outputs, origin }
	}

	/// How many outputs the worker was asked for.
	fn served(&self) -> usize {
		self.outputs.borrow().len()
	}

	/// End the latest output, as a worker does when the job stops.
	fn stop(&self) {
		let (output, _) = self.outputs.borrow_mut().pop().expect("an output");
		output.broadcast().close();
	}
}

/// Every announce event already delivered, as `(kind, prefix, instance)`.
async fn drain(announced: &mut origin::Routes) -> Vec<(&'static str, String, Instance)> {
	let mut events = Vec::new();
	while let Ok(event) = kio::clock::timeout(Duration::from_secs(1), announced.next()).await {
		let (kind, announce) = match event.expect("announce ended") {
			origin::RouteEvent {
				kind: origin::RouteKind::Start,
				announce: a,
			} => ("start", a),
			origin::RouteEvent {
				kind: origin::RouteKind::Update,
				announce: a,
			} => ("update", a),
			origin::RouteEvent {
				kind: origin::RouteKind::Restart,
				announce: a,
			} => ("restart", a),
			origin::RouteEvent {
				kind: origin::RouteKind::End,
				announce: a,
			} => ("end", a),
			origin::RouteEvent {
				kind: origin::RouteKind::Offline,
				announce: a,
			} => ("offline", a),
		};
		events.push((kind, announce.prefix.to_string(), announce.route.instance().clone()));
	}
	events
}

/// The events without their instances.
fn kinds(events: &[(&'static str, String, Instance)]) -> Vec<(&'static str, String)> {
	events.iter().map(|(kind, prefix, _)| (*kind, prefix.clone())).collect()
}

/// The first frame of `video` at `path` through `origin` on the first subscription, or
/// `None` when that subscription was reset before a group arrived.
async fn first(origin: &origin::Producer, path: &str) -> Option<String> {
	let broadcast = origin.consume().request(path, None).await.unwrap();
	let mut subscription = broadcast.track("video").unwrap().subscribe(None).await.ok()?;
	let mut group = kio::clock::timeout(Duration::from_secs(10), subscription.recv_group())
		.await
		.expect("subscription hung")
		.ok()??;
	let frame = group.read_frame().await.unwrap().expect("a frame");
	Some(String::from_utf8(frame.payload.to_vec()).unwrap())
}

/// The first frame at `path` through `origin`. Unless the first subscription through the
/// claim `continues`, it resets, and the next one reads.
async fn watch(continues: bool, origin: &origin::Producer, path: &str) -> String {
	if !continues {
		assert_eq!(first(origin, path).await, None, "the first subscription continued");
		settle().await;
	}
	first(origin, path).await.expect("the subscription was reset")
}

/// `W -> R -> E`: the edge reads through `W`'s claim on its first subscription on lite-07,
/// where every hop announces the answer under the claim's instance. An older wire mints an
/// instance per announcement, so its first subscription resets, unless the claim is at the
/// answer's own path: there the worker's answer is the same stream as the claim, so it
/// announces nothing new.
async fn chain(version: Version, claim: &str) {
	let worker = Worker::new(1, claim, "w");
	let relay = produce_origin(2);
	let edge = produce_origin(3);
	let _upstream = link(version, &worker.origin, &relay, 1).await;
	let _downstream = link(version, &relay, &edge, 1).await;
	settle().await;

	let mut at_relay = relay.consume().routes();
	let mut at_edge = edge.consume().routes();
	let claimed = [("start", claim.to_string())];
	assert_eq!(kinds(&drain(&mut at_relay).await), claimed, "{version}");
	assert_eq!(kinds(&drain(&mut at_edge).await), claimed, "{version}");

	let continues = version == lite07() || claim == "pool/p";
	assert_eq!(watch(continues, &edge, "pool/p").await, "w0", "{version}");
	assert_eq!(worker.served(), 1, "{version}");

	let lent = worker.claim.instance();
	for (at, events) in [
		("relay", drain(&mut at_relay).await),
		("edge", drain(&mut at_edge).await),
	] {
		match claim == "pool/p" {
			// Beneath the claim, the answer is another path.
			false => assert_eq!(
				kinds(&events),
				[("start", "pool/p".to_string())],
				"{version}: at the {at}"
			),
			// At the claim's own path, the answer is the same stream by another route.
			true => assert!(
				events.iter().all(|(kind, _, _)| *kind == "update"),
				"{version}: at the {at}: {events:?}"
			),
		}
		if version == lite07()
			&& let Some((_, _, instance)) = events.last()
		{
			assert_eq!(instance, &lent, "{version}: at the {at}");
		}
	}

	// A late snapshot starts what stands.
	let mut late = edge.consume().routes();
	let mut snapshot = vec![("start", "pool/p".to_string())];
	if claim != "pool/p" {
		snapshot.insert(0, ("start", claim.to_string()));
	}
	assert_eq!(kinds(&drain(&mut late).await), snapshot, "{version}: a late snapshot");
}

macro_rules! chain_tests {
	($($name:ident: $version:expr, $claim:literal,)*) => {
		$(
			#[kio::test]
			async fn $name() {
				kio::clock::timeout(TEST_TIMEOUT, chain($version, $claim))
					.await
					.expect("timed out");
			}
		)*
	};
}

chain_tests! {
	beneath_the_claim_lite06: lite06(), "pool",
	beneath_the_claim_lite07: lite07(), "pool",
	at_the_claim_lite06: lite06(), "pool/p",
	at_the_claim_lite07: lite07(), "pool/p",
}

/// A job that stops moves the claim to a fresh instance, end to end: the next request for
/// the path wakes the worker for another stream, a request pinned to the ended one is
/// refused, and the first subscription through the claim continues on the new job.
#[kio::test]
async fn a_restarted_job_is_another_stream() {
	kio::clock::timeout(TEST_TIMEOUT, async {
		let worker = Worker::new(1, "pool", "w");
		let relay = produce_origin(2);
		let edge = produce_origin(3);
		let _upstream = link(lite07(), &worker.origin, &relay, 1).await;
		let _downstream = link(lite07(), &relay, &edge, 1).await;
		settle().await;

		let mut at_edge = edge.consume().routes();
		drain(&mut at_edge).await;
		assert_eq!(first(&edge, "pool/p").await.as_deref(), Some("w0"));
		let spent = worker.claim.instance();
		drain(&mut at_edge).await;

		worker.stop();
		settle().await;
		let advanced = worker.claim.instance();
		assert_ne!(advanced, spent, "the claim kept the ended job's instance");
		let events = drain(&mut at_edge).await;
		assert!(
			events.contains(&("end", "pool/p".to_string(), spent.clone())),
			"{events:?}"
		);
		assert!(
			events.contains(&("restart", "pool".to_string(), advanced.clone())),
			"{events:?}"
		);

		let pinned = edge.consume().request("pool/p", spent).await.err();
		assert!(matches!(pinned, Some(Error::Unroutable)), "{pinned:?}");
		assert_eq!(worker.served(), 0, "a pinned request woke the worker");

		assert_eq!(first(&edge, "pool/p").await.as_deref(), Some("w0"));
		assert_eq!(worker.served(), 1);
		assert_eq!(worker.outputs.borrow()[0].0.instance(), advanced);
	})
	.await
	.expect("timed out");
}

/// Two workers claim `pool` on `R`, each with its own instance. `R`'s reader resolved through
/// the cheaper one continues on that worker's answer, and the other is never asked.
#[kio::test]
async fn two_workers_keep_their_own_readers() {
	kio::clock::timeout(TEST_TIMEOUT, async {
		let relay = produce_origin(3);
		let a = Worker::new(1, "pool", "a");
		let b = Worker::new(2, "pool", "b");
		let _a = link(lite07(), &a.origin, &relay, 1).await;
		let _b = link(lite07(), &b.origin, &relay, 10).await;
		settle().await;
		assert_ne!(a.claim.instance(), b.claim.instance());

		let mut announced = relay.consume().routes();
		assert_eq!(kinds(&drain(&mut announced).await), [("start", "pool".to_string())]);

		assert_eq!(first(&relay, "pool/p").await.as_deref(), Some("a0"));
		assert_eq!(
			drain(&mut announced).await,
			[("start", "pool/p".to_string(), a.claim.instance())]
		);
		assert_eq!(b.served(), 0);
	})
	.await
	.expect("timed out");
}

/// A broadcast published at the path without answering a request is a stream of its own,
/// even from the worker holding the claim.
#[kio::test]
async fn an_unrequested_publish_is_its_own_stream() {
	kio::clock::timeout(TEST_TIMEOUT, async {
		let worker = Worker::new(1, "pool", "w");
		let relay = produce_origin(2);
		let _link = link(lite07(), &worker.origin, &relay, 1).await;
		settle().await;

		let mut announced = relay.consume().routes();
		assert_eq!(kinds(&drain(&mut announced).await), [("start", "pool".to_string())]);

		let direct = worker.origin.route("pool/q", origin::RouteOptions::default()).unwrap();
		assert_ne!(direct.instance(), worker.claim.instance());
		settle().await;
		assert_eq!(
			drain(&mut announced).await,
			[("start", "pool/q".to_string(), direct.instance())]
		);
	})
	.await
	.expect("timed out");
}

/// `W -> R` on lite-07 and `R -> E` on lite-06: the relay's reader continues on the answer,
/// which the older edge sees as another broadcast, served by the same output.
#[kio::test]
async fn an_older_downstream_sees_another_broadcast() {
	kio::clock::timeout(TEST_TIMEOUT, async {
		let worker = Worker::new(1, "pool", "w");
		let relay = produce_origin(2);
		let edge = produce_origin(3);
		let _upstream = link(lite07(), &worker.origin, &relay, 1).await;
		let _downstream = link(lite06(), &relay, &edge, 1).await;
		settle().await;

		let mut at_relay = relay.consume().routes();
		let mut at_edge = edge.consume().routes();
		drain(&mut at_relay).await;
		drain(&mut at_edge).await;

		assert_eq!(first(&relay, "pool/p").await.as_deref(), Some("w0"));
		assert_eq!(kinds(&drain(&mut at_relay).await), [("start", "pool/p".to_string())]);
		assert_eq!(kinds(&drain(&mut at_edge).await), [("start", "pool/p".to_string())]);
		assert_eq!(first(&edge, "pool/p").await.as_deref(), Some("w0"));
		assert_eq!(worker.served(), 1, "the edge asked the worker again");
	})
	.await
	.expect("timed out");
}

/// `W -> R` on lite-06 and `R -> E` on lite-07: the older hop mints an instance for each
/// announcement, so the edge's first subscription through the claim sees another broadcast
/// rather than a splice, and the next one reads.
#[kio::test]
async fn an_older_upstream_hop_restarts_the_reader() {
	kio::clock::timeout(TEST_TIMEOUT, async {
		let worker = Worker::new(1, "pool", "w");
		let relay = produce_origin(2);
		let edge = produce_origin(3);
		let _upstream = link(lite06(), &worker.origin, &relay, 1).await;
		let _downstream = link(lite07(), &relay, &edge, 1).await;
		settle().await;

		assert_eq!(watch(false, &edge, "pool/p").await, "w0");
		assert_eq!(worker.served(), 1);
	})
	.await
	.expect("timed out");
}

/// `W -> {R1, R2} -> E` on lite-07: both relays carry the claim and its answer under the
/// worker's instance, so the edge's first subscription continues whichever way it routes.
#[kio::test]
async fn a_diamond_continues_the_first_reader() {
	kio::clock::timeout(TEST_TIMEOUT, async {
		let worker = Worker::new(1, "pool", "w");
		let left = produce_origin(2);
		let right = produce_origin(3);
		let edge = produce_origin(4);
		let _links = [
			link(lite07(), &worker.origin, &left, 1).await,
			link(lite07(), &worker.origin, &right, 1).await,
			link(lite07(), &left, &edge, 1).await,
			link(lite07(), &right, &edge, 2).await,
		];
		settle().await;

		assert_eq!(first(&edge, "pool/p").await.as_deref(), Some("w0"));
		assert_eq!(worker.served(), 1);
		let mut late = edge.consume().routes();
		let lent = worker.claim.instance();
		assert_eq!(
			drain(&mut late).await,
			[
				("start", "pool".to_string(), lent.clone()),
				("start", "pool/p".to_string(), lent),
			]
		);
	})
	.await
	.expect("timed out");
}
