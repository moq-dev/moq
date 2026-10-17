# [S] An audio rename keeps the arrival estimate

## Goal

When `@moq/publish` renames an audio track after a decoder identity change,
`@moq/watch` keeps playing audio without skipping or stalling: the replacement
audio consumer continues the outgoing track's arrival estimate, as video does.
A track read from another broadcast path still starts from the cold-start
prior, as `doc/concept/audio-jitter.md` ("Across renditions") requires.

## Plan

#5635 added `ConsumerProps.predecessor` to `@moq/hang` and the video hand-off
this quest mirrors; this quest adds no API of its own.

Today the audio decoder's `#runLegacyDecoder` and `#runCmafDecoder` clear
`#measured` on teardown, before the replacement subscription opens, and the
new `Container.Consumer` starts from the prior. At a rename, the audio target
drops out of the shared delay, then returns at the cold 100 ms and climbs as
the new track relearns the path. The ring skips on the drop and parks on each
rise, the same failure #5635 fixed for video.

Decided 2026-10-16 (recommended options; the paper trail is in the plan PR):

- Hand the outgoing `Container.Consumer` to the replacement through
  `ConsumerProps.predecessor`, only when both read the same broadcast path,
  mirroring the video decoder. No new API.
- Keep the outgoing estimate in `#measured` until the replacement's consumer
  exists. Clear it only when no replacement follows (disabled, muted, or the
  rendition removed), so a departed track still stops holding the buffer open.
- Prove it with a unit test in `js/watch/src/audio/decoder.test.ts` on a
  mocked clock: warm a bursty audio track, rename it, and assert the ring
  neither skips nor goes silent and the delay never drops. A second case moves
  the media to another path and asserts it does not inherit. No new
  real-browser media case: the unit test reproduces the mechanism
  deterministically, and the fixture would need a second audio codec
  configuration to rename audio at all.

Out of scope: `rs/moq-audio` builds a fresh `Target` per decode consumer, so a
native player that follows a rename restarts cold too. That needs its own
quest if native playback follows renames.

Public API: none. Wire: none.

## Related

- [Browser follows a renamed track](/quest/m1/browser-rename-check.md) - the browser check that found the video stall
