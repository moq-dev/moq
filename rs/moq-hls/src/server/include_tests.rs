use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use hang::catalog::{AudioCodec, AudioConfig, Include, Join, VideoCodec, VideoConfig};
use moq_mux::catalog::hang::Catalog;
use moq_net::{Instance, Timestamp, track};
use tower::ServiceExt;

use super::Server;

struct Published {
	route: moq_net::origin::Route,
	catalog: moq_json::snapshot::Producer<Catalog>,
	video: track::Producer,
	audio: track::Producer,
}

fn catalog() -> Catalog {
	let mut catalog = Catalog::default();
	catalog.media.join = Join::new(Duration::from_secs(2));
	let mut video = VideoConfig::new(VideoCodec::VP8);
	video.coded_width = Some(320);
	video.coded_height = Some(240);
	video.bitrate = Some(500_000);
	catalog.video.renditions.insert("same".into(), video);
	let mut audio = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
	audio.track = Some("audio".into());
	catalog.audio.renditions.insert("sound".into(), audio);
	catalog
}

fn publish(origin: &moq_net::origin::Producer, path: &str, instance: &Instance, value: &Catalog) -> Published {
	let route = origin
		.route(
			path,
			moq_net::origin::RouteOptions::hidden().with_instance(instance.clone(), 0),
		)
		.unwrap();
	let publication = route.broadcast();
	let track = publication
		.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())
		.unwrap();
	let mut catalog = moq_json::snapshot::Producer::new(track, Default::default());
	catalog.update(moq_json::Timed::from(value).at(Timestamp::ZERO)).unwrap();
	let video = publication
		.create_track("same", hang::container::track_info(1))
		.unwrap();
	// Separate tracks allow the test to observe exactly which rendition is requested.
	let audio = publication
		.create_track("audio", hang::container::track_info(2))
		.unwrap();
	route.announce().unwrap();
	Published {
		route,
		catalog,
		video,
		audio,
	}
}

fn frames(track: &track::Producer, first: u64) {
	for sequence in first..first + 3 {
		// Keyed by its first frame's pts, in the track's microseconds.
		let mut group = track.create_group((sequence * 2_000_000).into()).unwrap();
		hang::container::Frame {
			timestamp: Timestamp::from_secs(sequence * 2).unwrap(),
			payload: bytes::Bytes::from_static(if track.name() == "audio" {
				&[0xf8, 0xff, 0xfe]
			} else {
				&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00]
			}),
		}
		.write_to(&mut group)
		.unwrap();
		group.close().unwrap();
	}
}

async fn request(server: &Server, uri: &str) -> (StatusCode, String) {
	let response = server
		.router()
		.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
		.await
		.unwrap();
	let status = response.status();
	let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
	(status, String::from_utf8_lossy(&body).into_owned())
}

async fn master(server: &Server, contains: &str, present: bool) -> String {
	let deadline = kio::clock::now().saturating_add(Duration::from_secs(10));
	loop {
		let (status, body) = request(server, "/room/live/master.m3u8?jwt=viewer").await;
		if status == StatusCode::OK && body.contains(contains) == present {
			return body;
		}
		assert!(
			kio::clock::now() < deadline,
			"master never reached {contains:?} = {present}: {status} {body}"
		);
		kio::clock::sleep(Duration::from_millis(10)).await;
	}
}

fn parent_catalog() -> Catalog {
	let mut catalog = catalog();
	catalog.audio.renditions.get_mut("sound").unwrap().track = Some("audio".into());
	catalog.include.insert(
		"rung/a".into(),
		Include::new(moq_net::path::Relative::new("./ladder").unwrap()),
	);
	catalog
}

#[tokio::test(start_paused = true)]
async fn an_include_does_not_hide_the_parents_first_variants() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let child = Instance::random();
	let parent = publish(&origin, "room/live", &root, &parent_catalog());
	let _include = publish(&origin, "room/ladder", &child, &catalog());
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	let broadcaster = server.broadcaster("room/live").await.unwrap();
	let at = crate::export::Include {
		id: "rung/a".into(),
		instance: child.to_string(),
	};
	assert!(
		broadcaster
			.include(&at, crate::export::Kind::Video, "same")
			.await
			.unwrap()
			.is_some()
	);
	let mut pending = Box::pin(request(&server, "/room/live/master.m3u8"));
	assert!(futures::poll!(pending.as_mut()).is_pending());
	frames(&parent.video, 0);
	frames(&parent.audio, 0);
	let (status, body) = pending.await;
	assert_eq!(status, StatusCode::OK);
	assert!(body.contains(&format!("@{root}/video/same/media.m3u8")));
	assert!(body.contains(&format!("include/rung%2Fa/video/same/@{child}/media.m3u8")));
}

#[tokio::test(start_paused = true)]
async fn an_idle_predecessor_does_not_strand_a_requested_reconfigure() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let child = Instance::random();
	let parent = publish(&origin, "room/live", &root, &parent_catalog());
	frames(&parent.video, 0);
	let mut value = catalog();
	let mut include = publish(&origin, "room/ladder", &child, &value);
	frames(&include.video, 0);
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	master(&server, "include/", true).await;
	let uri = format!("/room/live/@{root}/include/rung%2Fa/video/same/@{child}/media.m3u8");
	assert_eq!(request(&server, &uri).await.0, StatusCode::OK);
	let next = include
		.route
		.broadcast()
		.create_track("next", hang::container::track_info(1))
		.unwrap();
	let video = value.video.renditions.get_mut("same").unwrap();
	video.track = Some("next".into());
	video.coded_width = Some(640);
	include
		.catalog
		.update(moq_json::Timed::from(&value).at(Timestamp::ZERO))
		.unwrap();
	master(&server, "RESOLUTION=640x240", true).await;
	// The new response lists the predecessor's retained rows while waiting for its
	// last pending segment. Its body keeps the replacement requested past that idle window.
	let held = server
		.router()
		.oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
		.await
		.unwrap();
	assert_eq!(held.status(), StatusCode::OK);
	tokio::time::advance(Duration::from_secs(21)).await;
	let mut demand = next.demand();
	kio::clock::timeout(Duration::from_secs(1), async {
		while demand.wanted_changed().await.unwrap().is_none() {}
	})
	.await
	.expect("the requested replacement must start after the idle predecessor releases");
	assert_eq!(include.video.demand().wanted(), None);
	frames(&next, 3);
	drop(held);
}

#[tokio::test(start_paused = true)]
async fn a_finished_parent_catalog_keeps_its_includes() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let child = Instance::random();
	let parent = publish(&origin, "room/live", &root, &parent_catalog());
	frames(&parent.video, 0);
	frames(&parent.audio, 0);
	let _include = publish(&origin, "room/ladder", &child, &catalog());
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	let listed = format!("include/rung%2Fa/video/same/@{child}");
	master(&server, &listed, true).await;
	// A clean end freezes the final catalog, entries included.
	let _finished = parent.catalog.finish().unwrap();
	kio::clock::sleep(Duration::from_secs(1)).await;
	master(&server, &listed, true).await;
}

#[tokio::test(start_paused = true)]
async fn a_failed_parent_catalog_stops_its_includes() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let child = Instance::random();
	let Published {
		route: _parent,
		catalog: parent_catalog_track,
		video,
		audio,
	} = publish(&origin, "room/live", &root, &parent_catalog());
	frames(&video, 0);
	frames(&audio, 0);
	let include = publish(&origin, "room/ladder", &child, &catalog());
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	master(&server, "include/", true).await;
	let uri = format!("/room/live/@{root}/include/rung%2Fa/video/same/@{child}/media.m3u8");
	parent_catalog_track.abort(moq_net::Error::Cancel).unwrap();
	let deadline = kio::clock::now().saturating_add(Duration::from_secs(10));
	// A live include's playlist waits for media this test never writes, so bound each request.
	while !matches!(
		kio::clock::timeout(Duration::from_secs(1), request(&server, &uri)).await,
		Ok((StatusCode::NOT_FOUND, _))
	) {
		assert!(kio::clock::now() < deadline, "the include outlived its parent catalog");
		kio::clock::sleep(Duration::from_millis(10)).await;
	}
	assert!(!request(&server, "/room/live/master.m3u8").await.1.contains("include/"));
	let mut catalog = include.catalog.demand();
	kio::clock::timeout(Duration::from_secs(1), async {
		while catalog.wanted_changed().await.unwrap().is_some() {}
	})
	.await
	.expect("the include catalog is released");
}

#[tokio::test(start_paused = true)]
async fn idle_release_waits_out_the_playlist_window() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let child = Instance::random();
	let parent = publish(&origin, "room/live", &root, &parent_catalog());
	frames(&parent.video, 0);
	let include = publish(&origin, "room/ladder", &child, &catalog());
	frames(&include.video, 0);
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	master(&server, "include/", true).await;
	let uri = format!("/room/live/@{root}/include/rung%2Fa/video/same/@{child}/media.m3u8");
	assert_eq!(request(&server, &uri).await.0, StatusCode::OK);
	let settle = || kio::clock::sleep(Duration::from_millis(10));
	settle().await;
	// Three target durations (12 s) would forget rows the 16 s window still lists.
	tokio::time::advance(Duration::from_secs(13)).await;
	settle().await;
	assert!(include.video.demand().wanted().is_some());
	tokio::time::advance(Duration::from_secs(8)).await;
	settle().await;
	assert_eq!(include.video.demand().wanted(), None);
}

#[tokio::test(start_paused = true)]
async fn idle_release_keeps_an_ended_playlists_inherited_segments() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let child = Instance::random();
	let parent = publish(&origin, "room/live", &root, &parent_catalog());
	frames(&parent.video, 0);
	let mut value = catalog();
	let mut include = publish(&origin, "room/ladder", &child, &value);
	frames(&include.video, 0);
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	master(&server, "include/", true).await;
	let uri = format!("/room/live/@{root}/include/rung%2Fa/video/same/@{child}/media.m3u8");
	assert_eq!(request(&server, &uri).await.0, StatusCode::OK);
	// Reconfigure onto another track; the predecessor ends with its own track.
	let next = include
		.route
		.broadcast()
		.create_track("next", hang::container::track_info(1))
		.unwrap();
	let video = value.video.renditions.get_mut("same").unwrap();
	video.track = Some("next".into());
	video.coded_width = Some(640);
	include
		.catalog
		.update(moq_json::Timed::from(&value).at(Timestamp::ZERO))
		.unwrap();
	master(&server, "RESOLUTION=640x240", true).await;
	let _video = include.video.finish().unwrap();
	let held = server
		.router()
		.oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
		.await
		.unwrap();
	assert_eq!(held.status(), StatusCode::OK);
	frames(&next, 3);
	let _next = next.finish().unwrap();
	drop(held);
	let deadline = kio::clock::now().saturating_add(Duration::from_secs(10));
	let ended = loop {
		let (status, playlist) = request(&server, &uri).await;
		if status == StatusCode::OK && playlist.contains("#EXT-X-ENDLIST") {
			break playlist;
		}
		assert!(
			kio::clock::now() < deadline,
			"the playlist never ended: {status} {playlist}"
		);
		kio::clock::sleep(Duration::from_millis(10)).await;
	};
	assert!(ended.contains("#EXT-X-MEDIA-SEQUENCE:0"), "{ended}");
	assert!(ended.contains("#EXT-X-DISCONTINUITY"), "{ended}");
	// Past the idle window, the ended playlist still lists its predecessor's segments.
	// Let the idle task start its timer, and run once it fires, before a request renews demand.
	kio::clock::sleep(Duration::from_millis(10)).await;
	tokio::time::advance(Duration::from_secs(21)).await;
	kio::clock::sleep(Duration::from_millis(10)).await;
	let (status, after) = request(&server, &uri).await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(after, ended);
	// The finished window keeps the track pin its retained groups are read through.
	let broadcaster = server.broadcaster("room/live").await.unwrap();
	let at = crate::export::Include {
		id: "rung/a".into(),
		instance: child.to_string(),
	};
	let rendition = broadcaster
		.include(&at, crate::export::Kind::Video, "same")
		.await
		.unwrap()
		.unwrap();
	assert!(rendition.pinned());
}

#[tokio::test(start_paused = true)]
async fn an_include_rendition_whose_broadcast_failed_is_not_listed() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let parent = publish(&origin, "room/live", &root, &parent_catalog());
	frames(&parent.video, 0);
	frames(&parent.audio, 0);
	let other = origin
		.claim("room/other", moq_net::origin::RouteOptions::default())
		.unwrap();
	let child = Instance::random();
	let mut value = catalog();
	value.audio.renditions.clear();
	value.video.renditions.get_mut("same").unwrap().broadcast = Some(moq_net::path::Relative::new("./other").unwrap());
	let _include = publish(&origin, "room/ladder", &child, &value);
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	master(&server, &format!("@{root}/video/same"), true).await;
	// Not a denial, but its media still can't be read.
	kio::clock::timeout(Duration::from_secs(5), other.requested())
		.await
		.expect("the binding never asked")
		.unwrap()
		.reject(moq_net::Error::Unroutable);
	kio::clock::sleep(Duration::from_secs(1)).await;
	let listed = master(&server, &format!("@{root}/video/same"), true).await;
	assert!(!listed.contains("include/"), "{listed}");
}

#[tokio::test(start_paused = true)]
async fn an_include_answers_the_master_when_no_own_rendition_is_listable() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let child = Instance::random();
	let mut value = parent_catalog();
	value.video.renditions.get_mut("same").unwrap().enabled = false;
	value.audio.renditions.get_mut("sound").unwrap().enabled = false;
	let _parent = publish(&origin, "room/live", &root, &value);
	let _include = publish(&origin, "room/ladder", &child, &catalog());
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	let broadcaster = server.broadcaster("room/live").await.unwrap();
	let at = crate::export::Include {
		id: "rung/a".into(),
		instance: child.to_string(),
	};
	assert!(
		broadcaster
			.include(&at, crate::export::Kind::Video, "same")
			.await
			.unwrap()
			.is_some()
	);
	// Disabled own renditions can't be listed, so the master needn't wait out its timeout.
	let start = kio::clock::now();
	let (status, body) = request(&server, "/room/live/master.m3u8").await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert!(
		body.contains(&format!("include/rung%2Fa/video/same/@{child}")),
		"{body}"
	);
	assert!(kio::clock::now() - start < Duration::from_secs(1));
}

#[tokio::test(start_paused = true)]
async fn an_include_rendition_is_listed_once_its_broadcast_resolves() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let parent = publish(&origin, "room/live", &root, &parent_catalog());
	frames(&parent.video, 0);
	frames(&parent.audio, 0);
	// The include's only rendition reads a sibling whose route has not answered yet.
	let other = origin
		.claim("room/other", moq_net::origin::RouteOptions::default())
		.unwrap();
	let child = Instance::random();
	let mut value = catalog();
	value.audio.renditions.clear();
	value.video.renditions.get_mut("same").unwrap().broadcast = Some(moq_net::path::Relative::new("./other").unwrap());
	let _include = publish(&origin, "room/ladder", &child, &value);
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	let listed = master(&server, &format!("@{root}/video/same"), true).await;
	assert!(
		!listed.contains("include/"),
		"a pending binding may yet be denied: {listed}"
	);
	let request = kio::clock::timeout(Duration::from_secs(5), other.requested())
		.await
		.expect("the binding never asked")
		.unwrap();
	request.serve(moq_net::broadcast::Info::new().produce());
	master(&server, &format!("include/rung%2Fa/video/same/@{child}"), true).await;
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn listing_is_catalog_only_and_requests_own_the_lazy_media_lifetime() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let child = Instance::random();
	let parent = publish(&origin, "room/live", &root, &parent_catalog());
	frames(&parent.video, 0);
	frames(&parent.audio, 0);
	let mut child_catalog = catalog();
	child_catalog.audio.renditions.get_mut("sound").unwrap().track = Some("audio".into());
	let include = publish(&origin, "room/ladder", &child, &child_catalog);
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	let prefix = format!("@{root}/include/rung%2Fa");
	let listed = master(&server, &prefix, true).await;
	assert!(listed.contains(&format!("@{root}/video/same/media.m3u8?jwt=viewer")));
	assert!(listed.contains(&format!("{prefix}/video/same/@{child}/media.m3u8?jwt=viewer")));
	assert!(listed.contains("NAME=\"rung/a/sound\""));
	assert_eq!(include.video.demand().wanted(), None);
	assert_eq!(include.audio.demand().wanted(), None);
	assert!(include.catalog.demand().wanted().is_some());

	let uri = format!("/room/live/{prefix}/video/same/@{child}/media.m3u8");
	let mut pending = Box::pin(request(&server, &uri));
	assert!(futures::poll!(pending.as_mut()).is_pending());
	let mut demand = include.video.demand();
	while demand.wanted_changed().await.unwrap().is_none() {}
	assert_eq!(include.audio.demand().wanted(), None);
	// The idle window is 20 s (the 16 s playlist window plus TARGETDURATION 4), and this first
	// encode is slower than all of it.
	tokio::time::advance(Duration::from_secs(21)).await;
	assert!(futures::poll!(pending.as_mut()).is_pending());
	assert!(include.video.demand().wanted().is_some());
	frames(&include.video, 0);
	let (status, playlist) = pending.await;
	assert_eq!(status, StatusCode::OK, "{playlist}");
	assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:0"));
	assert!(playlist.contains("#EXT-X-TARGETDURATION:4"));
	assert!(!playlist.contains("#EXT-X-ENDLIST"));
	let held = server
		.router()
		.oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
		.await
		.unwrap();
	assert_eq!(held.status(), StatusCode::OK);
	tokio::time::advance(Duration::from_secs(21)).await;
	assert!(
		include.video.demand().wanted().is_some(),
		"a slow body still holds demand"
	);
	drop(held);
	// The reload window starts at completion, and another request renews it.
	tokio::time::advance(Duration::from_secs(19)).await;
	assert!(include.video.demand().wanted().is_some());
	assert_eq!(request(&server, &uri).await.0, StatusCode::OK);
	tokio::time::advance(Duration::from_secs(19)).await;
	assert!(include.video.demand().wanted().is_some());
	tokio::time::advance(Duration::from_secs(2)).await;
	while demand.wanted_changed().await.unwrap().is_some() {}
	assert_eq!(include.audio.demand().wanted(), None);
	// A later request creates fresh demand and indexes later segments on the same grid.
	let mut resumed = Box::pin(request(&server, &uri));
	assert!(futures::poll!(resumed.as_mut()).is_pending());
	while demand.wanted_changed().await.unwrap().is_none() {}
	frames(&include.video, 3);
	let (status, playlist) = resumed.await;
	assert_eq!(status, StatusCode::OK);
	assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:3"), "{playlist}");
	assert_eq!(include.audio.demand().wanted(), None);
	// Neither the DASH manifest nor the recorder cursor expands the include.
	assert!(!request(&server, "/room/live/manifest.mpd").await.1.contains("include"));
	let broadcaster = server.broadcaster("room/live").await.unwrap();
	let mut recorder = broadcaster.renditions();
	for _ in 0..2 {
		assert!(recorder.next().await.unwrap().is_some());
	}
	assert!(futures::poll!(Box::pin(recorder.next()).as_mut()).is_pending());
	drop((broadcaster, server));
	while demand.wanted_changed().await.unwrap().is_some() {}
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn late_instances_replace_unpinned_entries_and_do_not_replace_pins() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let first = Instance::random();
	let second = Instance::random();
	let mut value = parent_catalog();
	let mut pin = value.include["rung/a"].clone();
	pin.instance = Some(first.clone());
	value.include.insert("pinned".into(), pin);
	let parent = publish(&origin, "room/live", &root, &value);
	frames(&parent.video, 0);
	frames(&parent.audio, 0);
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	master(&server, "include/", false).await;
	let old = publish(&origin, "room/ladder", &first, &catalog());
	let old_uri = format!("include/rung%2Fa/video/same/@{first}");
	let listed = master(&server, &old_uri, true).await;
	assert!(listed.contains(&format!("include/pinned/video/same/@{first}")));
	assert_eq!(old.video.demand().wanted(), None);
	old.route.withdraw();
	master(&server, "include/", false).await;
	old.route.announce().unwrap();
	master(&server, &format!("include/pinned/video/same/@{first}"), true).await;
	let new = publish(&origin, "room/ladder", &second, &catalog());
	let listed = master(&server, &format!("include/rung%2Fa/video/same/@{second}"), true).await;
	assert!(!listed.contains("include/pinned/"));
	assert!(!listed.contains(&old_uri));
	assert_eq!(new.video.demand().wanted(), None);
	assert_eq!(
		request(&server, &format!("/room/live/@{root}/{old_uri}/media.m3u8"))
			.await
			.0,
		StatusCode::NOT_FOUND
	);
	new.route.withdraw();
	master(&server, &format!("include/pinned/video/same/@{first}"), true).await;
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn includes_take_the_roots_grid_and_denied_ones_are_not_advertised() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let parent = publish(&origin, "room/live", &root, &parent_catalog());
	frames(&parent.video, 0);
	frames(&parent.audio, 0);
	let child = Instance::random();
	// The include declares its own interval, which HLS ignores for the root's.
	let mut value = catalog();
	value.media.join = Join::new(Duration::from_secs(1));
	let include = publish(&origin, "room/ladder", &child, &value);
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	master(&server, "include/", true).await;
	let patterns = ["room/live".parse::<moq_net::Pattern>().unwrap()].into_iter().collect();
	let denied = Server::new(origin.consume().scope("", &patterns).unwrap(), Default::default()).unwrap();
	master(&denied, "include/", false).await;
	for resource in ["media.m3u8", "init.abcd.mp4", "seg/abcd.0.m4s"] {
		assert_eq!(
			request(
				&denied,
				&format!("/room/live/@{root}/include/rung%2Fa/video/same/@{child}/{resource}")
			)
			.await
			.0,
			StatusCode::FORBIDDEN
		);
	}
	assert_eq!(include.video.demand().wanted(), None);
	let uri = format!("/room/live/@{root}/include/rung%2Fa/video/same/@{child}/media.m3u8");
	let mut pending = Box::pin(request(&server, &uri));
	assert!(futures::poll!(pending.as_mut()).is_pending());
	let mut demand = include.video.demand();
	kio::clock::timeout(Duration::from_secs(10), async {
		while demand.wanted_changed().await.unwrap().is_none() {}
	})
	.await
	.expect("the request never demanded the include's media");
	frames(&include.video, 0);
	let (status, playlist) = kio::clock::timeout(Duration::from_secs(10), pending)
		.await
		.expect("the include never listed a segment");
	assert_eq!(status, StatusCode::OK, "{playlist}");
	// Twice the root's 2 s interval; its own 1 s interval would give 2.
	assert!(playlist.contains("#EXT-X-TARGETDURATION:4"), "{playlist}");
}

#[tokio::test(start_paused = true)]
async fn a_refused_include_is_listed_once_announced() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let parent = publish(&origin, "room/live", &root, &parent_catalog());
	frames(&parent.video, 0);
	frames(&parent.audio, 0);
	// An on-demand route that drops its prepared answer refuses the first resolution.
	let claim = origin
		.claim("room/ladder", moq_net::origin::RouteOptions::default())
		.unwrap();
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	let refuse = async {
		let request = kio::clock::timeout(Duration::from_secs(5), claim.requested())
			.await
			.expect("never asked")
			.unwrap();
		drop(request.route(moq_net::origin::RouteOptions::hidden()).unwrap());
	};
	let own = format!("@{root}/video/same");
	let (listed, ()) = futures::join!(master(&server, &own, true), refuse);
	assert!(!listed.contains("include/"), "{listed}");
	// The refusing route is not asked again on a timer.
	assert!(
		kio::clock::timeout(Duration::from_secs(30), claim.requested())
			.await
			.is_err()
	);
	drop(claim);
	let child = Instance::random();
	let _include = publish(&origin, "room/ladder", &child, &catalog());
	master(&server, &format!("include/rung%2Fa/video/same/@{child}"), true).await;
}
