# [M] A listing's clean end keeps its finishes

## Goal

When a broadcast ends right after its tracks finish, by `close()` or by a
clean `finish()`, each IETF SUBSCRIBE_TRACKS PUBLISH for a finished track
ends with TRACK_ENDED, not INTERNAL_ERROR, in Rust and JS. A track left
unfinished still ends in error.

## Plan

Split from [moq-dev/moq#6399](https://github.com/moq-dev/moq/pull/6399)
(Codex finding
[r4357921727](https://github.com/moq-dev/moq/pull/6399#discussion_r4357921727)),
whose fix predates it. A listing reports its end ahead of the removals still
queued, so a finish queued just before the end is never read.

Why a "finished cleanly" check on the PUBLISH's track handle can't work:

- The IETF publisher gets every broadcast from `origin.request_broadcast`, so
  its handles are lazy lookups on the origin's front broadcast. A track is only
  resolved there when something subscribes, queries, or fetches it, so a
  paused PUBLISH's handle never sees its track's end. Making it resolve would
  add upstream demand for every listed track.
- The origin forwarder drops the finish before the front sees it: on the
  source listing's end it drops the reader (`rs/moq-net/src/model/origin.rs`,
  the `Step::Listed` `Ok(None)` arm, around lines 3966-3973), and the source's
  queued `Finished` is lost.
- `broadcast.finish()` is no better: a finished track is listed while held,
  and the listing closes before the holds drop, so no `Finished` is queued at
  all.

Open design (maintainer to decide):

1. Crate-internal drain, public `Tracks` order unchanged: crate readers get
   queued removals before a clean end. Also needed: a broadcast close unlists
   its held finished tracks as finished before closing the listing; the origin
   forwarder drains a source's removals before acting on its end; the IETF
   publisher reads the listing to its end after the route closes instead of
   acting on the route first. Mirror in JS (`#listBroadcast`'s `finally` in
   `js/net/src/ietf/publisher.ts` marks every entry "removed").
2. Change the public `Tracks` order so a clean end follows the queued
   removals. Fixes the cause at its source, but was ruled out for #6399, and
   still needs the forwarder, close, and publisher changes from option 1.

Starting point: this test in `rs/moq-net/src/ietf/publisher/tracks.rs` fails
on drafts 18-22 (the finished track gets status 0, not 2). Swapping
`h.broadcast.close()` for a dropped `finish()` fails the same way.

```rust
#[moq_clock::test]
async fn a_finished_track_ends_cleanly_when_its_listing_ends() {
	for version in DRAFTS {
		let mut h = harness(version);
		let mut request = h.request(&listing(false)).await;
		assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
		let mut published = HashMap::new();
		for _ in 0..2 {
			let (mut stream, _, name, _) = h.publish().await;
			publish_ok(&mut stream).await;
			published.insert(name, stream);
		}
		moq_clock::sleep(Duration::from_millis(10)).await;

		let video = h.tracks.remove("video").unwrap();
		video.finish().unwrap();
		drop(video);
		h.broadcast.close();
		for (name, code) in [
			("video", ietf::PublishDoneStatus::TrackEnded.code(version)),
			("audio", ietf::PublishDoneStatus::InternalError.code(version)),
		] {
			let (id, body) = next(published.get_mut(name).unwrap()).await;
			assert_eq!(id, ietf::PublishDone::ID, "{version}: {name}");
			let done: ietf::PublishDone = body.message(version).unwrap();
			assert_eq!(done.status_code, code, "{version}: {name}");
		}
	}
}
```
