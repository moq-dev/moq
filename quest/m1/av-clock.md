# [M] The audio playhead drives Sync.reference while audio plays

## Goal

`js/watch` keeps audio and video in sync from one clock. While audio plays,
`Sync.reference` derives from the audio playhead; muted or video-only playback
falls back to the wall clock. Today the ring is depth-driven and free-runs on
the AudioContext clock while video paces against a wall clock anchored at the
earliest arrival, so a ring that re-buffers or skips drifts against video until
the next re-anchor.

Acceptance includes the 2026-10-07 audit's skip case: a video-only latency
skip (`js/watch/src/video/decoder.ts`, `sync.reset()` on a video
discontinuity) must not move audio's timeline. Today it re-anchors the shared
`Sync` while the audio ring stays on the old one. Folded in here rather than
fixed separately, since audio driving the clock removes the shared anchor.

## Plan

Moved from m0 to m1 in the 2026-09-30 audit: it waits on the whole jitter
line and is a published `@moq/watch` break, so it is not in flight.

Settled: per-track handles, and this quest lands them. `sync.track("audio")`
and `sync.track("video")` each report their advertised delay and measured
spread, and one is nominated as the clock source. `SyncInput`
(`js/watch/src/sync.ts`, today `delay`, `buffer`, and `probe`) breaks once,
and a third track joins without another pair of inputs; `Sync.register`
already keeps one jitter entry per track and grows into the handles. The
measured target per track comes from the [Audio jitter
target](/quest/m1/audio-jitter-target/README.md) line: each decoder registers
its own target through `Sync.register`, and `probe` stays in `SyncInput` unread
so that line lands on `main`. This quest folds the registrations into the
handles and drops `probe`. `SyncInput` is a published `@moq/watch` shape, so
this is a break.

Recommendations for the implementation:

- Playhead source. On the SharedArrayBuffer path the worklet's sample counter
  is the playhead (`js/watch/src/audio/shared-ring-buffer.ts`). On the
  postMessage path (`js/watch/src/audio/ring-buffer.ts`) the worklet posts an
  estimate and the main thread extrapolates between posts.
- Video reads a locally extrapolated clock, re-synced once per audio quantum,
  so the per-frame `sync.wait()` (`js/watch/src/video/decoder.ts`) never
  crosses a thread.
- Transitions. On mute or audio track end the reference falls back to the
  wall clock at the last audio-derived value, so video does not jump. A ring
  re-stall reads as the playhead pausing, and the reference pauses with it.
- Reset coupling stays: `Player.reset()` (`js/watch/src/player.ts`) already
  flushes the audio ring alongside `sync.reset()`.
- The text renderer is the third track: it reads `sync.now()`
  (`js/watch/src/text/renderer.ts`) to drive the cue clock and prune cues.
- Close the player gaps [#4170](https://github.com/moq-dev/moq/pull/4170)
  left, since the handles own them. `Sync.received` only ever lowers its
  reference, so after the earliest subscribed track leaves, playback stays
  anchored to it; a track's handle going away must release its part of the
  reference, the same expiry the jitter target's arrival minimum has. Text
  renditions register no floor with `Sync`, so their catalog `delay` is
  ignored; the text handle registers one like audio and video. The MSF
  catalog schema (`js/msf/src/catalog.ts`) accepts a negative `delay` and
  folds it into absent; refuse it on decode instead.

## Required

- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the estimator this sits on, and the per-track targets this shape carries

## Related

- [Time stretch](/quest/m1/watch-audio-time-stretch.md) - stretching needs a clock to converge toward
- [Watch worker](/quest/m1/watch-worker.md) - moves `Sync` into a worker afterwards; keep the handles free of main-thread assumptions
