//! A publisher that finishes cleanly leaves a complete recording, whichever of its announce
//! `End` and its catalog's EOF reaches the export first.

use std::time::Duration;

use axum::http::StatusCode;

use crate::support::*;

/// Which reaches the export first.
#[derive(Clone, Copy, Debug)]
enum Order {
	/// The catalog's EOF, then the announce `End`.
	EndAfterEof,
	/// The announce `End`, then the catalog's EOF.
	EndBeforeEof,
}

async fn finish(order: Order) {
	let pair = Pair::connect().await;
	let mut publisher = Publisher::new(&pair.publisher, "live", Default::default());
	let server = pair.server();
	let app = server.router();
	let rendition = rendition(&server, "live").await;
	let mut cursor = rendition.segments();

	publisher.write(0, 13, 1);
	let media = media_uri(&app, "live").await;
	listed(&app, &media, ".2.m4s").await;
	// Reading, as a recorder does, so the cursor's subscription carries every group across.
	let first = kio::clock::timeout(TIMEOUT, cursor.next())
		.await
		.expect("the first segment")
		.expect("records")
		.expect("a segment");
	assert_eq!(first.segment, 0);

	match order {
		Order::EndAfterEof => {
			publisher.finish();
			// The served playlist ends while the broadcast is still announced.
			let playlist = listed(&app, &media, "#EXT-X-ENDLIST").await;
			assert_eq!(numbers(&playlist), [0, 1, 2, 3, 4], "{playlist}");
			publisher.broadcast.close();
		}
		Order::EndBeforeEof => {
			publisher.media.finish().expect("finish media");
			publisher.broadcast.close();
			// The server stops serving the instance once `End` reaches it.
			let deadline = kio::clock::now() + TIMEOUT;
			while get(&app, &media).await.status() != StatusCode::NOT_FOUND {
				assert!(kio::clock::now() < deadline, "End never reached the export");
				kio::clock::sleep(Duration::from_millis(50)).await;
			}
			// `End` alone confirms nothing: the recording stays open until the catalog's EOF.
			let playlist = rendition.media_playlist(None).expect("live").expect("listed");
			assert!(!playlist.contains("#EXT-X-ENDLIST"), "{playlist}");
			publisher.catalog.finish().expect("finish catalog");
		}
	}

	// The final GOP is the last segment, the finish a trailing gap, and the recording reads to
	// its end.
	let (read, end) = drain(&mut cursor).await;
	end.unwrap_or_else(|err| panic!("{order:?}: {err}"));
	assert_eq!(read, [1, 2, 3, 4], "{order:?}");
	let again = kio::clock::timeout(TIMEOUT, cursor.next())
		.await
		.expect("the cursor stalled");
	assert!(matches!(again, Ok(None)), "{order:?}: a finished cursor stays finished");

	let playlist = rendition.media_playlist(None).expect("finished").expect("listed");
	assert!(playlist.ends_with("#EXT-X-ENDLIST\n"), "{order:?}: {playlist}");
	assert_eq!(numbers(&playlist), [0, 1, 2, 3, 4], "{order:?}");
}

#[tokio::test(start_paused = true)]
async fn end_after_catalog_eof() {
	finish(Order::EndAfterEof).await;
}

#[tokio::test(start_paused = true)]
async fn end_before_catalog_eof() {
	finish(Order::EndBeforeEof).await;
}
