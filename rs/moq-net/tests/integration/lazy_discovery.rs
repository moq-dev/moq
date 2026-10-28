#![cfg(feature = "async")]

//! A client session asks its peer to announce only the prefixes its origin's readers
//! watch, keeps a request open while anything at or beneath it is watched, and lets
//! it go once nothing has been for a linger. A relay's link still asks for everything.

use std::time::Duration;

use moq_net::{Error, Hop, Pattern, Patterns, Version, announce, origin};

use crate::support;
use support::harness::{MockConnectOptions, MockPair, connect_mock};

// Versions that mark where the initial set ends, so a synced read holds all of it.
const VERSIONS: &[&str] = &["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"];

/// Long enough, in virtual time, for anything in flight to reach the far side.
const SETTLE: Duration = Duration::from_millis(500);

/// Past the 2s an announce request outlives its last watcher.
const LINGERED: Duration = Duration::from_secs(3);

/// The first byte of a moq-lite ANNOUNCE stream.
const ANNOUNCE: u8 = 1;

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// A server publishing `a/x` and `b/y`, and a client fed into `subscriber` asking
/// only about what its readers watch.
struct Lazy {
	subscriber: origin::Producer,
	pair: MockPair,
	_broadcasts: Vec<moq_net::origin::Publication>,
}

impl Lazy {
	async fn new(version: &str) -> Self {
		Self::with(version, produce_origin(2)).await
	}

	async fn with(version: &str, subscriber: origin::Producer) -> Self {
		let publisher = produce_origin(1);
		let broadcasts = ["a/x", "b/y"]
			.into_iter()
			.map(|path| {
				let broadcast = publisher.create_broadcast(path).unwrap();
				broadcast.announce(Default::default()).unwrap();
				broadcast
			})
			.collect();

		let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(subscriber.clone());
		options.client_discovers_all = false;
		let pair = connect_mock(options).await;
		moq_clock::sleep(SETTLE).await;
		Self {
			subscriber,
			pair,
			_broadcasts: broadcasts,
		}
	}

	/// How many announce requests the client has opened.
	fn requests(&self) -> usize {
		self.pair
			.client_transport
			.bidi_types()
			.into_iter()
			.filter(|kind| *kind == ANNOUNCE)
			.count()
	}

	/// Watch everything under `prefix`.
	fn watch(&self, prefix: &str) -> announce::Consumer {
		self.subscriber
			.consume()
			.scope(prefix, &Patterns::from(Pattern::all()))
			.unwrap()
			.announced()
	}

	/// Inspect the current table without running discovery for the observation itself.
	async fn routes(&self, path: &str) -> bool {
		let consumer = self
			.subscriber
			.consume()
			.scope("", &Patterns::from(Pattern::literal(path).unwrap()))
			.unwrap();
		let mut announced = consumer.announced();
		matches!(
			announced.poll_next(&kio::Waiter::noop()),
			std::task::Poll::Ready(Some(announce::Event::Start(_)))
		)
	}
}

/// Read until the stream syncs, returning the prefixes it then holds.
async fn synced(announced: &mut announce::Consumer) -> Vec<String> {
	let mut seen = Vec::new();
	while !announced.poll_synced(&moq_net::kio::Waiter::noop()).is_ready() {
		let event = moq_clock::timeout(SETTLE, announced.next())
			.await
			.expect("synced in time")
			.expect("the stream stays open");
		match event {
			announce::Event::Start(announce) | announce::Event::Restart(announce) => {
				seen.push(announce.prefix.to_string())
			}
			announce::Event::End(announce) => seen.retain(|prefix| *prefix != announce.prefix.as_str()),
			announce::Event::Update(_) => {}
		}
	}
	seen.sort();
	seen
}

/// Nothing watched, nothing asked: the peer's broadcasts stay undiscovered.
#[moq_clock::test]
async fn asks_nothing_until_watched() {
	for version in VERSIONS {
		let lazy = Lazy::new(version).await;
		assert_eq!(lazy.requests(), 0, "{version}");
		assert!(!lazy.routes("a/x").await, "{version}");
	}
}

/// A watch asks about its own prefix and nothing else.
#[moq_clock::test]
async fn asks_only_about_the_watched_prefix() {
	for version in VERSIONS {
		let lazy = Lazy::new(version).await;
		let mut watch = lazy.watch("a");
		assert_eq!(synced(&mut watch).await, ["x"], "{version}");
		assert_eq!(lazy.requests(), 1, "{version}");
		assert!(lazy.routes("a/x").await, "{version}");
		assert!(!lazy.routes("b/y").await, "{version}");
	}
}

/// A watch made before the session exists is asked about once it connects.
#[moq_clock::test]
async fn a_new_session_asks_about_existing_watches() {
	let subscriber = produce_origin(2);
	let mut watch = subscriber
		.consume()
		.scope("b", &Patterns::from(Pattern::all()))
		.unwrap()
		.announced();
	let lazy = Lazy::with("moq-lite-07-wip", subscriber).await;
	assert_eq!(synced(&mut watch).await, ["y"]);
	assert_eq!(lazy.requests(), 1);
	assert!(!lazy.routes("a/x").await);
}

/// The request outlives its last watcher by the linger, then closes and retracts.
#[moq_clock::test]
async fn lingers_after_the_last_watcher() {
	for version in VERSIONS {
		let lazy = Lazy::new(version).await;
		let mut watch = lazy.watch("a");
		synced(&mut watch).await;
		drop(watch);

		moq_clock::sleep(Duration::from_secs(1)).await;
		assert!(lazy.routes("a/x").await, "{version}: still lingering");

		// Watched again within the linger: the same request carries on.
		let again = lazy.watch("a");
		moq_clock::sleep(LINGERED).await;
		assert!(lazy.routes("a/x").await, "{version}: rewatched");
		assert_eq!(lazy.requests(), 1, "{version}");

		drop(again);
		moq_clock::sleep(LINGERED).await;
		assert!(!lazy.routes("a/x").await, "{version}: closed");
	}
}

/// A narrower watch rides an open broader request, which stays open while it does.
#[moq_clock::test]
async fn a_narrower_watch_rides_the_broader_request() {
	for version in VERSIONS {
		let lazy = Lazy::new(version).await;
		let mut root = lazy.watch("");
		assert_eq!(synced(&mut root).await, ["a/x", "b/y"], "{version}");
		let mut narrower = lazy.watch("a");
		assert_eq!(synced(&mut narrower).await, ["x"], "{version}");
		assert_eq!(lazy.requests(), 1, "{version}");

		drop(root);
		moq_clock::sleep(LINGERED).await;
		assert_eq!(lazy.requests(), 1, "{version}: nothing reopened");
		assert!(lazy.routes("a/x").await, "{version}");
		assert!(narrower.try_next().is_none(), "{version}: nothing retracted");
	}
}

/// A narrower request open before a broader one stays until the broader one goes.
#[moq_clock::test]
async fn a_broader_request_holds_the_narrower_open() {
	for version in VERSIONS {
		let lazy = Lazy::new(version).await;
		let mut narrower = lazy.watch("a");
		synced(&mut narrower).await;
		let mut root = lazy.watch("");
		assert_eq!(synced(&mut root).await, ["a/x", "b/y"], "{version}");
		assert_eq!(lazy.requests(), 2, "{version}");

		drop(narrower);
		moq_clock::sleep(LINGERED).await;
		assert!(root.try_next().is_none(), "{version}: nothing retracted");

		// The broader request lingers out, then the narrower one it held does.
		drop(root);
		moq_clock::sleep(LINGERED).await;
		assert!(!lazy.routes("b/y").await, "{version}");
		assert!(lazy.routes("a/x").await, "{version}");
		moq_clock::sleep(LINGERED).await;
		assert!(!lazy.routes("a/x").await, "{version}");
	}
}

/// A narrower request still lingering when a broader one opens is held like one still
/// watched, rather than lingering out on its own timer.
#[moq_clock::test]
async fn a_broader_request_holds_a_lingering_narrower_open() {
	for version in VERSIONS {
		let lazy = Lazy::new(version).await;
		let mut narrower = lazy.watch("a");
		synced(&mut narrower).await;
		drop(narrower);

		moq_clock::sleep(Duration::from_secs(1)).await;
		let mut root = lazy.watch("");
		assert_eq!(synced(&mut root).await, ["a/x", "b/y"], "{version}");
		assert_eq!(lazy.requests(), 2, "{version}");

		// Past the narrower request's own linger: it is held, not closed.
		moq_clock::sleep(LINGERED).await;
		assert!(root.try_next().is_none(), "{version}: nothing retracted");

		// The broader request lingers out, then the narrower one it held does.
		drop(root);
		moq_clock::sleep(LINGERED).await;
		assert!(!lazy.routes("b/y").await, "{version}");
		assert!(lazy.routes("a/x").await, "{version}");
		moq_clock::sleep(LINGERED).await;
		assert!(!lazy.routes("a/x").await, "{version}");
	}
}

/// A broadcast resolved through a discovered route keeps it discovered after the
/// listing that found it closes.
#[moq_clock::test]
async fn a_resolved_broadcast_keeps_its_route() {
	for version in VERSIONS {
		let lazy = Lazy::new(version).await;
		let mut watch = lazy.watch("a");
		synced(&mut watch).await;
		let broadcast = lazy.subscriber.consume().request_broadcast("a/x", None).await.unwrap();
		drop(watch);

		moq_clock::sleep(LINGERED).await;
		assert!(lazy.routes("a/x").await, "{version}");
		assert!(!lazy.routes("b/y").await, "{version}");

		drop(broadcast);
	}
}

/// Waiting on a path asks about it.
#[moq_clock::test]
async fn waiting_on_a_path_asks_about_it() {
	for version in VERSIONS {
		let lazy = Lazy::new(version).await;
		let consumer = lazy.subscriber.consume();
		moq_clock::timeout(SETTLE, consumer.routed_broadcast("a/x"))
			.await
			.expect("routed in time")
			.unwrap();
		let mut followed = consumer.announced_broadcast("b/y", None).unwrap();
		moq_clock::timeout(SETTLE, followed.available())
			.await
			.expect("announced in time")
			.unwrap();
		assert_eq!(lazy.requests(), 2, "{version}");
	}
}

/// A relay's link asks for everything, watched or not, so discovery never narrows
/// what a relay learns.
#[moq_clock::test]
async fn a_relay_link_asks_for_everything() {
	for version in VERSIONS {
		for upstream in [false, true] {
			let relay = produce_origin(2);
			let link = match upstream {
				true => relay.clone().with_link(moq_net::origin::Link::Upstream),
				false => relay.clone().with_link(moq_net::origin::Link::Peer),
			};
			let lazy = Lazy::with(version, link).await;
			assert_eq!(lazy.requests(), 1, "{version}");
			// Read through a plain handle: one taken from an upstream link hides its routes.
			for path in ["a/x", "b/y"] {
				relay.consume().request_broadcast(path, None).await.unwrap();
			}
		}
	}
}

/// A server asks a client for everything it may, so a relay ingests what clients publish.
#[moq_clock::test]
async fn a_server_asks_for_everything() {
	for version in VERSIONS {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("a/x").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let relay = produce_origin(2);
		let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
		options.client_publish = Some(publisher.consume());
		options.server_subscribe = Some(relay.clone());
		let _pair = connect_mock(options).await;
		moq_clock::sleep(SETTLE).await;
		relay.consume().request_broadcast("a/x", None).await.unwrap();
	}
}

/// Serving the peer's own request is no local reader: a client publishing and
/// consuming through one origin still asks about only what its readers watch.
#[moq_clock::test]
async fn serving_the_peer_asks_nothing() {
	for version in VERSIONS {
		let local = produce_origin(2);
		let remote = produce_origin(1);
		let broadcast = remote.create_broadcast("a/x").unwrap();
		broadcast.announce(Default::default()).unwrap();

		let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
		options.client_publish = Some(local.consume());
		options.client_subscribe = Some(local.clone());
		options.client_discovers_all = false;
		options.server_publish = Some(remote.consume());
		options.server_subscribe = Some(remote.clone());
		let pair = connect_mock(options).await;
		moq_clock::sleep(SETTLE).await;

		let requests = pair
			.client_transport
			.bidi_types()
			.into_iter()
			.filter(|kind| *kind == ANNOUNCE)
			.count();
		assert_eq!(requests, 0, "{version}");
		// An application request is a reader, unlike the peer's serving view.
		let _read = local.consume().request_broadcast("a/x", None).await.unwrap();
		let requests = pair
			.client_transport
			.bidi_types()
			.into_iter()
			.filter(|kind| *kind == ANNOUNCE)
			.count();
		assert_eq!(requests, 1, "{version}: only the application request starts discovery");
	}
}

/// Resolving a path owns the discovery that makes the peer's announcement visible.
#[moq_clock::test]
async fn requesting_an_unwatched_path_discovers_its_instance() {
	for version in VERSIONS {
		let lazy = Lazy::new(version).await;
		assert_eq!(lazy.requests(), 0, "{version}");
		let broadcast = lazy
			.subscriber
			.consume()
			.request_broadcast("a/x", None)
			.await
			.expect("request discovers the announced instance without a separate watcher");
		assert_eq!(broadcast.info().path.as_str(), "a/x");
		assert_eq!(lazy.requests(), 1, "{version}");
	}
}

/// Absence is answered by the peer's sync boundary, without a timer in resolution.
#[moq_clock::test]
async fn an_unwatched_missing_path_is_unroutable_after_sync() {
	for version in VERSIONS {
		let lazy = Lazy::new(version).await;
		let result = moq_clock::timeout(SETTLE, lazy.subscriber.consume().request_broadcast("missing", None))
			.await
			.expect("the peer synced");
		assert!(matches!(result, Err(Error::Unroutable)), "{version}");
		assert_eq!(lazy.requests(), 1, "{version}");
		let broadcast = lazy.subscriber.consume().request_broadcast("a/x", None).await.unwrap();
		moq_clock::sleep(LINGERED).await;
		assert!(
			lazy.routes("a/x").await,
			"{version}: request ownership keeps discovery alive"
		);
		drop(broadcast);
		moq_clock::sleep(LINGERED).await;
		assert!(
			!lazy.routes("a/x").await,
			"{version}: dropping the last reader releases discovery"
		);
	}
}

/// Every moq-transport draft. These read what arrived once everything in flight has.
const IETF: &[&str] = &["moq-transport-18", "moq-transport-22"];

/// Apply what `announced` reports once everything in flight has arrived to `held`, and
/// say whether any of it ended a prefix.
async fn settled(announced: &mut announce::Consumer, held: &mut Vec<String>) -> bool {
	moq_clock::sleep(SETTLE).await;
	let mut ended = false;
	while let Some(event) = announced.try_next() {
		match event {
			announce::Event::Start(announce) | announce::Event::Restart(announce) => {
				held.push(announce.prefix.to_string())
			}
			announce::Event::End(announce) => {
				ended = true;
				held.retain(|prefix| *prefix != announce.prefix.as_str())
			}
			announce::Event::Update(_) => {}
		}
	}
	held.sort();
	held.dedup();
	ended
}

/// Nothing watched, nothing asked: the peer's broadcasts stay undiscovered.
#[moq_clock::test]
async fn ietf_asks_nothing_until_watched() {
	for version in IETF {
		let lazy = Lazy::new(version).await;
		assert!(!lazy.routes("a/x").await, "{version}");
	}
}

/// A watch asks about its own prefix and nothing else.
#[moq_clock::test]
async fn ietf_asks_only_about_the_watched_prefix() {
	for version in IETF {
		let lazy = Lazy::new(version).await;
		let mut watch = lazy.watch("a");
		let mut held = Vec::new();
		settled(&mut watch, &mut held).await;
		assert_eq!(held, ["x"], "{version}");
		assert!(lazy.routes("a/x").await, "{version}");
		assert!(!lazy.routes("b/y").await, "{version}");
	}
}

/// The request outlives its last watcher by the linger, then ends, and what it reported
/// goes with it.
#[moq_clock::test]
async fn ietf_lingers_after_the_last_watcher() {
	for version in IETF {
		let lazy = Lazy::new(version).await;
		let mut watch = lazy.watch("a");
		settled(&mut watch, &mut Vec::new()).await;
		drop(watch);

		moq_clock::sleep(Duration::from_secs(1)).await;
		assert!(lazy.routes("a/x").await, "{version}: still lingering");

		let again = lazy.watch("a");
		moq_clock::sleep(LINGERED).await;
		assert!(lazy.routes("a/x").await, "{version}: rewatched");

		drop(again);
		moq_clock::sleep(LINGERED).await;
		assert!(!lazy.routes("a/x").await, "{version}: ended");
	}
}

/// A broader watch after a narrower one discovers everything, and retracts nothing the
/// narrower one holds.
#[moq_clock::test]
async fn ietf_a_broader_watch_retracts_nothing() {
	for version in IETF {
		let lazy = Lazy::new(version).await;
		let mut narrower = lazy.watch("a");
		let mut held = Vec::new();
		settled(&mut narrower, &mut held).await;
		assert_eq!(held, ["x"], "{version}");

		let mut root = lazy.watch("");
		let mut all = Vec::new();
		settled(&mut root, &mut all).await;
		assert_eq!(all, ["a/x", "b/y"], "{version}");
		assert!(
			!settled(&mut narrower, &mut held).await,
			"{version}: retracted {held:?}"
		);

		drop(root);
		moq_clock::sleep(LINGERED).await;
		assert!(lazy.routes("a/x").await, "{version}: the narrower watch still holds it");
		assert!(
			!settled(&mut narrower, &mut held).await,
			"{version}: retracted {held:?}"
		);

		drop(narrower);
		moq_clock::sleep(LINGERED * 2).await;
		assert!(!lazy.routes("a/x").await, "{version}");
		assert!(!lazy.routes("b/y").await, "{version}");
	}
}

/// Waiting on a path asks about it.
#[moq_clock::test]
async fn ietf_waiting_on_a_path_asks_about_it() {
	for version in IETF {
		let lazy = Lazy::new(version).await;
		let consumer = lazy.subscriber.consume();
		moq_clock::timeout(SETTLE, consumer.routed_broadcast("a/x"))
			.await
			.expect("routed in time")
			.unwrap();
		assert!(!lazy.routes("b/y").await, "{version}");
	}
}

/// A relay's link still asks for everything it may.
#[moq_clock::test]
async fn ietf_a_relay_link_asks_for_everything() {
	for version in IETF {
		let relay = produce_origin(2);
		let _lazy = Lazy::with(version, relay.clone().with_link(moq_net::origin::Link::Peer)).await;
		for path in ["a/x", "b/y"] {
			relay.consume().request_broadcast(path, None).await.unwrap();
		}
	}
}
