# [S] watch: the video decoder resets on a declared discontinuity

## Goal

A frame submitted before a declared discontinuity never surfaces after it.
The decoder is reset and reconfigured when the container consumer reports a
discontinuity, the way the audio decoder already does, so queued pictures from
the old epoch are discarded rather than parked against the new clock.

## Plan

`#onDiscontinuity` (`js/watch/src/video/decoder.ts:528-535`) clears
`timestamp`, clears the buffered ranges, and calls `sync.reset()`, but never
`decoder.reset()`, so chunks still queued in the `VideoDecoder` keep decoding.
The output callback's generation guard reads the counter when the frame comes
out (`:312`), not when its chunk went in, so a frame decoded after the bump
compares the new value against itself at `:335` and passes. It then waits
against the re-anchored clock for the full distance between the two timelines.

- Call `decoder.reset()` and re-`configure()` in `#onDiscontinuity`, as the
  audio decoder does (`js/watch/src/audio/decoder.ts:374-376`). WebCodecs
  `reset()` discards queued outputs, so stale frames never surface.
- Keep the post-await guard. `reset()` cannot cancel a callback that already
  holds a frame and is parked in `Promise.race([wait, effect.cancel])`
  (`decoder.ts:332-334`); `sync.reset()` releases exactly that wait, and the
  guard is what stops it writing `timestamp` and `frame` after the reset.
- The regression needs a real WebCodecs decoder, so it lives in a browser
  harness rather than a bun unit test.

[Monotonic timeline](/quest/m1/monotonic-timeline.md) makes the container
signal a playhead generation, not a codec reset: native decode stops flushing
on it. This quest is whether watch still calls `decoder.reset()` to drop
in-flight WebCodecs chunks when that generation bumps. The decoder is the
same on main, so the fix lands there once the timeline quest has settled.

## Required

- [Monotonic timeline](/quest/m1/monotonic-timeline.md) - settles playhead generation before watch's reaction to it is pinned

## Closes

- [#3056](https://github.com/moq-dev/moq/issues/3056) - close this issue when the quest finishes
