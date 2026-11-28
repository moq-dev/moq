#![cfg(feature = "async")]

//! A claim broader than its publisher's grant is shown only where a route of it can serve:
//! at each granted prefix beneath it, however many relays it crosses. A viewer asking beside
//! the grant is shown nothing, so it never waits on a claim a request then fails Unroutable on.

use crate::support;
use support::AppendNext;

use std::{rc::Rc, time::Duration};

use moq_net::{Error, Prefix, Prefixes, Version, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

fn produce_origin(hop: u64) -> origin::Producer {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.proxy = true;
	let (producer, driver) = origin::Producer::new(config);
	support::harness::spawn(driver);
	producer
}

fn lite07() -> Version {
	"moq-lite-07-wip".parse().unwrap()
}

fn prefixes(members: &[&str]) -> Prefixes {
	members
		.iter()
		.map(|member| Prefix::try_from(*member).unwrap())
		.collect()
}

/// Have `to` pull what `from` publishes, as `to`'s readers ask for it.
async fn link(from: &origin::Producer, to: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(lite07());
	options.server_publish = Some(from.consume());
	options.client_subscribe = Some(to.clone());
	connect_mock(options).await
}

/// Let every task settle. Time is paused, so this returns once the runtime is idle.
async fn settle() {
	kio::clock::sleep(Duration::from_millis(500)).await;
}

/// Claim `prefix` on `origin` through a handle granted `granted`, answering each request with
/// a `video` track whose one frame names the path.
fn claim(origin: &origin::Producer, granted: &[&str], prefix: &str) -> Rc<origin::Claim> {
	let scoped = origin.scope("", &prefixes(granted)).unwrap();
	let claim = Rc::new(scoped.claim(prefix, origin::RouteOptions::default()).unwrap());
	let handler = claim.clone();
	drop(kio::clock::sim::spawn(async move {
		let mut routes = Vec::new();
		while let Some(request) = handler.requested().await {
			let path = request.path().to_string();
			let route = request.route(origin::RouteOptions::hidden()).unwrap();
			let track = route
				.broadcast()
				.create_track(
					"video",
					track::Info::default().with_timescale(moq_net::Timescale::MILLI),
				)
				.unwrap();
			let group = track.append_next(path.into_bytes()).unwrap();
			group.finish().unwrap();
			route.announce().unwrap();
			routes.push((route, track));
		}
	}));
	claim
}

/// Every prefix announced on `origin` beneath `under`, once it settles.
async fn announced(origin: &origin::Producer, under: &str) -> Vec<String> {
	let mut routes = origin
		.consume()
		.scope(under, &Prefixes::all())
		.unwrap()
		.covering()
		.routes();
	let mut seen = Vec::new();
	while let Ok(event) = kio::clock::timeout(Duration::from_secs(1), routes.next()).await {
		let event = event.expect("routes ended");
		if event.kind == origin::RouteKind::Start {
			seen.push(event.announce.prefix.to_string());
		}
	}
	seen
}

/// The first frame of `video` at `path` through `origin`.
async fn first(origin: &origin::Producer, path: &str) -> Result<String, Error> {
	let broadcast = origin.consume().request(path, None).await?;
	let mut subscription = broadcast.track("video")?.subscribe(None).await?;
	let mut group = kio::clock::timeout(Duration::from_secs(10), subscription.recv_group())
		.await
		.expect("subscription hung")?
		.expect("the track ended before a group");
	let frame = group.read_frame().await?.expect("a frame");
	Ok(String::from_utf8(frame.payload.to_vec()).unwrap())
}

/// `S -> R1 -> R2`: a claim at `a` granted only `a/b` is filed at `a/b` at the source and
/// shown there one and two hops on, never at `a`. A request beside the grant fails at once at
/// every hop, and one within it reaches the claim.
#[kio::test]
async fn a_claim_is_shown_no_broader_than_its_grant_through_relays() {
	let source = produce_origin(10);
	let _claim = claim(&source, &["a/b"], "a");
	let one = produce_origin(1);
	let two = produce_origin(2);
	let _links = [link(&source, &one).await, link(&one, &two).await];
	settle().await;

	for (hop, node) in [("source", &source), ("one", &one), ("two", &two)] {
		assert_eq!(announced(node, "a").await, ["b"], "at {hop}");
		assert!(announced(node, "a/q").await.is_empty(), "at {hop}");
		assert_eq!(announced(node, "a/b/c").await, [""], "at {hop}");
		let refused = kio::clock::timeout(Duration::from_secs(5), first(node, "a/q"))
			.await
			.expect("a request beside the grant waits");
		assert!(matches!(refused, Err(Error::Unroutable)), "at {hop}: {refused:?}");
	}
	assert_eq!(first(&two, "a/b/c").await.unwrap(), "a/b/c");
}
