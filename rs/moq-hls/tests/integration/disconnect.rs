//! A publisher whose session drops leaves an incomplete recording: the cursor drains what
//! already arrived, then fails with a typed error, and the playlist refuses.

use axum::http::StatusCode;

use crate::support::*;

/// Where in the publisher's output the session drops.
#[derive(Clone, Copy, Debug)]
enum At {
	/// With a GOP open past the last completed segment.
	Segment,
	/// Right after a catalog update, before it reaches the export.
	Catalog,
}

/// Drop the publisher's session at `at` and check the recording ends as incomplete.
///
/// The catalog's and the records' subscriptions both fail with the session. Whichever reaches
/// the cursor first must end it with a typed error, so the order is left to the session.
async fn disconnect(at: At) {
	let mut pair = Pair::connect().await;
	let mut publisher = Publisher::new(&pair.publisher, "live", Default::default());
	let server = pair.server();
	let app = server.router();
	let rendition = rendition(&server, "live").await;
	// Recording from the start, as a recorder does, so every group reaches the export.
	let mut cursor = rendition.segments();

	// Three completed segments, then a GOP still open at 12s.
	publisher.write(0, 13, 1);
	let media = media_uri(&app, "live").await;
	let playlist = listed(&app, &media, ".2.m4s").await;
	assert_eq!(numbers(&playlist), [0, 1, 2], "{playlist}");
	let first = kio::clock::timeout(TIMEOUT, cursor.next())
		.await
		.expect("the first segment")
		.expect("records")
		.expect("a segment");
	assert_eq!(first.segment, 0);

	if let At::Catalog = at {
		publisher
			.catalog
			.mutate(|catalog| {
				let config = catalog.video.renditions.get_mut(TRACK).expect("listed");
				config.coded_width = Some(640);
				config.coded_height = Some(480);
			})
			.expect("catalog update");
	}
	pair.disconnect();

	// The completed tail drains, and the open GOP is never read as a segment.
	let (read, end) = drain(&mut cursor).await;
	assert_eq!(read, [1, 2], "{at:?}");
	let err = end.expect_err("an incomplete recording never reads as complete");
	// A raw transport error would read as retryable, so a recorder would never finalize.
	assert!(
		matches!(
			err,
			moq_hls::Error::CatalogFailed { .. }
				| moq_hls::Error::RecordsFailed { .. }
				| moq_hls::Error::SegmentUnavailable { .. }
		),
		"{at:?}: a disconnect ended the recording with {err:?}"
	);
	// The failure is final: every later call answers it again.
	for _ in 0..2 {
		let again = kio::clock::timeout(TIMEOUT, cursor.next())
			.await
			.expect("the cursor stalled");
		let Err(again) = again else {
			panic!("{at:?}: a failed cursor resumed");
		};
		assert_eq!(again.to_string(), err.to_string(), "{at:?}");
	}

	// The playlist refuses rather than ending with `EXT-X-ENDLIST`.
	let refused = rendition
		.media_playlist(None)
		.expect_err("the playlist refuses rather than ending");
	assert!(
		matches!(
			refused,
			moq_hls::Error::CatalogFailed { .. } | moq_hls::Error::RecordsFailed { .. }
		),
		"{at:?}: {refused:?}"
	);
	let response = get(&app, &media).await;
	assert_ne!(
		response.status(),
		StatusCode::OK,
		"{at:?}: {}",
		String::from_utf8_lossy(response.body())
	);
	drop(publisher);
}

#[tokio::test(start_paused = true)]
async fn mid_segment() {
	disconnect(At::Segment).await;
}

#[tokio::test(start_paused = true)]
async fn mid_catalog() {
	disconnect(At::Catalog).await;
}
