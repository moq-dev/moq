# [S] watch: the video decoder resets on a declared discontinuity

## Goal

A frame submitted before a declared discontinuity never surfaces after it.
The decoder is reset (and reconfigured) when the container consumer reports a
discontinuity, the way the audio decoder already does, so queued pictures from
the old epoch are discarded rather than parked against the new clock.

## Plan

Rescoped in the 2026-09 planning pass: undeclared rewinds are going away
([Monotonic timeline](/quest/m1/monotonic-timeline.md)), so the generation
mismatch below no longer produces the sixty-second park, but the bug it
describes still applies to a declared discontinuity, where a stale frame must
not be presented at all. Fix it the first way the issue suggests: call
`decoder.reset()` and re-`configure()` in `#onDiscontinuity`, so queued chunks
never surface. Keep the post-await generation guard as well: `reset()` cannot
cancel an output callback that already holds a frame and is parked in
`#park()` or `sync.wait()`, and `sync.reset()` releases exactly that wait, so
the guard is what stops a stale callback writing `timestamp` and `frame`
after the reset. The
regression needs a real WebCodecs decoder, so it lives in a browser harness
rather than a bun unit test.

## Required

- [Monotonic timeline](/quest/m1/monotonic-timeline.md) - settles what a discontinuity is before the decoder's reaction to one is pinned

### Issue context

Found during the review of #3048. Pre-existing on `main`; that PR does not introduce it.

#### The bug

`DecoderTrack`'s WebCodecs output callback guards against rewinds with a generation counter:

```ts
output: async (frame) => {
    const generation = this.#discontinuity;   // read when the frame comes OUT
```

The counter is read when the frame is *decoded*, not when its chunk was *submitted*. A frame submitted before a rewind but decoded after it therefore reads the already-bumped value, so the later `generation !== this.#discontinuity` check compares the new value against itself and passes.

The guard only catches frames that were already inside the callback and parked in `#park` when the rewind landed, which is why it sits after the await. Frames still queued inside the `VideoDecoder` are exactly the ones it misses.

`#onDiscontinuity` clears `timestamp`, clears the buffered ranges, and calls `sync.reset()`, but never calls `decoder.reset()`, so the queued chunks keep decoding.

#### Consequence

A stale frame that survives the guard parks against the re-anchored clock for the full distance between the two timelines. With the old timeline at 60s and the rewind restarting at 0, `sync.wait()` computes a sleep of roughly 60s for it. It then paints long after it is meaningless, or gets released early by an unrelated change to the pacing input.

#### Possible fixes

- Call `decoder.reset()` (and re-`configure()`) in `#onDiscontinuity`, which is what the audio decoder already does for its own discontinuities. WebCodecs `reset()` discards queued outputs, so the stale frames never surface.
- Or associate each submitted chunk with the generation it was submitted under and reject mismatches on output.

Either wants a regression test where a pre-rewind output arrives after the reset. That needs a real WebCodecs `VideoDecoder`, which bun's test environment does not provide, so it likely belongs in `test/wasm/` or another browser harness rather than a unit test.

#### Note

The maintainer intends to remove rewind support, in which case deleting the path is the simpler resolution than fixing the generation plumbing. Filing it so the behavior is recorded either way.

## Closes

- [#3056](https://github.com/moq-dev/moq/issues/3056) - close this issue when the quest finishes
