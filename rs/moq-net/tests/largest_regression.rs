//! A relay's lingering copy of a track judges the route's answer when a reader returns.
//!
//! A worker claims the `pool` prefix with `origin::Producer::dynamic`, so relay `R`
//! never sees it close an output. When it serves `pool/p` again, the new output starts
//! over at group 0, below what `R` still caches for a returning reader. Through a route
//! without an epoch that is new content under the old name: the reader gets an error
//! and re-requests, rather than the old output's group and then nothing.
//!
//! Through a route with an epoch the answer may come from a replica that lags behind
//! the copy, so `R` keeps the copy and waits for the replica to catch up.

mod support;

use std::{cell::RefCell, rc::Rc, time::Duration};

use moq_net::{Error, Hop, Timestamp, Version, broadcast, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The versions whose answer carries the route's largest position.
const VERSIONS: &[&str] = &[
	"moq-lite-07-wip",
	"moq-transport-14",
	"moq-transport-16",
	"moq-transport-22",
];

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

/// Write one finished group carrying `payload`.
fn append(track: &track::Producer, payload: String) {
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, payload.into_bytes()).unwrap();
	group.finish().unwrap();
}

/// Subscribe to `video` at `path` through `relay` and read the first group's frame.
async fn watch(relay: &origin::Producer, path: &str) -> Result<(u64, String), Error> {
	let broadcast = relay.consume().request_broadcast(path, None).await?;
	let mut subscription = broadcast.track("video")?.subscribe(None).await?;
	let mut group = subscription.recv_group().await?.ok_or(Error::Dropped)?;
	let frame = group.read_frame().await?.ok_or(Error::Dropped)?;
	Ok((group.sequence, String::from_utf8(frame.payload.to_vec()).unwrap()))
}

/// A worker claiming `pool`: each request gets a new output whose `video` track starts
/// at group 0. The first output writes groups 0 to 2, later ones group 0 only.
struct Worker {
	_claim: Rc<origin::Dynamic>,
	outputs: Rc<RefCell<Vec<(broadcast::Producer, track::Producer)>>>,
	origin: origin::Producer,
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
				let generation = served.borrow().len();
				let groups = if generation == 0 { 3 } else { 1 };
				for sequence in 0..groups {
					append(&track, format!("{generation}:{sequence}"));
				}
				request.accept(&output);
				served.borrow_mut().push((output, track));
			}
		}));
		Self {
			_claim: claim,
			outputs,
			origin,
		}
	}
}

async fn restart_within_the_linger(version: Version) -> Result<(), String> {
	let relay = produce_origin(2);
	let worker = Worker::new(1);
	let _link = link(version, &worker.origin, &relay).await;
	settle().await;

	// A viewer reads the first output's newest group and leaves; the relay keeps the copy.
	match watch(&relay, "pool/p").await {
		Ok((2, payload)) if payload == "0:2" => {}
		other => return Err(format!("the first viewer got {other:?}")),
	}
	settle().await;

	// The worker closes the output, so the next request it gets starts a new one at group 0.
	worker.outputs.borrow().iter().for_each(|(output, _)| output.close());
	settle().await;

	// Back within the linger: the copy still holds the old output's group 2.
	match watch(&relay, "pool/p").await {
		Err(Error::Unroutable) => {}
		other => return Err(format!("the returning viewer got {other:?}")),
	}

	// The re-request reaches the new output from its first group.
	match watch(&relay, "pool/p").await {
		Ok((0, payload)) if payload == "1:0" => {}
		other => return Err(format!("the re-request got {other:?}")),
	}
	match worker.outputs.borrow().len() {
		2 => Ok(()),
		outputs => Err(format!("the worker served {outputs} outputs")),
	}
}

#[moq_net_sim::test]
async fn a_restart_ends_the_lingering_copy() {
	let mut failures = Vec::new();
	for version in VERSIONS {
		let version: Version = version.parse().unwrap();
		match moq_net_sim::timeout(TEST_TIMEOUT, restart_within_the_linger(version)).await {
			Ok(Ok(())) => {}
			Ok(Err(err)) => failures.push(format!("{version}: {err}")),
			Err(_) => failures.push(format!("{version}: timed out")),
		}
	}
	assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// `P1` and `P2` publish `live` under one epoch, and relay `X` pulls both for relay `R`.
/// `R` caches group 3 from `P1`, its reader leaves, and `P1` goes, so `X` serves `P2`,
/// which is only at group 1. A returning reader on `R` hears group 1 as the largest,
/// below the copy, and still gets the cached group and then `P2`'s next ones.
#[moq_net_sim::test]
async fn a_lagging_replica_with_the_epoch_resumes_the_copy() {
	moq_net_sim::timeout(TEST_TIMEOUT, async {
		let version: Version = "moq-lite-07-wip".parse().unwrap();
		let epoch = moq_net::Epoch::mint();
		let relay_x = produce_origin(3);
		let relay_r = produce_origin(4);

		let mut replicas = Vec::new();
		for (name, hop, cost, groups) in [("P1", 1, 1, 4), ("P2", 2, 5, 2)] {
			let publisher = produce_origin(hop);
			let broadcast = publisher.create_broadcast("live").unwrap();
			let track = broadcast.create_track("video", None).unwrap();
			for sequence in 0..groups {
				append(&track, format!("{name}:{sequence}"));
			}
			broadcast
				.announce(origin::Route::default().with_epoch(epoch.clone()).with_cost(cost))
				.unwrap();
			let link = link(version, &publisher, &relay_x).await;
			replicas.push((publisher, broadcast, track, link));
		}
		let _x_r = link(version, &relay_x, &relay_r).await;
		settle().await;

		assert_eq!(watch(&relay_r, "live").await.unwrap(), (3, "P1:3".into()));
		settle().await;

		// `P1` goes, so `X` serves `P2`, and a reader on `X` brings its group 1 in.
		let (_publisher, _broadcast, _track, p1) = replicas.remove(0);
		p1.server.abort(Error::Cancel);
		p1.client.abort(Error::Cancel);
		settle().await;
		let local = relay_x.consume().request_broadcast("live", None).await.unwrap();
		let mut local = local.track("video").unwrap().subscribe(None).await.unwrap();
		assert_eq!(local.recv_group().await.unwrap().unwrap().sequence, 1);

		let broadcast = relay_r.consume().request_broadcast("live", None).await.unwrap();
		let mut subscription = broadcast.track("video").unwrap().subscribe(None).await.unwrap();
		let mut group = subscription.recv_group().await.unwrap().unwrap();
		let frame = group.read_frame().await.unwrap().unwrap();
		assert_eq!((group.sequence, frame.payload.as_ref()), (3, b"P1:3".as_ref()));

		let (_, _, p2, _) = &replicas[0];
		for sequence in 2..5 {
			append(p2, format!("P2:{sequence}"));
		}
		let mut group = subscription.recv_group().await.unwrap().unwrap();
		let frame = group.read_frame().await.unwrap().unwrap();
		assert_eq!((group.sequence, frame.payload.as_ref()), (4, b"P2:4".as_ref()));
	})
	.await
	.expect("timed out");
}
