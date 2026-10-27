# [M] HLS takeover leaks the replacement's media

## Goal

After a new publisher instance takes over a broadcast path, a recording cursor on the
replaced instance never yields the replacement's media. It drains the old run's own
segments, then ends cleanly or fails with a typed error. It never stalls.

The `takeover` scenario below lands in `rs/moq-hls/tests/integration/restart.rs` with
the fix, as part of the session suite.

## Plan

Found 2026-10-25 by the session suite. The takeover scenario fails on `main`.

**Symptom.** The old run publishes three complete segments and leaves a GOP open at 12s.
A new instance takes over `live` and publishes its own groups from zero. The old run
finishes. The old cursor reads segments 1 and 2 with the old bytes. Segment 3, the old
run's final GOP (group 6), comes back with the new run's bytes.

- If the new run has not completed its own group 6, the old cursor stalls on segment 3
  instead.
- The leak also happens when the old run finishes before the new one announces.
- It reproduces on moq-lite-06 (the default) and moq-lite-07-wip, with an epochless old
  route and a new route with an epoch.
- Over HTTP, the old instance's URLs answer 404 once it is replaced, so only the cursor
  leaks.

**Where the fetch goes.** Temporary logging in `Rendition::fetch`
(`rs/moq-hls/src/export/rendition.rs`) showed the segment-3 fetch going through the
replaced broadcast: its epoch was `None` and `is_closed()` was true. The fetch reads
through the track that `Handle::capture` pinned with a `track.query()` before the
takeover, yet `fetch_group(6)` returned the replacement's group with the same sequence.

**A bare moq-net reproduction did not leak.** The setup used lite-07, an epochless old
broadcast, and a held `track.query()` taken before the replacement, with:

- a subscription on the old track, with its in-flight group either read or dropped unread;
- the replacement announced with an epoch and writing groups 0 to 2;
- the old group 1 finished after the takeover;
- in one variant, a concurrent subscription to the replacement.

`fetch_group(1)` on the old query returned the old bytes. Groups the old subscription
never delivered answered `Unroutable`. So the leak needs something the export adds.

**Where I looked:**

- `rs/moq-hls/src/export/rendition.rs`: `Media`, `Handle::capture`, `poll_sync`,
  `poll_track`, `Rendition::fetch`, and the free `fetch`. Pinning and window-ending on
  close apply only to siblings (`self.sibling`). The catalog's own broadcast relies on
  the source pin.
- `rs/moq-hls/src/export/segments.rs`: `Consumer::poll_feed`. The cursor's feed
  subscribes through the binding (`poll_subscribe`, `FEED_MAX_DELAY`) and drops each
  group unread. Its doc says a resumed subscription "only pulls while it is read".
- `rs/moq-mux/src/source.rs`: `Source::new` pins the catalog path to the served
  instance; `Binding::poll_broadcast` returns the pin.
- `rs/moq-net-tests/tests/integration/restart.rs`: `retired_fetch` covers a retired handle
  whose query is taken after the restart, not one held before it.
- `rs/moq-net/src/lite/subscriber.rs`: `SubscriptionCleanup` tears subscriptions down in
  `HashMap` order. That explains why the disconnect failure order varies, not this leak.

**Leading hypothesis, unconfirmed:** the feed subscription, or a resubscribe from the
pinned query, resumes across the route change onto the replacement and fills the
retired track's cache for a group the old run had not delivered yet. Confirm by logging
which subscription delivers group 6 into the old track, then decide whether moq-net
refuses the resume or the export stops reading a replaced instance's tail.

The takeover scenario uses the suite's `support` harness and needs a `contains` helper,
which was dropped from `support.rs` when the scenario moved here:

```rust
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

/// Whether `haystack` contains `needle`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
	haystack.windows(needle.len()).any(|window| window == needle)
}
```

## Related

- [HLS restart drain cutoff](/quest/m2/hls-restart-drain-cutoff.md) - changes what a replaced run's cursor drains; update the takeover scenario with it
- `export hls --output` (`rs/moq-cli/src/hls/record.rs`) - the recorder that would write the leaked segment
- moq-hls's `moq-tokio` dev-dependency features (`tcp`, `quic`, `aws-lc-rs`) now serve only `origin::spawn`. Trimming them to `net` first needs moq-tokio's `Backoff::validate` fixed, since it is dead code under `net` alone.
