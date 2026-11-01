//! A new publisher instance taking over a path is served under its own URLs, while the run it
//! replaced finishes its own recording and never serves the new run's bytes.

use std::time::Duration;

use axum::http::StatusCode;
use moq_hls::export::segments;

use crate::support::*;

const OLD: u8 = 1;
const NEW: u8 = 2;

/// What the replaced run's recording holds when the new instance takes over.
#[derive(Clone, Copy, Debug)]
enum Case {
	/// A cached segment returned while the cursor's live SUBSCRIBE was still unanswered.
	/// Without an epoch nothing says which instance answers it, so it is refused and the
	/// recording loses the old run's last uncached GOP.
	Unanswered,
	/// As [`Self::Unanswered`], while the replacement's own group of the same number is
	/// still open, which the old cursor once waited on forever.
	UnansweredUnfinished,
	/// The live subscription was answered before the takeover, so it stays on the old run
	/// and drains every segment of its own.
	Answered,
	/// As [`Self::Unanswered`], with the old run finished just before the new instance
	/// announces.
	FinishedFirst,
}

async fn takeover(version: &str, case: Case) {
	let pair = Pair::new(version.parse().unwrap(), Default::default()).await;
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
	// Read as a recorder does. A cached segment can return while the live feed is still opening.
	let first = kio::clock::timeout(TIMEOUT, cursor.next())
		.await
		.expect("the first segment")
		.expect("records")
		.expect("a segment");
	assert_eq!(first.segment, 0);
	assert!(contains(
		first.media.as_deref().expect("first segment media"),
		&[OLD; 8]
	));
	match case {
		// Paused, the clock only moves once every task is idle, so the feed has its answer.
		Case::Answered => kio::clock::sleep(Duration::from_millis(500)).await,
		Case::FinishedFirst => old.finish(),
		Case::Unanswered | Case::UnansweredUnfinished => {}
	}

	// The new instance numbers its groups and segments from zero again.
	let route = moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint());
	let mut new = Publisher::new(&pair.publisher, "live", route);
	let deadline = kio::clock::now() + TIMEOUT;
	let current = loop {
		let current = server.broadcaster("live").await.expect("announced");
		if !std::sync::Arc::ptr_eq(&current, &replaced) {
			break current;
		}
		assert!(kio::clock::now() < deadline, "the new instance never took over");
		kio::clock::sleep(Duration::from_millis(50)).await;
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

	// The old run ends, while the new one carries on past the old run's last group unless it
	// leaves that group open.
	if !matches!(case, Case::FinishedFirst) {
		old.finish();
	}
	if !matches!(case, Case::UnansweredUnfinished) {
		new.write(13, 21, NEW);
	}
	drop(old);

	// The old recording reads only its own segments, and either completes or fails at the GOP
	// it lost.
	let (read, end) = drain_old(&mut cursor).await;
	match case {
		Case::Answered => {
			end.unwrap_or_else(|err| panic!("{case:?}: {err}"));
			assert_eq!(read, [1, 2, 3], "{case:?}");
			let playlist = rendition.media_playlist(None).expect("finished").expect("listed");
			assert!(playlist.ends_with("#EXT-X-ENDLIST\n"), "{case:?}: {playlist}");
		}
		Case::Unanswered | Case::UnansweredUnfinished | Case::FinishedFirst => {
			assert!(
				matches!(end, Err(moq_hls::Error::SegmentUnavailable { segment: 3 })),
				"{case:?}: {end:?}"
			);
			assert_eq!(read, [1, 2], "{case:?}");
		}
	}

	for uri in &old_segments {
		assert_old(&app, uri).await;
	}
	drop(new);
}

#[tokio::test(start_paused = true)]
async fn unanswered_lite06() {
	takeover("moq-lite-06", Case::Unanswered).await;
}

#[tokio::test(start_paused = true)]
async fn unanswered_lite07() {
	takeover("moq-lite-07-wip", Case::Unanswered).await;
}

#[tokio::test(start_paused = true)]
async fn unanswered_unfinished() {
	takeover("moq-lite-07-wip", Case::UnansweredUnfinished).await;
}

#[tokio::test(start_paused = true)]
async fn answered() {
	takeover("moq-lite-07-wip", Case::Answered).await;
}

#[tokio::test(start_paused = true)]
async fn finished_first() {
	takeover("moq-lite-07-wip", Case::FinishedFirst).await;
}

/// Read `cursor` until it ends, asserting each segment carries only the old run's media.
async fn drain_old(cursor: &mut segments::Consumer) -> (Vec<u64>, moq_hls::Result<()>) {
	let mut read = Vec::new();
	loop {
		let next = kio::clock::timeout(TIMEOUT, cursor.next())
			.await
			.expect("the replaced run's cursor stalled");
		let segment = match next {
			Ok(Some(segment)) => segment,
			Ok(None) => return (read, Ok(())),
			Err(err) => return (read, Err(err)),
		};
		// The stop the old run ends with is a gap, which carries no media of either run.
		let Some(media) = segment.media.as_deref() else {
			continue;
		};
		assert!(
			contains(media, &[OLD; 8]) && !contains(media, &[NEW; 8]),
			"the replaced run's segment {} carries the new run's media",
			segment.segment
		);
		read.push(segment.segment);
	}
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

/// Whether `haystack` contains `needle`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
	haystack.windows(needle.len()).any(|window| window == needle)
}
