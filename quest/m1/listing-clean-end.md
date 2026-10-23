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
  the `Step::Listed` `Ok(None)` arm), and the source's
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

Rust reproduction in `rs/moq-net/src/ietf/publisher/tracks.rs`:

On base `f719866cea`, all 20 combinations of drafts 18-22, `close()` versus
a dropped `finish()`, and the finished producer retained versus dropped
report status 0 instead of 2. All 20 unfinished-audio controls correctly
report status 0. The simulated-clock test fails its final aggregate
assertion, so every variant is checked.

Insert this into the existing test module and run
`nix develop --command just rs test -p moq-net -E 'test(listing_clean_end_repro)' --no-capture`:

```rust
#[moq_clock::test]
async fn listing_clean_end_repro() {
	let mut failures = Vec::new();
	for version in DRAFTS {
		for finish in [false, true] {
			for retained in [false, true] {
				let mut h = harness(version);
				let mut request = h.request(&listing(false)).await;
				assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID);
				let mut published = HashMap::new();
				for _ in 0..2 {
					let (mut stream, _, name, _) = h.publish().await;
					publish_ok(&mut stream).await;
					published.insert(name, stream);
				}
				moq_clock::sleep(Duration::from_millis(10)).await;
				let video = h.tracks.remove("video").unwrap();
				video.finish().unwrap();
				let held = if retained { Some(video) } else { drop(video); None };
				if finish { drop(h.broadcast.finish()); } else { h.broadcast.close(); }
				for (name, expected) in [("video", ietf::PublishDoneStatus::TrackEnded), ("audio", ietf::PublishDoneStatus::InternalError)] {
					let (id, body) = next(published.get_mut(name).unwrap()).await;
					assert_eq!(id, ietf::PublishDone::ID);
					let done: ietf::PublishDone = body.message(version).unwrap();
					let result = format!("{version}: finish={finish} retained={retained} {name}: actual={} expected={}", done.status_code, expected.code(version));
					eprintln!("{result}");
					if done.status_code != expected.code(version) { failures.push(result); }
				}
				drop(held);
			}
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}
```

### Investigation checkpoint

On base `f719866cea`, the JS wire harness in
`js/net/src/ietf/subscribe_tracks.test.ts` reproduces status 0 instead of 2
for all 20 combinations of drafts 18-22, `close()` versus
`finish().close()`, and a finished track retained versus explicitly removed.
The test uses `Manual` and `clock.advance(Nano.zero)` to settle PUBLISH_OK
before closing, with no wall-clock delay. A local diagnostic that lets
`Queue.next()` drain before its clean end still fails all 20 combinations.
Changing the queue alone therefore does not fix the wire result.

Insert this case inside that file's existing `for (const version of drafts)`
loop, then run
`nix develop --command bun test js/net/src/ietf/subscribe_tracks.test.ts --test-name-pattern 'listing clean end'`:

```typescript
for (const finish of [false, true]) {
  for (const remove of [false, true]) {
    test(`listing clean end ${version} finish=${finish} remove=${remove}`, async () => {
      const clock = new Manual();
      using _installed = { [Symbol.dispose]: clock.install() };
      const fx = fixture(version);
      const broadcast = fx.origin.createBroadcast(Path.from("room/live"));
      broadcast.announce();
      const video = broadcast.createTrack("video");
      const listing = await fx.list();
      await ok(listing, version);
      const { stream } = await fx.offered();
      await clock.advance(Nano.zero);
      video.close();
      if (remove) broadcast.removeTrack("video");
      if (finish) broadcast.finish().close(); else broadcast.close();
      expect(await stream.reader.u53()).toBe(PublishDone.id);
      const done = await PublishDone.decode(stream.reader, version);
      expect(done.statusCode).toBe(PublishDoneStatus.TRACK_ENDED);
    });
  }
}
```

Closing before answering PUBLISH, using `fx.offered(false)` and answering
only after closure, also exposes an AbortError from the publisher's route
watch in all 20 cases. Preserve that pending-answer edge when implementing
the fix; a listed PUBLISH must still conclude with its correct reason.

The remaining ownership boundaries are:

- Rust `broadcast::Alive::close` closes the listing before ending the token;
  `Finish` drops its producer before releasing its finished-track holds.
  Capture finishes from held tracks without resolving lazy front tracks,
  sealing otherwise live tracks, or taking track teardown locks under the
  broadcast lock.
- Rust origin forwarding polls `source.poll_closed` before the source listing.
  `SourceClosed` detaches the source, and `Action::End` closes the front.
  Drain that same source's finish removals before committing its end.
- JS `broadcast.close()` withdraws its announcement before `closeState`
  detaches listing readers. `resume.Broadcast.#list` abandons the reader
  when concluded or replaced; `conclude()` releases its source. Deliver
  terminal removals before releasing that ownership.
- Rust IETF publishing checks the route's `Consumer<()>` before its listing.
  JS IETF publishing uses one AbortController for route withdrawal,
  instance replacement, and authorization removal; its `finally` marks
  remaining entries removed. Neither notification currently acknowledges
  that the origin's terminal listing events have reached the front.

Recommendation: retain public `Tracks` end ordering. The internal proposal
needs a bounded lifecycle handoff: retain the existing source listing long
enough to forward its terminal removals and acknowledge that drain before
the publisher concludes its already-listed PUBLISHes. It must distinguish
that clean-source completion from an unannounce of a still-live source,
instance replacement, authorization loss, and session/request cancellation.
Waiting for every withdrawn listing to end can hang on a live unannounced
broadcast; delaying authorization loss is not an acceptable substitute.
No new per-track subscriptions or unbounded event history should be needed.

The maintainer still owns approval of that handoff and its scope versus a
public ordering change. No production fix is implemented in this checkpoint;
the Goal remains unmet. The failing reproductions are recorded here rather
than installed as failing CI tests. Once implemented, promote the cases to
the existing suites, add the unfinished-track and cancellation controls,
run `just check`, `just ci`, and `just test interop --all`, and update the
matching drafts if observable protocol behavior changes.
