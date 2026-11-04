use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use hang::catalog::{AudioCodec, AudioConfig, Contribution, Join, VideoCodec, VideoConfig};
use moq_mux::catalog::hang::Catalog;
use moq_net::{Instance, Timestamp, track};
use tower::ServiceExt;

use super::Server;

struct Published {
	publication: moq_net::origin::Publication,
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
	let publication = origin.create_broadcast(path).unwrap();
	let track = publication
		.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())
		.unwrap();
	let mut catalog = moq_json::snapshot::Producer::new(track, Default::default());
	catalog.update(moq_net::Timed::from(value).at(Timestamp::ZERO)).unwrap();
	let video = publication
		.create_track("same", hang::container::track_info(1))
		.unwrap();
	// Separate tracks allow the test to observe exactly which rendition is requested.
	let audio = publication
		.create_track("audio", hang::container::track_info(2))
		.unwrap();
	publication
		.announce(moq_net::origin::Route::default().with_instance(instance.clone(), 0))
		.unwrap();
	Published {
		publication,
		catalog,
		video,
		audio,
	}
}

fn frames(track: &track::Producer, first: u64) {
	for sequence in first..first + 3 {
		let mut group = track.create_group(sequence.into()).unwrap();
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
		group.finish().unwrap();
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
	let deadline = kio::clock::now() + Duration::from_secs(10);
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
	catalog.contributions.insert(
		"rung/a".into(),
		Contribution::new(moq_net::path::Relative::new("./ladder").unwrap()),
	);
	catalog
}

#[tokio::test(start_paused = true)]
async fn a_contribution_does_not_hide_the_parents_first_variants() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let child = Instance::random();
	let parent = publish(&origin, "room/live", &root, &parent_catalog());
	let _contribution = publish(&origin, "room/ladder", &child, &catalog());
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	let broadcaster = server.broadcaster("room/live").await.unwrap();
	let at = crate::export::Contribution {
		id: "rung/a".into(),
		instance: child.to_string(),
	};
	assert!(
		broadcaster
			.contribution(&at, crate::export::Kind::Video, "same")
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
	assert!(body.contains(&format!("contribution/rung%2Fa/video/same/@{child}/media.m3u8")));
}

#[tokio::test(start_paused = true)]
async fn an_idle_predecessor_does_not_strand_a_requested_reconfigure() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let child = Instance::random();
	let parent = publish(&origin, "room/live", &root, &parent_catalog());
	frames(&parent.video, 0);
	let mut value = catalog();
	let mut contribution = publish(&origin, "room/ladder", &child, &value);
	frames(&contribution.video, 0);
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	master(&server, "contribution/", true).await;
	let uri = format!("/room/live/@{root}/contribution/rung%2Fa/video/same/@{child}/media.m3u8");
	assert_eq!(request(&server, &uri).await.0, StatusCode::OK);
	let next = contribution
		.publication
		.create_track("next", hang::container::track_info(1))
		.unwrap();
	let video = value.video.renditions.get_mut("same").unwrap();
	video.track = Some("next".into());
	video.coded_width = Some(640);
	contribution
		.catalog
		.update(moq_net::Timed::from(&value).at(Timestamp::ZERO))
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
	tokio::time::advance(Duration::from_secs(13)).await;
	let mut demand = next.demand();
	kio::clock::timeout(Duration::from_secs(1), async {
		while demand.wanted_changed().await.unwrap().is_none() {}
	})
	.await
	.expect("the requested replacement must start after the idle predecessor releases");
	assert_eq!(contribution.video.demand().wanted(), None);
	frames(&next, 3);
	drop(held);
}

#[tokio::test(start_paused = true)]
async fn a_failed_parent_catalog_stops_its_contributions() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let child = Instance::random();
	let Published {
		publication: _parent,
		catalog: parent_catalog_track,
		video,
		audio,
	} = publish(&origin, "room/live", &root, &parent_catalog());
	frames(&video, 0);
	frames(&audio, 0);
	let contribution = publish(&origin, "room/ladder", &child, &catalog());
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	master(&server, "contribution/", true).await;
	let uri = format!("/room/live/@{root}/contribution/rung%2Fa/video/same/@{child}/media.m3u8");
	parent_catalog_track.abort(moq_net::Error::Cancel).unwrap();
	let deadline = kio::clock::now() + Duration::from_secs(10);
	// A live contribution's playlist waits for media this test never writes, so bound each request.
	while !matches!(
		kio::clock::timeout(Duration::from_secs(1), request(&server, &uri)).await,
		Ok((StatusCode::NOT_FOUND, _))
	) {
		assert!(
			kio::clock::now() < deadline,
			"the contribution outlived its parent catalog"
		);
		kio::clock::sleep(Duration::from_millis(10)).await;
	}
	assert!(
		!request(&server, "/room/live/master.m3u8")
			.await
			.1
			.contains("contribution/")
	);
	let mut catalog = contribution.catalog.demand();
	kio::clock::timeout(Duration::from_secs(1), async {
		while catalog.wanted_changed().await.unwrap().is_some() {}
	})
	.await
	.expect("the contribution catalog is released");
}

#[tokio::test(start_paused = true)]
async fn idle_release_keeps_an_ended_playlists_inherited_segments() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let child = Instance::random();
	let parent = publish(&origin, "room/live", &root, &parent_catalog());
	frames(&parent.video, 0);
	let mut value = catalog();
	let mut contribution = publish(&origin, "room/ladder", &child, &value);
	frames(&contribution.video, 0);
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	master(&server, "contribution/", true).await;
	let uri = format!("/room/live/@{root}/contribution/rung%2Fa/video/same/@{child}/media.m3u8");
	assert_eq!(request(&server, &uri).await.0, StatusCode::OK);
	// Reconfigure onto another track; the predecessor ends with its own track.
	let next = contribution
		.publication
		.create_track("next", hang::container::track_info(1))
		.unwrap();
	let video = value.video.renditions.get_mut("same").unwrap();
	video.track = Some("next".into());
	video.coded_width = Some(640);
	contribution
		.catalog
		.update(moq_net::Timed::from(&value).at(Timestamp::ZERO))
		.unwrap();
	master(&server, "RESOLUTION=640x240", true).await;
	contribution.video.finish().unwrap();
	let held = server
		.router()
		.oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
		.await
		.unwrap();
	assert_eq!(held.status(), StatusCode::OK);
	frames(&next, 3);
	next.finish().unwrap();
	drop(held);
	let deadline = kio::clock::now() + Duration::from_secs(10);
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
	tokio::time::advance(Duration::from_secs(13)).await;
	kio::clock::sleep(Duration::from_millis(10)).await;
	let (status, after) = request(&server, &uri).await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(after, ended);
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
	let contribution = publish(&origin, "room/ladder", &child, &child_catalog);
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	let prefix = format!("@{root}/contribution/rung%2Fa");
	let listed = master(&server, &prefix, true).await;
	assert!(listed.contains(&format!("@{root}/video/same/media.m3u8?jwt=viewer")));
	assert!(listed.contains(&format!("{prefix}/video/same/@{child}/media.m3u8?jwt=viewer")));
	assert!(listed.contains("NAME=\"rung/a/sound\""));
	assert_eq!(contribution.video.demand().wanted(), None);
	assert_eq!(contribution.audio.demand().wanted(), None);
	assert!(contribution.catalog.demand().wanted().is_some());

	let uri = format!("/room/live/{prefix}/video/same/@{child}/media.m3u8");
	let mut pending = Box::pin(request(&server, &uri));
	assert!(futures::poll!(pending.as_mut()).is_pending());
	let mut demand = contribution.video.demand();
	while demand.wanted_changed().await.unwrap().is_none() {}
	assert_eq!(contribution.audio.demand().wanted(), None);
	// TARGETDURATION = 4, so this first encode is slower than the entire 12 s idle window.
	tokio::time::advance(Duration::from_secs(13)).await;
	assert!(futures::poll!(pending.as_mut()).is_pending());
	assert!(contribution.video.demand().wanted().is_some());
	frames(&contribution.video, 0);
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
	tokio::time::advance(Duration::from_secs(13)).await;
	assert!(
		contribution.video.demand().wanted().is_some(),
		"a slow body still holds demand"
	);
	drop(held);
	// The reload window starts at completion, and another request renews it.
	tokio::time::advance(Duration::from_secs(11)).await;
	assert!(contribution.video.demand().wanted().is_some());
	assert_eq!(request(&server, &uri).await.0, StatusCode::OK);
	tokio::time::advance(Duration::from_secs(11)).await;
	assert!(contribution.video.demand().wanted().is_some());
	tokio::time::advance(Duration::from_secs(2)).await;
	while demand.wanted_changed().await.unwrap().is_some() {}
	assert_eq!(contribution.audio.demand().wanted(), None);
	// A later request creates fresh demand and indexes later segments on the same grid.
	let mut resumed = Box::pin(request(&server, &uri));
	assert!(futures::poll!(resumed.as_mut()).is_pending());
	while demand.wanted_changed().await.unwrap().is_none() {}
	frames(&contribution.video, 3);
	let (status, playlist) = resumed.await;
	assert_eq!(status, StatusCode::OK);
	assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:3"), "{playlist}");
	assert_eq!(contribution.audio.demand().wanted(), None);
	// Neither the DASH manifest nor the recorder cursor expands the contribution.
	assert!(
		!request(&server, "/room/live/manifest.mpd")
			.await
			.1
			.contains("contribution")
	);
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
	let mut pin = value.contributions["rung/a"].clone();
	pin.instance = Some(first.clone());
	value.contributions.insert("pinned".into(), pin);
	let parent = publish(&origin, "room/live", &root, &value);
	frames(&parent.video, 0);
	frames(&parent.audio, 0);
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	master(&server, "contribution/", false).await;
	let old = publish(&origin, "room/ladder", &first, &catalog());
	let old_uri = format!("contribution/rung%2Fa/video/same/@{first}");
	let listed = master(&server, &old_uri, true).await;
	assert!(listed.contains(&format!("contribution/pinned/video/same/@{first}")));
	assert_eq!(old.video.demand().wanted(), None);
	old.publication.unannounce();
	master(&server, "contribution/", false).await;
	old.publication
		.announce(moq_net::origin::Route::default().with_instance(first.clone(), 0))
		.unwrap();
	master(&server, &format!("contribution/pinned/video/same/@{first}"), true).await;
	let new = publish(&origin, "room/ladder", &second, &catalog());
	let listed = master(&server, &format!("contribution/rung%2Fa/video/same/@{second}"), true).await;
	assert!(!listed.contains("contribution/pinned/"));
	assert!(!listed.contains(&old_uri));
	assert_eq!(new.video.demand().wanted(), None);
	assert_eq!(
		request(&server, &format!("/room/live/@{root}/{old_uri}/media.m3u8"))
			.await
			.0,
		StatusCode::NOT_FOUND
	);
	new.publication.unannounce();
	master(&server, &format!("contribution/pinned/video/same/@{first}"), true).await;
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn denied_and_incompatible_contributions_are_not_advertised() {
	let origin = moq_tokio::origin::spawn();
	let root = Instance::random();
	let parent = publish(&origin, "room/live", &root, &parent_catalog());
	frames(&parent.video, 0);
	frames(&parent.audio, 0);
	let child = Instance::random();
	let mut value = catalog();
	value.media.join = Join::new(Duration::from_secs(1));
	let mut contribution = publish(&origin, "room/ladder", &child, &value);
	let server = Server::new(origin.consume(), Default::default()).unwrap();
	master(&server, "contribution/", false).await;
	value.media.join = None;
	contribution
		.catalog
		.update(moq_net::Timed::from(&value).at(Timestamp::ZERO))
		.unwrap();
	master(&server, "contribution/", false).await;
	assert_eq!(contribution.video.demand().wanted(), None);
	value.media.join = Join::new(Duration::from_secs(2));
	contribution
		.catalog
		.update(moq_net::Timed::from(&value).at(Timestamp::ZERO))
		.unwrap();
	master(&server, "contribution/", true).await;
	let patterns = ["room/live".parse::<moq_net::Pattern>().unwrap()].into_iter().collect();
	let denied = Server::new(origin.consume().scope("", &patterns).unwrap(), Default::default()).unwrap();
	master(&denied, "contribution/", false).await;
	for resource in ["media.m3u8", "init.abcd.mp4", "seg/abcd.0.m4s"] {
		assert_eq!(
			request(
				&denied,
				&format!("/room/live/@{root}/contribution/rung%2Fa/video/same/@{child}/{resource}")
			)
			.await
			.0,
			StatusCode::FORBIDDEN
		);
	}
	assert_eq!(contribution.video.demand().wanted(), None);
}
