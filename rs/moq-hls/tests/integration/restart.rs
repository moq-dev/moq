//! A new publisher instance taking over a path is served under its own URLs, while the run it
//! replaced finishes its own recording and never serves the new run's bytes.

use std::time::Duration;

use axum::http::StatusCode;

use crate::support::*;

const OLD: u8 = 1;
const NEW: u8 = 2;

/// The old run keeps publishing through the takeover and ends after it. Until a replaced run's
/// drain has a cutoff of its own, its cursor ends only once the old publisher does.
#[tokio::test(start_paused = true)]
async fn takeover() {
	let pair = Pair::connect().await;
	let mut old = Publisher::new(&pair.publisher, "live", Default::default());
	let server = pair.server();
	let app = server.router();
	let rendition = rendition(&server, "live").await;
	let mut cursor = rendition.segments();

	old.write(0, 13, OLD);
	let old_media = media_uri(&app, "live").await;
	listed(&app, &old_media, ".2.m4s").await;
	let replaced = server.broadcaster("live").await.expect("announced");
	replaced.set_generation(Some("run-1")).expect("generation");
	let playlist = listed(&app, &old_media, "seg/run-1.").await;
	let old_segments: Vec<_> = (0..3).map(|index| segment_uri(&old_media, &playlist, index)).collect();
	for uri in &old_segments {
		assert_old(&app, uri).await;
	}
	// Reading, as a recorder does, so the cursor's subscription carries every group across.
	let first = moq_clock::timeout(TIMEOUT, cursor.next())
		.await
		.expect("the first segment")
		.expect("records")
		.expect("a segment");
	assert_eq!(first.segment, 0);
	assert!(contains(&first.media, &[OLD; 8]));

	// The new instance numbers its groups and segments from zero again.
	let route = moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint());
	let mut new = Publisher::new(&pair.publisher, "live", route);
	let deadline = moq_clock::now() + TIMEOUT;
	let current = loop {
		let current = server.broadcaster("live").await.expect("announced");
		if !std::sync::Arc::ptr_eq(&current, &replaced) {
			break current;
		}
		assert!(moq_clock::now() < deadline, "the new instance never took over");
		moq_clock::sleep(Duration::from_millis(50)).await;
	};
	current.set_generation(Some("run-2")).expect("generation");
	new.write(0, 13, NEW);

	// The master lists the new instance, whose playlist lists its own generation.
	let new_media = media_uri(&app, "live").await;
	assert_ne!(new_media, old_media, "a new instance gets its own URLs");
	let playlist = listed(&app, &new_media, ".2.m4s").await;
	assert!(!playlist.contains("run-1."), "{playlist}");
	let new_segment = segment_uri(&new_media, &playlist, 0);
	assert!(new_segment.contains("/seg/run-2."), "{new_segment}");
	let response = get(&app, &new_segment).await;
	assert_eq!(response.status(), StatusCode::OK);
	assert!(contains(response.body(), &[NEW; 8]) && !contains(response.body(), &[OLD; 8]));
	// Nor does the new instance answer the old run's generation.
	let crossed = new_segment.replace("/seg/run-2.", "/seg/run-1.");
	assert_eq!(get(&app, &crossed).await.status(), StatusCode::NOT_FOUND);

	// The old run's URLs, under either generation, never answer with the new run's bytes.
	for uri in &old_segments {
		assert_old(&app, uri).await;
		assert_old(&app, &uri.replace("/seg/run-1.", "/seg/run-2.")).await;
	}

	// The old run ends while the new one carries on past the old run's last group.
	old.finish();
	new.write(13, 21, NEW);
	drop(old);

	// The old recording drains every segment of its own, its final GOP included, and nothing else.
	let mut read = Vec::new();
	loop {
		let next = moq_clock::timeout(TIMEOUT, cursor.next())
			.await
			.expect("the replaced run's cursor never ended");
		let Some(segment) = next.expect("the replaced run finished cleanly") else {
			break;
		};
		assert!(
			contains(&segment.media, &[OLD; 8]) && !contains(&segment.media, &[NEW; 8]),
			"the replaced run's segment {} carries the new run's media",
			segment.segment
		);
		read.push(segment.segment);
	}
	assert_eq!(read, [1, 2, 3]);
	let playlist = rendition.media_playlist(None).expect("finished").expect("listed");
	assert!(playlist.ends_with("#EXT-X-ENDLIST\n"), "{playlist}");

	for uri in &old_segments {
		assert_old(&app, uri).await;
	}
	assert_eq!(get(&app, &new_segment).await.status(), StatusCode::OK);
	drop(new);
}

/// `uri` answers the old run's bytes or nothing at all.
async fn assert_old(app: &axum::Router, uri: &str) {
	let response = get(app, uri).await;
	assert!(!contains(response.body(), &[NEW; 8]), "{uri} served the new run");
	match response.status() {
		StatusCode::OK => assert!(contains(response.body(), &[OLD; 8]), "{uri}"),
		_ => assert_missing(&response),
	}
}

/// The URI of the `index`th segment `playlist` lists, beside the media playlist at `media`.
fn segment_uri(media: &str, playlist: &str, index: usize) -> String {
	let line = playlist
		.lines()
		.filter(|line| line.starts_with("seg/"))
		.nth(index)
		.unwrap_or_else(|| panic!("no segment {index} in {playlist}"));
	media.replace("media.m3u8", line)
}
