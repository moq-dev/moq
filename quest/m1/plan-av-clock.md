# [M] The audio playhead drives Sync.reference while audio plays

## Goal

`js/watch` keeps audio and video in sync from one clock. While audio plays,
`Sync.reference` derives from the audio playhead; muted or video-only playback
falls back to the wall clock. Today the ring is depth-driven and free-runs on
the AudioContext clock while video paces against a wall clock anchored at the
earliest arrival, so a ring that re-buffers or skips drifts against video until
the next re-anchor.

## Plan

Settled: per-track handles, and this quest lands them. `sync.track("audio")`
and `sync.track("video")` each report their advertised delay and measured
spread, and one is nominated as the clock source. `SyncInput`
(`js/watch/src/sync.ts:21-46`, today `delay`, `buffer`, `probe`, `audio`,
`video`) breaks once, and a third track joins without another pair of inputs.
The measured spread per track comes from
[Watch](/quest/m2/audio-jitter-target/watch.md); its branch carries flat
`audioSpread` and `videoSpread` inputs in place of `probe`, which this quest
folds into the handles. This is a published `@moq/watch` break, so it targets
`dev`.

Recommendations for the implementation:

- Playhead source. On the SharedArrayBuffer path the worklet's sample counter
  is the playhead (`js/watch/src/audio/shared-ring-buffer.ts`). On the
  postMessage path (`js/watch/src/audio/ring-buffer.ts`) the worklet posts an
  estimate and the main thread extrapolates between posts.
- Video reads a locally extrapolated clock, re-synced once per audio quantum,
  so the per-frame `sync.wait()` (`js/watch/src/video/decoder.ts:332`) never
  crosses a thread.
- Transitions. On mute or audio track end the reference falls back to the
  wall clock at the last audio-derived value, so video does not jump. A ring
  re-stall reads as the playhead pausing, and the reference pauses with it.
- Reset coupling stays: `<moq-watch>` already flushes the ring alongside
  `sync.reset()` (`js/watch/src/element.ts:301`, `:620-621`).
- The text renderer is the third track: it reads `sync.now()`
  (`js/watch/src/text/renderer.ts:261`) to drive the cue clock and prune cues
  at `:263-265`.

## Related

- [Watch](/quest/m2/audio-jitter-target/watch.md) - the per-track spread estimator this shape carries
- [Audio jitter target](/quest/m2/audio-jitter-target/README.md) - the estimator this sits on
- [Time stretch](/quest/m2/watch-audio-time-stretch.md) - stretching needs a clock to converge toward
