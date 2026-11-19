//! The import reading what the export serves: a pause the export lists as `EXT-X-GAP` slots is
//! followed past, and the media after it is imported.

use std::path::Path;
use std::task::Poll;

use axum::http::StatusCode;

use crate::support::*;

/// Copy the media playlist at `media` and every resource it lists into `dir`, at the paths it
/// names, returning the gaps it lists. Each gap's URI answers 404, as the import's would.
async fn mirror(app: &axum::Router, media: &str, dir: &Path) -> Vec<String> {
	let playlist = String::from_utf8(get(app, media).await.into_body().to_vec()).expect("utf-8");
	let base = media.rsplit_once('/').expect("a directory").0;
	let mut gaps = Vec::new();
	let mut gap = false;
	for line in playlist.lines() {
		let uri = match line.strip_prefix("#EXT-X-MAP:URI=\"") {
			Some(rest) => rest.split('"').next().expect("quoted"),
			None if line == "#EXT-X-GAP" => {
				gap = true;
				continue;
			}
			None if line.starts_with('#') || line.is_empty() => continue,
			None => line,
		};
		let response = get(app, &format!("{base}/{uri}")).await;
		if std::mem::take(&mut gap) {
			assert_missing(&response);
			gaps.push(uri.to_string());
			continue;
		}
		assert_eq!(response.status(), StatusCode::OK, "{uri}");
		let path = dir.join(uri);
		std::fs::create_dir_all(path.parent().expect("a parent")).unwrap();
		std::fs::write(path, response.body()).unwrap();
	}
	std::fs::write(dir.join("media.m3u8"), &playlist).unwrap();
	gaps
}

/// The first timestamp of every group on `track`, in seconds.
fn groups(track: &mut moq_net::track::Ordered) -> Vec<u64> {
	let mut out = Vec::new();
	loop {
		let mut group = match track.poll_next_group(&kio::Waiter::noop()) {
			Poll::Ready(Ok(Some(moq_net::track::Item::Group(group)))) => group,
			Poll::Ready(Ok(Some(moq_net::track::Item::Gap { .. }))) => continue,
			Poll::Pending | Poll::Ready(Ok(None)) => return out,
			Poll::Ready(Err(err)) => panic!("the imported track failed: {err}"),
		};
		let frame = match group.poll_read_frame(&kio::Waiter::noop()) {
			Poll::Ready(Ok(Some(frame))) => frame,
			other => panic!("reading an imported group's first frame: {other:?}"),
		};
		out.push(frame.timestamp.as_secs().expect("seconds"));
	}
}

#[tokio::test(start_paused = true)]
async fn a_paused_export_imports_past_its_gaps() {
	// moq-lite-07 carries the group end that times the last segment; an earlier wire leaves it to
	// the spacing estimate.
	let pair = Pair::new("moq-lite-07-wip".parse().unwrap(), Default::default()).await;
	let mut publisher = Publisher::new(&pair.publisher, "live", Default::default());
	let server = pair.server();
	let app = server.router();
	let dir = tempfile::tempdir().unwrap();

	// Segments 0 and 1 complete, and the GOP at 8s opens segment 2.
	publisher.write(0, 9, 1);
	let media = media_uri(&app, "live").await;
	listed(&app, &media, ".1.m4s").await;
	assert!(mirror(&app, &media, dir.path()).await.is_empty());

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let playlist = url::Url::from_file_path(dir.path().join("media.m3u8")).unwrap();
	let mut import = moq_hls::import::Import::new(broadcast, catalog.clone(), moq_hls::import::Config::new(playlist))
		.expect("import");
	import.init().await.expect("joins the live playlist");

	let name = catalog
		.snapshot()
		.video
		.renditions
		.keys()
		.next()
		.expect("video")
		.clone();
	let replay = moq_net::track::Subscription::default()
		.with_delay(moq_net::track::Delay::default().with_max(std::time::Duration::from_secs(3600)));
	let mut track = consumer
		.track(&name)
		.unwrap()
		.subscribe(replay)
		.await
		.unwrap()
		.ordered();

	// A pause skips the slots from 12s to 20s, which the export lists as gaps, then the
	// broadcast resumes and ends.
	publisher.write(20, 27, 1);
	publisher.finish();
	listed(&app, &media, "#EXT-X-ENDLIST").await;
	let gaps = mirror(&app, &media, dir.path()).await;
	assert_eq!(gaps.len(), 2, "{gaps:?}");

	kio::clock::timeout(TIMEOUT, import.run())
		.await
		.expect("the import reaches the end")
		.expect("the import follows the playlist past its gaps");
	assert_eq!(groups(&mut track), [0, 2, 4, 6, 8, 20, 22, 24, 26]);
	drop(import.finish().unwrap());
}
