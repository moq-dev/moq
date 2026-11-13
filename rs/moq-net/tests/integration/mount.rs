#![cfg(feature = "async")]

//! A session answered through a mounted handle presents the target's broadcasts under
//! the mount point on every version, hides what the origin holds at the mount point
//! itself, and a reader there reaches the target.

use crate::support;
use support::AtStart;
use support::AppendNext;

use std::time::Duration;

use moq_net::{Pattern, Patterns, Version, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TEST_TIMEOUT: Duration = Duration::from_secs(600);

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer.with_proxy(true)
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
	group.close().unwrap();
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
		.scope(root, &Patterns::from(Pattern::all()))
		.unwrap()
		.with_proxy(true)
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
			} => events.push((true, announce.prefix.to_string())),
			origin::RouteEvent {
				kind: origin::RouteKind::End,
				announce,
			} => events.push((false, announce.prefix.to_string())),
			origin::RouteEvent {
				kind: origin::RouteKind::Offline,
				announce,
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

/// What a reader on `version` sees once `paths` leave without a withdrawal it can
/// certify: offline where the route layer carries it, ended where only an ANNOUNCE
/// says it went.
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

	// What leaves the target leaves the mount. The route layer carries the mount's
	// withdrawal without a floor certifying it, so it goes offline there.
	drop(published.remove(0));
	assert_eq!(drain(&mut routes).await, lost(version, &["shared/cam"]), "{version}");
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

macro_rules! mounted_tests {
	($($name:ident: $test:ident, $version:literal,)*) => {
		$(
			#[kio::test]
			async fn $name() {
				kio::clock::timeout(TEST_TIMEOUT, $test($version))
					.await
					.expect("timed out");
			}
		)*
	};
}

mounted_tests! {
	mounted_lite06: mounted, "moq-lite-06",
	mounted_lite07: mounted, "moq-lite-07-wip",
	rooted_inside_lite06: rooted_inside, "moq-lite-06",
	rooted_inside_lite07: rooted_inside, "moq-lite-07-wip",
	relayed_lite06: relayed, "moq-lite-06",
	relayed_lite07: relayed, "moq-lite-07-wip",
}
