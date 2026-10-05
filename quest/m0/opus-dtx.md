# [XS] Voice audio publishes without Opus DTX

## Goal

`@moq/publish` no longer enables Opus DTX, by default or on request, so the
published audio timeline tracks the capture clock through silence and the
advertised jitter stays at its measured value instead of climbing to seconds.

## Plan

Chromium stamps encoder output as the first input timestamp plus the samples
it emitted, so with DTX every suppressed frame pulls later audio earlier.
`js/publish/src/audio/encoder.ts` publishes chunks at the encoder's timestamp
and only re-bases on an input gap, never on output suppression.

Decided (2026-10-04):

- Delete `usedtx: true` from the voice defaults (`opusKindDefaults`) and
  remove the `usedtx` option. A caller that still passes it gets an error,
  like the existing `source` check, rather than a silent drop. Remove the
  demo's DTX checkbox. Rust `Settings::dtx` stamps by input count and stays.
- This is a stopgap that costs voice bandwidth during silence; restoring DTX
  is [its own quest](/quest/m2/opus-dtx-timestamps.md). It lands first
  because the audio jitter target's acceptance run with a real microphone
  would otherwise measure DTX drift as network jitter.

Test: the voice config omits `usedtx`, and passing it throws. Bun has no
Chromium encoder, so a timeline test could not fail there.

## Closes

- [#4783](https://github.com/moq-dev/moq/issues/4783) - close this issue when the quest finishes

## Related

- [Audio jitter target](/quest/m0/audio-jitter-target/README.md) - requires this so its estimate measures the network, not DTX
- [DTX timestamps](/quest/m2/opus-dtx-timestamps.md) - the root fix that restores DTX
- [Opus backend](/quest/m2/audio-opus-backend.md) - measures DTX on the native side
