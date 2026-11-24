#![cfg(feature = "async")]

//! A session answered through a mounted handle presents the target's broadcasts under
//! the mount point on every version, hides what the origin holds at the mount point
//! itself, and a reader there reaches the target.

use crate::support;
use support::AppendNext;
use support::AtStart;

use std::time::Duration;

use moq_net::{Prefixes, Version, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TEST_TIMEOUT: Duration = Duration::from_secs(600);

fn produce_origin(hop: u64) -> origin::Producer {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.proxy = true;
	let (producer, driver) = origin::Producer::new(config);
	support::harness::spawn(driver);
	producer
}

/// Publish a broadcast at `path` whose `video` track carries the path.
fn publish(origin: &origin::Producer, path: &str) -> (origin::Route, track::Producer) {
	let route = origin.route(path, origin::RouteOptions::default()).unwrap();
	let track = route
		.broadcast()
		.create_track(
			"video",
			track::Info::default().with_timescale(moq_net::Timescale::MILLI),
		)
		.unwrap();
	let mut group = track.append_next().unwrap();
	group.write_frame(group.at_start(), path.as_bytes().to_vec()).unwrap();
	group.finish().unwrap();
	(route, track)
}

const PUBLISHED: [&str; 4] = [
	"tenant-a/public/cam",
	"tenant-a/private/mic",
	"tenant-b/own",
	"tenant-b/shared/forged",
];

/// Tenant B's handle rooted at `root`, with tenant A's public broadcasts mounted at
/// `tenant-b/shared`.
fn tenant(origin: &origin::Producer, root: &str) -> origin::Consumer {
	origin
		.mount("tenant-b/shared", "tenant-a/public")
		.unwrap()
		.scope(root, &Prefixes::all())
		.unwrap()
		.consume()
}

/// Have `to` read `from` through a session.
async fn link(version: Version, from: origin::Consumer, to: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(from);
	options.client_subscribe = Some(to.clone());
	connect_mock(options).await
}

/// Every announce event already delivered, as `(started, prefix)`.
async fn drain(routes: &mut origin::Routes) -> Vec<(bool, String)> {
	let mut events = Vec::new();
	while let Ok(event) = kio::clock::timeout(Duration::from_secs(1), routes.next()).await {
		match event.expect("announce ended") {
			origin::RouteEvent {
				kind: origin::RouteKind::Start,
				announce,
				..
			} => events.push((true, announce.prefix.to_string())),
			origin::RouteEvent {
				kind: origin::RouteKind::End,
				announce,
				..
			} => events.push((false, announce.prefix.to_string())),
			origin::RouteEvent {
				kind: origin::RouteKind::Offline,
				announce,
				..
			} => events.push((false, format!("offline {}", announce.prefix))),
			_ => {}
		}
	}
	events.sort();
	events
}

/// The first frame of `video` at `path` through `origin`.
async fn watch(origin: &origin::Producer, path: &str) -> String {
	let broadcast = origin.consume().request(path, None).await.unwrap();
	let mut subscription = broadcast.track("video").unwrap().subscribe(None).await.unwrap();
	let mut group = kio::clock::timeout(Duration::from_secs(10), subscription.recv_group())
		.await
		.expect("subscription hung")
		.expect("the subscription was reset")
		.expect("a group");
	let frame = group.read_frame().await.unwrap().expect("a frame");
	String::from_utf8(frame.payload.to_vec()).unwrap()
}

fn started(paths: &[&str]) -> Vec<(bool, String)> {
	paths.iter().map(|path| (true, path.to_string())).collect()
}

fn ended(paths: &[&str]) -> Vec<(bool, String)> {
	paths.iter().map(|path| (false, path.to_string())).collect()
}

/// What a reader on `version` sees once `paths` leave with the session that carried
/// them, which no publisher withdrew: offline where the route layer carries it, ended
/// where only an ANNOUNCE says it went.
fn lost(version: Version, paths: &[&str]) -> Vec<(bool, String)> {
	match version.to_string().as_str() {
		"moq-lite-07-wip" => paths.iter().map(|path| (false, format!("offline {path}"))).collect(),
		_ => ended(paths),
	}
}

async fn mounted(version: &str) {
	let version: Version = version.parse().unwrap();
	let source = produce_origin(1);
	let viewer = produce_origin(2);
	let mut published: Vec<_> = PUBLISHED.into_iter().map(|path| publish(&source, path)).collect();
	let _pair = link(version, tenant(&source, "tenant-b"), &viewer).await;
	kio::clock::sleep(Duration::from_millis(500)).await;

	let mut routes = viewer.consume().routes();
	assert_eq!(drain(&mut routes).await, started(&["own", "shared/cam"]), "{version}");
	assert_eq!(watch(&viewer, "shared/cam").await, "tenant-a/public/cam", "{version}");

	// What leaves the target leaves the mount.
	drop(published.remove(0));
	assert_eq!(drain(&mut routes).await, ended(&["shared/cam"]), "{version}");
}

/// A session rooted inside the mount reads only the target.
async fn rooted_inside(version: &str) {
	let version: Version = version.parse().unwrap();
	let source = produce_origin(1);
	let viewer = produce_origin(2);
	let _published: Vec<_> = PUBLISHED.into_iter().map(|path| publish(&source, path)).collect();
	let _pair = link(version, tenant(&source, "tenant-b/shared"), &viewer).await;
	kio::clock::sleep(Duration::from_millis(500)).await;

	let mut routes = viewer.consume().routes();
	assert_eq!(drain(&mut routes).await, started(&["cam"]), "{version}");
	assert_eq!(watch(&viewer, "cam").await, "tenant-a/public/cam", "{version}");
}

/// `W -> R -> V`: the relay learns the target from upstream and presents it under the
/// mount, and lets it go with the upstream.
async fn relayed(version: &str) {
	let version: Version = version.parse().unwrap();
	let worker = produce_origin(1);
	let relay = produce_origin(2);
	let viewer = produce_origin(3);
	let _published: Vec<_> = PUBLISHED.into_iter().map(|path| publish(&worker, path)).collect();
	let upstream = link(version, worker.consume(), &relay).await;
	let _downstream = link(version, tenant(&relay, "tenant-b"), &viewer).await;
	kio::clock::sleep(Duration::from_millis(500)).await;

	let mut routes = viewer.consume().routes();
	assert_eq!(drain(&mut routes).await, started(&["own", "shared/cam"]), "{version}");
	assert_eq!(watch(&viewer, "shared/cam").await, "tenant-a/public/cam", "{version}");

	drop(upstream);
	kio::clock::sleep(Duration::from_millis(500)).await;
	assert_eq!(
		drain(&mut routes).await,
		lost(version, &["own", "shared/cam"]),
		"{version}"
	);
}

/// `W -> R -> V`: `W`'s worker claims `tenant-a`, which covers the mount's target, and
/// answers a request for `tenant-a/public/cam` with that exact path under the claim's
/// instance. `V` reads `R` through tenant B's mounted handle, or plain when `mounted` is
/// false, and asks for the camera by the name its handle gives it.
///
/// Proves the answer arrives beneath the mount point exactly as it does unmounted, as
/// `claim_continues::chain` pins it: the claim starts at the mount point, the answer
/// starts beneath it under the lent instance, and the first subscription continues on
/// lite-07 while an older wire's resets once.
async fn claimed(version: &str, mounted: bool) {
	use crate::claim_continues::{Worker, drain, kinds, settle, watch};

	let version: Version = version.parse().unwrap();
	let worker = Worker::new(1, "tenant-a", "w");
	let relay = produce_origin(2);
	let viewer = produce_origin(3);
	let _upstream = link(version, worker.origin.consume(), &relay).await;
	let (handle, claim, answer) = match mounted {
		true => (tenant(&relay, "tenant-b"), "shared", "shared/cam"),
		false => (relay.consume(), "tenant-a", "tenant-a/public/cam"),
	};
	let _downstream = link(version, handle, &viewer).await;
	settle().await;

	let mut routes = viewer.consume().routes();
	assert_eq!(
		kinds(&drain(&mut routes).await),
		[("start", claim.to_string())],
		"{version}"
	);

	let continues = version.to_string() == "moq-lite-07-wip";
	assert_eq!(watch(continues, &viewer, answer).await, "w0", "{version}");
	assert_eq!(worker.served(), 1, "{version}");

	let events = drain(&mut routes).await;
	assert_eq!(kinds(&events), [("start", answer.to_string())], "{version}");
	if continues {
		assert_eq!(events[0].2, worker.claim.instance(), "{version}");
	}

	// A late snapshot starts what stands.
	let mut late = viewer.consume().routes();
	assert_eq!(
		kinds(&drain(&mut late).await),
		[("start", claim.to_string()), ("start", answer.to_string())],
		"{version}: a late snapshot"
	);
}

/// A claim above the mount's target covers all of the mount, so a reader of the mounted handle
/// takes it over a claim at the mount point's parent: the handle resolves the mount point's
/// paths through the target, whichever claims are above the mount point.
///
/// `W1`'s claim at the origin's root covers the target `tenant-a/public` and is shown at it, two
/// segments above. Renamed beneath the mount point `tenant-b/x/shared` it is three deep, and
/// carried as two above would rank below `W2`'s exact claim at `tenant-b/x`, which the handle
/// never resolves a path beneath the mount through.
#[kio::test]
async fn a_claim_above_the_target_outranks_a_claim_above_the_mount_point() {
	use crate::claim_continues::{Worker, settle, watch};

	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let root = Worker::new(1, "", "root");
	let rival = Worker::new(3, "tenant-b/x", "rival");
	let relay = produce_origin(2);
	let viewer = produce_origin(4);
	let _roots = link(version, root.origin.consume(), &relay).await;
	let _rivals = link(version, rival.origin.consume(), &relay).await;
	let handle = relay
		.mount("tenant-b/x/shared", "tenant-a/public")
		.unwrap()
		.scope("", &Prefixes::all())
		.unwrap()
		.consume();
	let _downstream = link(version, handle, &viewer).await;
	settle().await;

	assert_eq!(watch(true, &viewer, "tenant-b/x/shared/cam").await, "root0");
	assert_eq!(rival.served(), 0, "the claim above the mount point was asked");
}

macro_rules! mounted_tests {
	($($name:ident: $test:ident($($arg:expr),*),)*) => {
		$(
			#[kio::test]
			async fn $name() {
				kio::clock::timeout(TEST_TIMEOUT, $test($($arg),*))
					.await
					.expect("timed out");
			}
		)*
	};
}

mounted_tests! {
	mounted_lite06: mounted("moq-lite-06"),
	mounted_lite07: mounted("moq-lite-07-wip"),
	rooted_inside_lite06: rooted_inside("moq-lite-06"),
	rooted_inside_lite07: rooted_inside("moq-lite-07-wip"),
	relayed_lite06: relayed("moq-lite-06"),
	relayed_lite07: relayed("moq-lite-07-wip"),
	claimed_unmounted_lite06: claimed("moq-lite-06", false),
	claimed_unmounted_lite07: claimed("moq-lite-07-wip", false),
	claimed_beneath_the_mount_lite06: claimed("moq-lite-06", true),
	claimed_beneath_the_mount_lite07: claimed("moq-lite-07-wip", true),
}
