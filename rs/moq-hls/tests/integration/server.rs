//! The HTTP serve path and the export's lookups, for broadcasts that cross a session.

use axum::http::StatusCode;
use moq_hls::export::{Broadcaster, Route, renditions};

use crate::support::*;

/// An embedder parses the path, rewrites the broadcast into its own scope, and answers through
/// `respond` with the query propagated to every child URL.
#[tokio::test(start_paused = true)]
async fn respond_serves_a_route_rewritten_into_scope() {
	let pair = Pair::connect().await;
	let mut publisher = Publisher::new(&pair.publisher, "live", Default::default());
	publisher.write(0, 5, 1);

	let server = pair.server();
	let media = media_uri(&server.router(), "live").await;
	let mut route = Route::parse(&format!("/tenant{media}")).expect("media route");
	route.broadcast = route.broadcast.strip_prefix("tenant/").expect("scoped").to_string();

	let deadline = kio::clock::now() + TIMEOUT;
	let body = loop {
		let response = server.respond(&route, Some("jwt=abc")).await;
		if response.status() == StatusCode::OK {
			let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
			let body = String::from_utf8(body.to_vec()).unwrap();
			if body.contains(".0.m4s") {
				break body;
			}
		}
		assert!(kio::clock::now() < deadline, "media playlist never listed a segment");
		kio::clock::sleep(std::time::Duration::from_millis(50)).await;
	};
	assert!(body.contains(".mp4?jwt=abc\""), "{body}");
	assert!(body.contains(".0.m4s?jwt=abc"), "{body}");
}

/// A recorder's cursor gets the media of a publisher whose moq-lite has no FETCH: the cursor's
/// own subscription carries every group to the subscriber's cache.
#[tokio::test(start_paused = true)]
async fn a_cursor_records_a_publisher_without_fetch() {
	for version in [
		"moq-lite-01",
		"moq-lite-02",
		"moq-lite-03",
		"moq-lite-04",
		"moq-lite-05",
	] {
		let pair = Pair::new(version.parse().unwrap(), Default::default()).await;
		let mut publisher = Publisher::new(&pair.publisher, "live", Default::default());

		let origin = pair.subscriber.consume();
		let remote = kio::clock::timeout(TIMEOUT, origin.follow("live", None).unwrap().available())
			.await
			.expect("request timed out")
			.expect("remote broadcast");
		let (broadcaster, driver) = Broadcaster::new(moq_mux::Source::new(origin, remote), Default::default());
		tokio::spawn(driver.run());
		// Recorded as soon as the catalog lists it with its records, as a recorder does.
		let mut renditions = broadcaster.renditions();
		let rendition = match kio::clock::timeout(TIMEOUT, renditions.next()).await {
			Ok(Ok(Some(renditions::Event::Added(rendition)))) => rendition,
			_ => panic!("{version}: the catalog never listed the rendition"),
		};
		let mut cursor = rendition.segments();
		let mut next = std::pin::pin!(cursor.next());
		// Waiting for its first segment, the cursor subscribes before any media exists.
		let mut demand = publisher.media.demand();
		let wanted = async {
			while demand.wanted_changed().await?.is_none() {}
			moq_net::Result::Ok(())
		};
		kio::clock::timeout(TIMEOUT, async {
			tokio::select! {
				_ = next.as_mut() => panic!("{version}: a segment before any media"),
				wanted = wanted => wanted.expect("track open"),
			}
		})
		.await
		.unwrap_or_else(|_| panic!("{version}: the cursor never subscribed"));

		publisher.write(0, 5, 1);
		let segment = kio::clock::timeout(TIMEOUT, next)
			.await
			.unwrap_or_else(|_| panic!("{version}: no segment recorded"))
			.unwrap_or_else(|err| panic!("{version}: {err}"))
			.expect("a segment");
		assert_eq!(segment.segment, 0, "{version}");
		assert!(!segment.media.expect("media, not a gap").is_empty(), "{version}");
	}
}

/// A miss that crossed a moq-lite session answers 404, not 500.
#[tokio::test(start_paused = true)]
async fn a_session_crossed_cache_miss_answers_404() {
	let mut config = moq_net::origin::Config::default();
	config.pool = moq_net::cache::Pool::new(moq_net::cache::Config::default().with_capacity(1));
	let version = *moq_net::Versions::default().iter().next().unwrap();
	let pair = Pair::new(version, config).await;
	let mut publisher = Publisher::new(&pair.publisher, "live", Default::default());
	publisher.write(0, 5, 1);

	let app = pair.server().router();
	let media = media_uri(&app, "live").await;
	let first = segment(&listed(&app, &media, ".0.m4s").await, 0);
	assert_missing(&get(&app, &media.replace("media.m3u8", &first)).await);
}

#[tokio::test(start_paused = true)]
async fn a_disconnected_publisher_on_the_catalog_broadcast_answers_404() {
	let pair = Pair::connect().await;
	let mut publisher = Publisher::new(&pair.publisher, "live", Default::default());
	publisher.write(0, 5, 1);

	let app = pair.server().router();
	let media = media_uri(&app, "live").await;
	let first = segment(&listed(&app, &media, ".0.m4s").await, 0);

	let remote = kio::clock::timeout(TIMEOUT, pair.subscriber.consume().request("live", None))
		.await
		.expect("remote resolve timed out")
		.expect("remote broadcast");
	drop(publisher);
	kio::clock::timeout(TIMEOUT, remote.closed())
		.await
		.expect("publisher close timed out");

	assert_missing(&get(&app, &media.replace("media.m3u8", &first)).await);
}

/// The server watches its whole scope, so a lookup rides one request to the peer rather than
/// opening its own for every path an HTTP client names.
#[tokio::test(start_paused = true)]
async fn lookups_ride_the_servers_scope_watch() {
	let pair = Pair::connect().await;
	let other = pair.publisher.create_broadcast("room/other").expect("other");
	other.announce(Default::default()).expect("announce other");

	let server = pair.server();
	let missing = kio::clock::timeout(TIMEOUT, server.broadcaster("room/missing"))
		.await
		.expect("lookup timed out");
	assert!(missing.is_none());

	// Nothing watches `room/other` on its own, yet the synced lookup already discovered it.
	pair.subscriber
		.consume()
		.request("room/other", None)
		.await
		.expect("discovered through the scope watch");
}

/// A client asks its peer only about watched paths, so a sibling no listing covers is asked about
/// before it is requested, on every lite version, including those whose announce stream marks no
/// initial set.
#[tokio::test(start_paused = true)]
async fn a_sibling_resolves_through_a_client_session() {
	for version in [
		"moq-lite-01",
		"moq-lite-02",
		"moq-lite-03",
		"moq-lite-04",
		"moq-lite-05",
		"moq-lite-06",
		"moq-lite-07-wip",
	] {
		let pair = Pair::new(version.parse().unwrap(), Default::default()).await;
		let catalog = pair.publisher.create_broadcast("room/live").expect("catalog");
		catalog.announce(Default::default()).expect("announce catalog");
		let media = pair.publisher.create_broadcast("room/source").expect("media");
		media.announce(Default::default()).expect("announce media");

		let origin = pair.subscriber.consume();
		let broadcast = kio::clock::timeout(TIMEOUT, origin.follow("room/live", None).unwrap().available())
			.await
			.expect("catalog resolve timed out")
			.expect("catalog broadcast");
		let source = moq_mux::Source::new(origin, broadcast);
		let sibling = kio::clock::timeout(
			TIMEOUT,
			source.resolve(Some(&moq_net::path::Relative::new("./source").unwrap()), None),
		)
		.await
		.expect("sibling resolve timed out")
		.unwrap_or_else(|err| panic!("{version}: sibling broadcast: {err}"));
		assert!(!sibling.is_closed(), "{version}");
	}
}

#[tokio::test(start_paused = true)]
async fn a_disconnected_sibling_publisher_answers_404() {
	let pair = Pair::connect().await;
	let catalog_broadcast = pair.publisher.create_broadcast("room/live").expect("catalog");
	catalog_broadcast
		.announce(Default::default())
		.expect("announce catalog");
	let media_broadcast = pair.publisher.create_broadcast("room/source").expect("media");
	media_broadcast.announce(Default::default()).expect("announce media");

	let catalog =
		moq_mux::catalog::Producer::new(&mut catalog_broadcast.broadcast().clone(), catalog_config()).unwrap();
	let reserved = catalog.reserve();
	let mut config = video_config();
	config.broadcast = Some(moq_net::path::Relative::new("./source").unwrap());
	let track = media_broadcast.create_track(TRACK, timed()).unwrap();
	let mut media = reserved
		.video(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
			config,
		)
		.unwrap();
	drop(reserved);
	for second in 0..5 {
		media.write(frame(second, 1)).unwrap();
	}

	let app = pair.server().router();
	let uri = media_uri(&app, "room/live").await;
	let first = segment(&listed(&app, &uri, ".0.m4s").await, 0);

	// Waited on rather than requested once: a client only asks about what it watches.
	let remote_media = kio::clock::timeout(
		TIMEOUT,
		pair.subscriber
			.consume()
			.follow("room/source", None)
			.unwrap()
			.available(),
	)
	.await
	.expect("sibling resolve timed out")
	.expect("sibling broadcast");
	// Only the sibling goes: the catalog keeps listing the rendition, so the request reaches the
	// media fetch rather than missing the rendition.
	media_broadcast.close();
	kio::clock::timeout(TIMEOUT, remote_media.closed())
		.await
		.expect("sibling close timed out");

	assert_missing(&get(&app, &uri.replace("media.m3u8", &first)).await);
	drop((catalog, catalog_broadcast, media));
}
