# [M] A media hole in the player keeps the timeline

## Goal

When `<moq-watch>` skips a group (a max-delay skip or a hole timeout), audio
and video treat it as a hole on the same timeline: the decoder re-primes,
the audio ring keeps the PCM already decoded, and the shared `Sync` clock
keeps its anchor. Only a real new timeline (a declared end followed by a
later group, or a new instance) re-anchors, as Rust's `moq-audio` already
behaves.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Source-inferred,
inherited (release resets on container `discontinuity` the same way).

With 20 ms audio groups under congestion, one lost group runs
`#onRead` (`js/watch/src/audio/decoder.ts:891`) -> `Terminal` gap ->
`#restart()` (`audio/terminal.ts:57`): `this.sync.reset()` bumps the epoch,
dropping every decoded video frame parked in `Sync.wait` (`sync.ts:304`),
and `this.reset()` zeroes the worklet ring (`ring-buffer.ts:216`). 20 ms of
loss throws away a floor of valid PCM and re-stalls playback; video skips its
in-flight frames. Rust's `rs/moq-audio/src/decode/consumer.rs:513` resets
codec prediction only and plays silence through the hole.
`decoder.test.ts:1182` asserts the flush (with an 840 ms gap) and changes
with the fix.

- Make the container read distinguish a hole from a new timeline, in
  `@moq/hang`'s consumer if that is where the distinction is known.
- On a hole: `decoder.reset()`/`configure()` (Chrome stamps output from its
  first input) and re-prime the `Terminal` epoch; keep the ring (it is
  timestamp-indexed) and `Sync`. Apply the same to video's `#onBreak`
  (`video/decoder.ts:606`, `:691`).
- Each decoder has near-identical legacy and CMAF read loops
  (`audio/decoder.ts:522-664`, `video/decoder.ts:589-728`); fold each into
  one loop over the container format first if that makes the change land
  once instead of twice.

The audit traced about a dozen watch fixes to resets of the shared clock and
ring (8b6abc0a13, 69b0538326, 1696858a0f, 177e92c755, 1a98a3aea7,
076cfb7500, c09e14995a, 4311a7c9a7, 8b83bb5f8a, d6e4f47238, 41906d97e0);
with only a new timeline re-anchoring, most reset call sites should go.

Public API: none. Wire: none.

## Related

- [A/V clock](/quest/m1/av-clock.md) - the video-skip half, and the audio playhead driving `Sync`
- [Buffered playback subscribes in ascending order](/quest/m1/buffered-ascending.md) - the buffered-mode half of the same timeline
