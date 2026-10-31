//! A replaced broadcast restarts announce consumers across relays, end to end over
//! real sessions, while the subscriptions already on it stay.
//!
//! On moq-lite-07 the restart travels as ANNOUNCE_RESTART, and on moq-transport between
//! two epochs as a new advertisement before the old one's withdrawal (MoQ Epoch); older
//! versions and anything epochless carry it as an end and a fresh start. Either way a downstream relay
//! retires its copy of the old source, so a new subscriber never sees the old
//! instance's cached groups.

use crate::support;

use std::{cell::RefCell, rc::Rc, time::Duration};

use moq_net::{Epoch, Hop, Timestamp, Version, announce, broadcast, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

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
	moq_clock::sleep(Duration::from_millis(500)).await;
}

/// The next announce event, failing on a hang.
async fn next_event(announced: &mut announce::Consumer) -> announce::Event {
	moq_clock::timeout(Duration::from_secs(10), announced.next())
		.await
		.expect("announce hung")
		.expect("announce ended")
}

/// Expect the restart of `prefix` without any epoch, as `version` carries it: one restart
/// on lite-07, an end then a start elsewhere.
async fn expect_restart(version: Version, announced: &mut announce::Consumer, prefix: &str) {
	expect_replaced(
		version,
		announced,
		prefix,
		version == "moq-lite-07-wip".parse().unwrap(),
	)
	.await
}

/// Expect the restart of `prefix` as one restart when the wire replaced the instance in
/// place, so the path was never absent, or as an end then a start.
async fn expect_replaced(version: Version, announced: &mut announce::Consumer, prefix: &str, restarts: bool) {
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

/// Nothing more arrives, past the update hold.
async fn quiet(version: Version, announced: &mut announce::Consumer) {
	if let Ok(event) = moq_clock::timeout(Duration::from_secs(1), announced.next()).await {
		panic!("{version}: expected nothing more, got {event:?}");
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
	let mut group = moq_clock::timeout(Duration::from_secs(10), subscription.recv_group())
		.await
		.expect("subscription hung")
		.unwrap()
		.expect("subscription ended");
	let frame = group.read_frame().await.unwrap().expect("a frame");
	String::from_utf8(frame.payload.to_vec()).unwrap()
}

async fn subscribe(origin: &origin::Producer, path: &str) -> (broadcast::Consumer, track::Subscriber) {
	let broadcast = origin.consume().request(path, None).await.unwrap();
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
	let old_track = old.create_track("video", timed()).unwrap();
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
	let new_track = new.create_track("video", timed()).unwrap();
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
			#[moq_clock::test]
			async fn $name() {
				moq_clock::timeout(TEST_TIMEOUT, relay_chain($version))
					.await
					.expect("timed out");
			}
		)*
	};
}

relay_chain_tests! {
	relay_chain_lite06: "moq-lite-06",
	relay_chain_lite07: "moq-lite-07-wip",
	relay_chain_ietf18: "moq-transport-18",
	relay_chain_ietf22: "moq-transport-22",
}

/// A worker claiming `pool` without an epoch, answering each request with an output
/// whose `video` track carries the worker's name. The claim is a peer's, landed through
/// the hidden `fuzz` module as a session lands it, so its answers carry no exact route.
struct Worker {
	origin: origin::Producer,
	_claim: Rc<origin::Requests>,
	_outputs: Rc<RefCell<Vec<(broadcast::Producer, track::Producer)>>>,
}

impl Worker {
	fn new(hop: u64) -> Self {
		let origin = produce_origin(hop);
		let claim = Rc::new(
			moq_net::fuzz::requests_route(&origin, moq_net::Path::from_static("pool"), origin::Route::default())
				.unwrap(),
		);
		let outputs = Rc::new(RefCell::new(Vec::new()));
		let (handler, served) = (claim.clone(), outputs.clone());
		drop(moq_clock::sim::spawn(async move {
			while let Ok(request) = handler.requested_broadcast().await {
				let output = broadcast::Info::new().produce();
				let track = output.create_track("video", timed()).unwrap();
				write(&track, &format!("{hop}"));
				moq_net::fuzz::accept(request, &output);
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

#[moq_clock::test]
async fn prefix_pool_lite06() {
	moq_clock::timeout(TEST_TIMEOUT, prefix_pool("moq-lite-06"))
		.await
		.expect("timed out");
}

#[moq_clock::test]
async fn prefix_pool_lite07() {
	moq_clock::timeout(TEST_TIMEOUT, prefix_pool("moq-lite-07-wip"))
		.await
		.expect("timed out");
}

/// A claim at `live` whose handler answers each request with a fresh output, kept so
/// the test can write into it: `outputs[n]` answered the `n`th request. Only a peer's
/// claim carries an epoch or re-prices, so it lands through the hidden `fuzz` module as
/// a session lands it.
struct Claim {
	dynamic: Rc<origin::Requests>,
	outputs: Rc<RefCell<Vec<(broadcast::Producer, track::Producer)>>>,
}

impl Claim {
	fn new(origin: &origin::Producer, epoch: Option<Epoch>) -> Self {
		let mut route = origin::Route::default();
		route.epoch = epoch;
		let dynamic =
			Rc::new(moq_net::fuzz::requests_route(origin, moq_net::Path::from_static("live"), route).unwrap());
		let outputs = Rc::new(RefCell::new(Vec::new()));
		let (handler, served) = (dynamic.clone(), outputs.clone());
		drop(moq_clock::sim::spawn(async move {
			while let Ok(request) = handler.requested_broadcast().await {
				let output = broadcast::Info::new().produce();
				let track = output.create_track("video", timed()).unwrap();
				moq_net::fuzz::accept(request, &output);
				served.borrow_mut().push((output, track));
			}
		}));
		Self { dynamic, outputs }
	}

	/// Write `tag` into the output that answered the `n`th request.
	fn write(&self, n: usize, tag: &str) {
		write(&self.outputs.borrow()[n].1, tag);
	}

	fn answered(&self) -> usize {
		self.outputs.borrow().len()
	}

	/// Move the claim to `epoch` in place, keeping the rest of its route.
	fn update_epoch(&self, epoch: Option<Epoch>) {
		let mut route = self.dynamic.route();
		route.epoch = epoch;
		self.update_route(route);
	}

	fn update_route(&self, route: origin::Route) {
		moq_net::fuzz::update_route(&self.dynamic, route).unwrap();
	}
}

/// `P -> R -> V`, where `P` claims `live` at `from` and updates the claim in place.
/// A same-epoch re-price keeps every handle. Moving to `to` restarts `V`, the
/// subscription already open stays on the old answer, and a re-request asks the
/// handler again and reads none of the old answer's cached groups.
async fn dynamic_epoch(version: &str, from: Option<Epoch>, to: Option<Epoch>) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let viewer = produce_origin(3);
	let _upstream = link(version, &publisher, &relay).await;
	let _downstream = link(version, &relay, &viewer).await;

	let claim = Claim::new(&publisher, from.clone());
	let mut announced = viewer.consume().announced();
	match next_event(&mut announced).await {
		announce::Event::Start(announce) => assert_eq!(announce.prefix.as_str(), "live"),
		other => panic!("{version}: expected the claim, got {other:?}"),
	}

	let (stale, mut sticky) = subscribe(&viewer, "live/cam").await;
	claim.write(0, "old:0");
	assert_eq!(read(&mut sticky).await, "old:0", "{version}");

	// A re-price at the same epoch is an update at most: every handle survives.
	claim.update_route(claim.dynamic.route().with_cost(5));
	while let Ok(event) = moq_clock::timeout(Duration::from_secs(1), announced.next()).await {
		assert!(
			matches!(event, Some(announce::Event::Update(_))),
			"{version}: a re-price delivered {event:?}"
		);
	}
	claim.write(0, "old:1");
	assert_eq!(read(&mut sticky).await, "old:1", "{version}");
	let (same, _) = subscribe(&viewer, "live/cam").await;
	assert!(same.is_clone(&stale), "{version}: a re-price replaced the broadcast");
	assert_eq!(claim.answered(), 1, "{version}: a re-price asked the handler again");

	// Another epoch, or none, is another instance downstream. lite-07 restarts it in place,
	// and so does moq-transport (MoQ Epoch) between two epochs: the new instance is
	// advertised before the old is withdrawn, so the path is never absent.
	let lite = |name: &str| version == name.parse().unwrap();
	let in_place = lite("moq-lite-07-wip") || !lite("moq-lite-06") && from.is_some() && to.is_some();
	claim.update_epoch(to.clone());
	expect_replaced(version, &mut announced, "live", in_place).await;

	// The subscription already open stays on the old answer.
	claim.write(0, "old:2");
	assert_eq!(read(&mut sticky).await, "old:2", "{version}");

	// A re-request asks the handler again and never reads the old answer.
	let (fresh, mut subscription) = subscribe(&viewer, "live/cam").await;
	assert!(!fresh.is_clone(&stale), "{version}: joined the old instance's copy");
	assert_eq!(claim.answered(), 2, "{version}: the old answer was served again");
	// Every wire here but lite-06 carries the epoch: lite-07, and moq-transport with the
	// MoQ Epoch extension.
	if version != "moq-lite-06".parse().unwrap() {
		assert_eq!(fresh.info().epoch, to, "{version}: resolved under the old epoch");
	}
	claim.write(1, "new:0");
	assert_eq!(read(&mut subscription).await, "new:0", "{version}");

	// The sticky subscription's path resolving through the new route is the restart
	// already delivered, not another.
	quiet(version, &mut announced).await;
}

/// The epoch the claim starts at, and a newer one it moves to.
fn epochs() -> (Epoch, Epoch) {
	(
		"01900000-0000-7000-8000-000000000001".parse().unwrap(),
		"01900000-0000-7000-8000-000000000002".parse().unwrap(),
	)
}

macro_rules! dynamic_epoch_tests {
	($($name:ident: $version:literal,)*) => {
		$(
			#[moq_clock::test]
			async fn $name() {
				let (a, b) = epochs();
				for (from, to) in [(Some(a.clone()), Some(b.clone())), (Some(a), None), (None, Some(b))] {
					moq_clock::timeout(TEST_TIMEOUT, dynamic_epoch($version, from, to))
						.await
						.expect("timed out");
				}
			}
		)*
	};
}

dynamic_epoch_tests! {
	dynamic_epoch_lite06: "moq-lite-06",
	dynamic_epoch_lite07: "moq-lite-07-wip",
	dynamic_epoch_ietf18: "moq-transport-18",
	dynamic_epoch_ietf22: "moq-transport-22",
}

/// An update does not override route selection: with a second claim still at the
/// epoch, a claim dropping it hands re-requests to the second, which outranks any
/// route without one.
#[moq_clock::test]
async fn dynamic_epoch_second_route_keeps_winning() {
	let (a, _) = epochs();
	let origin = produce_origin(1);
	let first = Claim::new(&origin, Some(a.clone()));
	first.update_route(first.dynamic.route().with_cost(1));
	let second = Claim::new(&origin, Some(a.clone()));
	second.update_route(second.dynamic.route().with_cost(5));
	let mut announced = origin.consume().announced();
	match next_event(&mut announced).await {
		announce::Event::Start(announce) => assert_eq!(announce.route.epoch.as_ref(), Some(&a)),
		other => panic!("expected the claim, got {other:?}"),
	}

	let (resolved, mut sticky) = subscribe(&origin, "live/cam").await;
	first.write(0, "first:0");
	assert_eq!(read(&mut sticky).await, "first:0");
	assert_eq!(second.answered(), 0, "the costlier claim answered");

	// The epoch still stands through the second claim, so this is no restart, and the
	// instance's subscriptions resume through it.
	first.update_epoch(None);
	while let Ok(event) = moq_clock::timeout(Duration::from_secs(1), announced.next()).await {
		assert!(
			matches!(event, Some(announce::Event::Update(_))),
			"dropping a shared epoch delivered {event:?}"
		);
	}
	let fresh = origin.consume().request("live/cam", None).await.unwrap();
	assert!(fresh.is_clone(&resolved), "the epoch's broadcast was replaced");
	assert_eq!((first.answered(), second.answered()), (1, 1));
	// The same epoch serves the same bytes: group 0 was delivered, so it resumes at 1.
	second.write(0, "first:0");
	second.write(0, "second:1");
	assert_eq!(read(&mut sticky).await, "second:1");
}

/// A held track must not fill an old instance's cache from its replacement.
async fn retired_fetch(version: &str, explicit: bool) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let viewer = produce_origin(2);
	let _session = link(version, &publisher, &viewer).await;
	let (a, b) = epochs();
	let old_route = if explicit {
		origin::Route::default().with_epoch(a)
	} else {
		origin::Route::default()
	};
	let new_route = if explicit {
		origin::Route::default().with_epoch(b)
	} else {
		origin::Route::default()
	};
	let old = publisher.publish("live", old_route).unwrap();
	let old_track = old.create_track("video", timed()).unwrap();
	let mut announced = viewer.consume().announced();
	assert!(matches!(next_event(&mut announced).await, announce::Event::Start(_)));
	let (held, mut subscription) = subscribe(&viewer, "live").await;
	let track = held.track("video").unwrap();
	write(&old_track, "old:0");
	assert_eq!(read(&mut subscription).await, "old:0");

	let replacement = publisher.publish("live", new_route).unwrap();
	let replacement_track = replacement.create_track("video", timed()).unwrap();
	let _late = replacement
		.create_track("late", Some(track::Info::default().with_priority(17)))
		.unwrap();
	write(&replacement_track, "new:0");
	write(&replacement_track, "new:1");
	expect_restart(version, &mut announced, "live").await;

	let fetched = track.fetch_group(1, None).await;
	assert!(
		fetched.is_err(),
		"{version}: retired handle fetched replacement content"
	);
	let mut cached = track.fetch_group(0, None).await.expect("old cache stays readable");
	assert_eq!(cached.read_frame().await.unwrap().unwrap().payload.as_ref(), b"old:0");
	write(&old_track, "old:1");
	assert_eq!(read(&mut subscription).await, "old:1");
	if let Ok(late) = held.track("late")
		&& let Ok(info) = late.info().await
	{
		assert_ne!(info.priority, 17, "{version}: late track resolved replacement");
	}
	// A held query keeps reading the retired instance; a bare handle would not.
	let _reading = track.info();
	drop(subscription);
	settle().await;
	let resumed = track.subscribe(None).await;
	settle().await;
	assert!(
		replacement_track.subscription().is_none(),
		"{version}: resubscribed to replacement"
	);
	drop(resumed);
	let mut cached = track
		.fetch_group(0, None)
		.await
		.expect("old cache survives resubscribe refusal");
	assert_eq!(cached.read_frame().await.unwrap().unwrap().payload.as_ref(), b"old:0");
}

macro_rules! retired_fetch_tests {
	($($name:ident: $version:literal,)*) => { $(
		#[moq_clock::test]
		async fn $name() {
			moq_clock::timeout(TEST_TIMEOUT, retired_fetch($version, false)).await.expect("timed out");
		}
	)* };
}

retired_fetch_tests! {
	retired_fetch_lite05: "moq-lite-05",
	retired_fetch_lite06: "moq-lite-06",
	retired_fetch_lite07: "moq-lite-07-wip",
	retired_fetch_ietf18: "moq-transport-18",
	retired_fetch_ietf22: "moq-transport-22",
}

/// An epochless SUBSCRIBE names no instance, so until the publisher answers it, it may have
/// resolved whichever instance won the path. Unanswered when its instance is replaced, it is
/// refused, even if the publisher had resolved it on the old one, rather than risk filling
/// the old instance from the replacement.
async fn unanswered_takeover(version: &str) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let viewer = produce_origin(2);
	let _session = link(version, &publisher, &viewer).await;
	let old = publisher.publish("live", origin::Route::default()).unwrap();
	// Quiet, so the publisher has nothing to answer the SUBSCRIBE with yet.
	let old_track = old.create_track("video", timed()).unwrap();
	let mut announced = viewer.consume().announced();
	assert!(matches!(next_event(&mut announced).await, announce::Event::Start(_)));
	let (_held, mut subscription) = subscribe(&viewer, "live").await;
	settle().await;
	assert!(
		old_track.subscription().is_some(),
		"{version}: the SUBSCRIBE reached the old instance"
	);

	let replacement = publisher.publish("live", origin::Route::default()).unwrap();
	let replacement_track = replacement.create_track("video", timed()).unwrap();
	write(&replacement_track, "new:0");
	expect_restart(version, &mut announced, "live").await;
	write(&old_track, "old:0");

	let next = moq_clock::timeout(Duration::from_secs(10), subscription.recv_group())
		.await
		.expect("subscription hung");
	match next {
		Err(err) => assert!(matches!(err, moq_net::Error::Unroutable), "{version}: {err}"),
		Ok(group) => panic!(
			"{version}: the replaced instance's SUBSCRIBE went on: {:?}",
			group.map(|group| group.sequence)
		),
	}
}

macro_rules! unanswered_takeover_tests {
	($($name:ident: $version:literal,)*) => { $(
		#[moq_clock::test]
		async fn $name() {
			moq_clock::timeout(TEST_TIMEOUT, unanswered_takeover($version)).await.expect("timed out");
		}
	)* };
}

// moq-transport answers a SUBSCRIBE at once, before any group, and its subscriber already
// waits for that answer.
unanswered_takeover_tests! {
	unanswered_takeover_lite05: "moq-lite-05",
	unanswered_takeover_lite06: "moq-lite-06",
	unanswered_takeover_lite07: "moq-lite-07-wip",
}

#[moq_clock::test]
async fn retired_fetch_explicit_epoch_control() {
	moq_clock::timeout(TEST_TIMEOUT, retired_fetch("moq-lite-07-wip", true))
		.await
		.expect("timed out");
}

#[moq_clock::test]
async fn withdrawal_refuses_new_fetches_but_keeps_open_reads() {
	for version in ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		let version: Version = version.parse().unwrap();
		let publisher = produce_origin(1);
		let viewer = produce_origin(2);
		let _session = link(version, &publisher, &viewer).await;
		let source = publisher.publish("live", origin::Route::default()).unwrap();
		let produced = source.create_track("video", timed()).unwrap();
		let mut announced = viewer.consume().announced();
		assert!(matches!(next_event(&mut announced).await, announce::Event::Start(_)));
		let (held, mut subscription) = subscribe(&viewer, "live").await;
		let track = held.track("video").unwrap();
		write(&produced, "old:0");
		assert_eq!(read(&mut subscription).await, "old:0");
		source.unannounce();
		assert!(matches!(next_event(&mut announced).await, announce::Event::End(_)));
		assert!(track.fetch_group(1, None).await.is_err(), "{version}");
		write(&produced, "old:1");
		assert_eq!(read(&mut subscription).await, "old:1");
	}
}

#[moq_clock::test]
async fn a_more_specific_source_cannot_fill_a_held_covering_track() {
	for name in ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		let version: Version = name.parse().unwrap();
		let publisher = produce_origin(1);
		let viewer = produce_origin(2);
		let _session = link(version, &publisher, &viewer).await;
		let old = Claim::new(&publisher, None);
		let mut announced = viewer.consume().announced();
		assert!(matches!(next_event(&mut announced).await, announce::Event::Start(_)));
		let (held, mut subscription) = subscribe(&viewer, "live/job").await;
		let track = held.track("video").unwrap();
		old.write(0, "old:0");
		assert_eq!(read(&mut subscription).await, "old:0");
		let replacement = publisher.publish("live/job", origin::Route::default()).unwrap();
		let replacement_track = replacement.create_track("video", timed()).unwrap();
		write(&replacement_track, "new:0");
		write(&replacement_track, "new:1");
		assert!(matches!(next_event(&mut announced).await, announce::Event::Start(_)));
		assert!(
			track.fetch_group(1, None).await.is_err(),
			"{version}: covering handle fetched replacement"
		);
		old.write(0, "old:1");
		assert_eq!(read(&mut subscription).await, "old:1");
	}
}

/// A worker claiming `pool` through the public API: each request is prepared under a
/// fresh epoch and published at its exact path, and `outputs[path]` holds the latest.
struct Pool {
	_claim: Rc<origin::Requests>,
	outputs: Rc<RefCell<std::collections::HashMap<String, (origin::Publication, track::Producer)>>>,
	answered: Rc<RefCell<usize>>,
}

impl Pool {
	fn new(origin: &origin::Producer) -> Self {
		let claim = Rc::new(origin.requests("pool").unwrap());
		let outputs = Rc::new(RefCell::new(std::collections::HashMap::new()));
		let answered = Rc::new(RefCell::new(0));
		let (handler, served, count) = (claim.clone(), outputs.clone(), answered.clone());
		drop(moq_clock::sim::spawn(async move {
			while let Ok(request) = handler.requested_broadcast().await {
				let path = request.path().to_string();
				let prepared = request.prepare(Epoch::mint()).unwrap();
				let track = prepared.broadcast().create_track("video", timed()).unwrap();
				let output = prepared.publish().unwrap();
				served.borrow_mut().insert(path, (output, track));
				*count.borrow_mut() += 1;
			}
		}));
		Self {
			_claim: claim,
			outputs,
			answered,
		}
	}

	fn write(&self, path: &str, tag: &str) {
		write(&self.outputs.borrow()[path].1, tag);
	}

	/// End the output at `path`, as a worker does when it restarts that job.
	fn close(&self, path: &str) {
		self.outputs.borrow()[path].0.close();
	}
}

/// Wake the worker for `path` through the claim and subscribe. moq-lite-07 announces the
/// answer as a takeover of the claim, so the first subscription keeps reading. Older wires
/// carry it as an ordinary announcement, which resets the first subscription through the
/// claim, so they wake the worker first and subscribe through the exact route.
async fn claimed(version: Version, origin: &origin::Producer, path: &str) -> (broadcast::Consumer, track::Subscriber) {
	if version != "moq-lite-07-wip".parse().unwrap() {
		let cold = origin.consume().request(path, None).await.unwrap();
		drop(cold.track("video").unwrap().subscribe(None).await);
		settle().await;
	}
	subscribe(origin, path).await
}

/// `W -> R -> E`, where worker `W` claims `pool` and restarts the output at `pool/p`
/// within the cache window. A returning reader at the edge gets the new output from its
/// first group, never a group cached from the old one, while `pool/q` keeps its reader.
async fn claim_restart(version: &str) {
	let version: Version = version.parse().unwrap();
	let worker = produce_origin(1);
	let relay = produce_origin(2);
	let edge = produce_origin(3);
	let _upstream = link(version, &worker, &relay).await;
	let _downstream = link(version, &relay, &edge).await;
	let pool = Pool::new(&worker);
	settle().await;

	let (_sibling, mut sibling) = claimed(version, &edge, "pool/q").await;
	pool.write("pool/q", "q:0");
	assert_eq!(read(&mut sibling).await, "q:0", "{version}");

	let (old, mut first) = claimed(version, &edge, "pool/p").await;
	for group in 0..3 {
		pool.write("pool/p", &format!("old:{group}"));
		assert_eq!(read(&mut first).await, format!("old:{group}"), "{version}");
	}
	drop(first);
	pool.close("pool/p");
	settle().await;

	let (fresh, mut returning) = claimed(version, &edge, "pool/p").await;
	assert!(!fresh.is_clone(&old), "{version}: joined the old output");
	assert_eq!(*pool.answered.borrow(), 3, "{version}: the worker was not asked again");
	pool.write("pool/p", "new:0");
	assert_eq!(read(&mut returning).await, "new:0", "{version}");

	pool.write("pool/q", "q:1");
	assert_eq!(
		read(&mut sibling).await,
		"q:1",
		"{version}: the sibling lost its reader"
	);
}

macro_rules! claim_restart_tests {
	($($name:ident: $version:literal,)*) => {
		$(
			#[moq_clock::test]
			async fn $name() {
				moq_clock::timeout(TEST_TIMEOUT, claim_restart($version))
					.await
					.expect("timed out");
			}
		)*
	};
}

claim_restart_tests! {
	claim_restart_lite05: "moq-lite-05",
	claim_restart_lite06: "moq-lite-06",
	claim_restart_lite07: "moq-lite-07-wip",
	claim_restart_ietf18: "moq-transport-18",
	claim_restart_ietf22: "moq-transport-22",
}

/// A mid-stream priority change on a viewer's subscription reaches the publisher's
/// aggregate, directly or through a relay (`P -> R -> V`), and on an old instance after
/// an epoch switch left the subscription on it.
async fn priority_update(version: &str, relayed: bool, switch: bool) {
	let version: Version = version.parse().unwrap();
	let case = format!("{version} relayed={relayed} switch={switch}");
	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let viewer = produce_origin(3);
	let mut links = Vec::new();
	if relayed {
		links.push(link(version, &publisher, &relay).await);
		links.push(link(version, &relay, &viewer).await);
	} else {
		links.push(link(version, &publisher, &viewer).await);
	}

	let (a, b) = epochs();
	let old = publisher
		.publish("live", origin::Route::default().with_epoch(a))
		.unwrap();
	let old_track = old.create_track("video", timed()).unwrap();
	let mut announced = viewer.consume().announced();
	assert!(
		matches!(next_event(&mut announced).await, announce::Event::Start(_)),
		"{case}"
	);

	let (_held, mut subscription) = subscribe(&viewer, "live").await;
	write(&old_track, "old:0");
	assert_eq!(read(&mut subscription).await, "old:0", "{case}");

	let _new = switch.then(|| {
		publisher
			.publish("live", origin::Route::default().with_epoch(b))
			.unwrap()
	});
	if switch {
		let in_place = version != "moq-lite-06".parse().unwrap();
		expect_replaced(version, &mut announced, "live", in_place).await;
	}

	// Demoted, promoted, then back to the default, each reaching the publisher before the
	// viewer reads again: nothing waits for its next read to forward it.
	for (group, priority) in [-2, 1, 0].into_iter().enumerate() {
		subscription
			.update(subscription.subscription().with_priority(priority))
			.unwrap();
		settle().await;
		let seen = old_track.subscription().map(|subscription| subscription.priority);
		assert_eq!(seen, Some(priority), "{case}: the publisher saw {seen:?}");
		let tag = format!("old:{}", group + 1);
		write(&old_track, &tag);
		assert_eq!(read(&mut subscription).await, tag, "{case}");
	}
}

macro_rules! priority_update_tests {
	($($name:ident: $version:literal,)*) => { $(
		#[moq_clock::test]
		async fn $name() {
			for relayed in [false, true] {
				for switch in [false, true] {
					moq_clock::timeout(TEST_TIMEOUT, priority_update($version, relayed, switch))
						.await
						.expect("timed out");
				}
			}
		}
	)* };
}

priority_update_tests! {
	priority_update_lite06: "moq-lite-06",
	priority_update_lite07: "moq-lite-07-wip",
	priority_update_ietf18: "moq-transport-18",
	priority_update_ietf22: "moq-transport-22",
}
