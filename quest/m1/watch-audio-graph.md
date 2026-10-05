# [S] The watch audio graph outlives a catalog absence

## Goal

When an audio rendition leaves the catalog, or is disabled, and comes back
with the same shape, a listener hears the queued tail before the pause and
the first frames after it, and an idle graph costs no CPU while it waits.

## Plan

`js/watch/src/audio/decoder.ts` keys the AudioContext and worklet on
`#config`. When the rendition leaves the catalog the config is undefined, the
effect's cleanup closes the context, and the queued tail is cut. On return the
decoder subscribes at once while the ring is still awaiting `addModule`, and
`#emit` drops samples with no ring. The graph also rebuilds on codec,
container, or description changes it does not depend on.

Decided (2026-10-04):

- Key the graph on sample rate and channel count only; close it on `close()`
  or a real shape change, never on absence. No idle timeout.
- Once the ring drains during an absence, `suspend()` the context, and
  `resume()` on return, so pages with many tiles pay nothing while idle.
- Subscribe at once and start the decode loop only once the ring exists, so
  early frames wait in the consumer instead of being dropped.
- This fixes #4780's symptoms for every publisher, including ones that remove
  a rendition rather than disable it.
- Measure whether any start loss remains from the decoder warmup, and leave
  that to the [audio warmup](/quest/m1/audio-warmup.md) quest if so.

Test: a rendition removed and re-added with the same shape keeps one
AudioContext, suspends once drained, plays the ring to empty, and emits the
first frame after return.

## Closes

- [#4780](https://github.com/moq-dev/moq/issues/4780) - close this issue when the quest finishes

## Related

- [Enabled flag](/quest/m1/catalog-enabled.md) - a publisher mute disables the rendition instead of removing it
- [Delay rebuild](/quest/m0/watch-delay-rebuild.md) - the other needless audio teardown in this file
- [Audio warmup](/quest/m1/audio-warmup.md) - the Opus pre-roll trim on the same path
