# [S] The watch audio graph outlives a catalog absence

## Goal

When a publisher mutes by disabling its audio encoder, the rendition leaves
the catalog and comes back, and a listener hears the queued tail before the
pause and the first frames after it.

## Plan

`js/watch/src/audio/decoder.ts` builds the AudioContext and worklet from
`#config`. When the rendition leaves the catalog the config is undefined, the
effect's cleanup closes the context, and the queued tail is cut. On return the
decoder subscribes at once while the ring is still awaiting `addModule`, and
`#emit` drops samples with no ring.

Decided (2026-10-04):

- Key the graph on the last known sample rate and channel count; close it only
  on `close()` or a real shape change, never on catalog absence.
- The decoder waits for the ring before subscribing, so early frames wait in
  the consumer instead of being dropped. No idle timeout.
- Measure whether any start loss remains from the decoder warmup, and leave
  that to the [audio warmup](/quest/m1/audio-warmup.md) quest if so.

Test: a rendition removed and re-added with the same shape keeps one
AudioContext, plays the ring to empty, and emits the first frame after return.

## Closes

- [#4780](https://github.com/moq-dev/moq/issues/4780) - close this issue when the quest finishes

## Related

- [Delay rebuild](/quest/m0/watch-delay-rebuild.md) - the other needless audio teardown in this file
- [Audio warmup](/quest/m1/audio-warmup.md) - the Opus pre-roll trim on the same path
