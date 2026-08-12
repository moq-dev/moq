//! Lazy announce solicitation over the in-memory mock transport (#2708).
//!
//! A session solicits announces only once its origin has announce interest, a peer's
//! own solicitation is never reciprocated, and a `request_broadcast` on a cold table
//! waits for the solicitation it triggers instead of failing.
//!
//! Time is paused in the reciprocation test: the mock transport does no real I/O, so
//! a sleep only fires once every in-flight task has quiesced, making "nothing else
//! arrived" a deterministic observation rather than a race.

mod support;

use std::time::Duration;

use moq_net::{AsPath, Error, Origin, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

/// Maximum time any single test may run before being treated as a deadlock.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Both wire families: the lite ANNOUNCE_PLEASE stream and the IETF
/// SUBSCRIBE_NAMESPACE request, before and after their respective initial-set
/// boundaries (AnnounceOk landed in lite-05).
const VERSIONS: &[&str] = &["moq-lite-04", "moq-lite-05", "moq-transport-19"];

/// The versions that mark the end of the initial announce set (ANNOUNCE_INIT on
/// moq-lite-01/02, ANNOUNCE_OK on moq-lite-05+), which is what lets a cold
/// `request_broadcast` wait for an announcement instead of racing it.
const BOUNDED_VERSIONS: &[&str] = &["moq-lite-02", "moq-lite-05"];

fn announced() -> moq_net::broadcast::Route {
	moq_net::broadcast::Route::new().with_announce(true)
}

/// A peer's solicitation is served but never reciprocated: a session whose origin has
/// no announce interest of its own solicits nothing, no matter how interested the
/// peer is. The first local consumer then solicits, and the peer's broadcasts arrive.
#[tokio::test]
async fn peer_interest_is_not_reciprocated() {
	tokio::time::pause();
	for version in VERSIONS {
		tokio::time::timeout(TEST_TIMEOUT, async {
			let version: Version = version.parse().unwrap();

			// Client: one origin wired in both directions, the common `connect_origin`
			// shape, publishing `hello`.
			let client_origin = Origin::random().produce();
			let _hello = client_origin
				.create_broadcast("hello", announced())
				.expect("publish hello");

			// Server: publishes `secret`, and holds announce interest of its own so it
			// solicits the client immediately.
			let server_pub = Origin::random().produce();
			let _secret = server_pub
				.create_broadcast("secret", announced())
				.expect("publish secret");
			let server_sub = Origin::random().produce();
			let mut server_interest = server_sub.consume().announced();

			let mut opts = MockConnectOptions::new(version);
			opts.client_publish = Some(client_origin.clone());
			opts.client_subscribe = Some(client_origin.clone());
			opts.server_publish = Some(server_pub.clone());
			opts.server_subscribe = Some(server_sub.clone());
			let MockPair { client, server } = connect_mock(opts).await;

			// The server's interest pulls the client's `hello` across: solicitation
			// works, and the client has served the peer's announce interest.
			loop {
				let announce = server_interest.next().await.expect("server session died");
				if announce.path.as_path() == moq_net::Path::new("hello").as_path() && announce.broadcast.is_some() {
					break;
				}
			}

			// Quiesce: with paused time this sleep only fires once nothing is left
			// in flight, so a reciprocal solicitation (the bug) would have landed
			// `secret` in the client's table by now.
			tokio::time::sleep(Duration::from_secs(1)).await;

			// A fresh consumer's initial replay is synchronous: what the client's
			// table held before this consumer's own interest could solicit anything.
			let mut client_announced = client_origin.consume().announced();
			let mut initial = Vec::new();
			while let Some(announce) = client_announced.try_next() {
				if announce.broadcast.is_some() {
					initial.push(announce.path);
				}
			}
			assert!(
				initial
					.iter()
					.any(|p| p.as_path() == moq_net::Path::new("hello").as_path()),
				"version {version}: local publish missing from the initial replay"
			);
			assert!(
				initial
					.iter()
					.all(|p| p.as_path() != moq_net::Path::new("secret").as_path()),
				"version {version}: the peer's solicitation was reciprocated"
			);

			// That consumer IS interest: the client now solicits, and `secret` arrives.
			loop {
				let announce = client_announced.next().await.expect("client session died");
				if announce.path.as_path() == moq_net::Path::new("secret").as_path() && announce.broadcast.is_some() {
					break;
				}
			}

			drop(client);
			drop(server);
		})
		.await
		.expect("test timed out (likely a mock deadlock)");
	}
}

/// A `request_broadcast` for a path the peer announced resolves on a cold table: the
/// request raises interest, the session solicits, and the announcement satisfies it.
/// The data path stays live afterwards, since the solicited stream is what carries
/// the route.
///
/// Only the versions that mark the end of the initial announce set can promise this.
/// moq-lite-03/04 and moq-transport mark none, so the request may conclude before the
/// announcement arrives; asserting otherwise there would be asserting a scheduling race.
#[tokio::test]
async fn request_resolves_after_lazy_solicitation() {
	for version in BOUNDED_VERSIONS {
		tokio::time::timeout(TEST_TIMEOUT, async {
			let version: Version = version.parse().unwrap();

			// Server publishes a broadcast with a track holding one frame.
			let pub_origin = Origin::random().produce();
			let mut broadcast = pub_origin
				.create_broadcast("cam", announced())
				.expect("create broadcast");
			let mut track = broadcast.create_track("video", None).expect("create track");
			let mut group = track.append_group().expect("append group");
			group
				.write_frame(moq_net::Timestamp::ZERO, b"frame".as_ref())
				.expect("write frame");
			group.finish().expect("finish group");

			let sub_origin = Origin::random().produce();
			let mut opts = MockConnectOptions::new(version);
			opts.server_publish = Some(pub_origin.clone());
			opts.client_subscribe = Some(sub_origin.clone());
			let MockPair { client, server } = connect_mock(opts).await;

			// Nothing consumed announcements before this request; it must trigger the
			// solicitation itself rather than failing on the cold table.
			let bc = sub_origin
				.consume()
				.request_broadcast("cam")
				.await
				.expect("request resolves once the announcement arrives");

			// The route the solicited stream carries serves data.
			let mut sub = bc.track("video").unwrap().subscribe(None).await.expect("subscribe");
			let mut group = sub.recv_group().await.expect("recv_group").expect("track closed");
			let frame = group.read_frame().await.expect("read frame").expect("frame");
			assert_eq!(&frame.payload[..], b"frame", "version {version}");

			drop(client);
			drop(server);
		})
		.await
		.expect("test timed out (likely a mock deadlock)");
	}
}

/// A `request_broadcast` for a path nobody announced still fails, but only after the
/// solicited initial set arrives: `Unroutable`, not a hang.
#[tokio::test]
async fn request_settles_unroutable_across_the_wire() {
	for version in VERSIONS {
		tokio::time::timeout(TEST_TIMEOUT, async {
			let version: Version = version.parse().unwrap();

			// The server has a publish origin, but nothing at the requested path.
			let pub_origin = Origin::random().produce();
			let sub_origin = Origin::random().produce();
			let mut opts = MockConnectOptions::new(version);
			opts.server_publish = Some(pub_origin.clone());
			opts.client_subscribe = Some(sub_origin.clone());
			let MockPair { client, server } = connect_mock(opts).await;

			let result = sub_origin.consume().request_broadcast("missing").await;
			assert!(
				matches!(result, Err(Error::Unroutable)),
				"version {version}: expected Unroutable"
			);

			drop(client);
			drop(server);
		})
		.await
		.expect("test timed out (likely a mock deadlock)");
	}
}
