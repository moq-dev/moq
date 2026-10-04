# [XS] Voice audio publishes without Opus DTX

## Goal

`@moq/publish` voice sources no longer enable Opus DTX by default, so the
published audio timeline tracks the capture clock through silence and the
advertised jitter stays at its measured value instead of climbing to seconds.

## Plan

Chromium stamps encoder output as the first input timestamp plus the samples
it emitted, so with DTX every suppressed frame pulls later audio earlier.
`js/publish/src/audio/encoder.ts` publishes chunks at the encoder's timestamp
and only re-bases on an input gap, never on output suppression.

Decided (2026-10-04): delete `usedtx: true` from the voice defaults
(`opusKindDefaults`). The quest is a prerequisite of the audio jitter target,
whose acceptance run with a real microphone would otherwise measure DTX drift
as network jitter.

Open for the PR: an explicit `usedtx: true` has the same drift. Either re-base
output timestamps on the input clock or remove the option; ask the maintainer.

Test: a voice encoder with silence between utterances publishes frames whose
timestamps follow the input timeline.

## Closes

- [#4783](https://github.com/moq-dev/moq/issues/4783) - close this issue when the quest finishes

## Related

- [Audio jitter target](/quest/m0/audio-jitter-target/README.md) - requires this so its estimate measures the network, not DTX
- [Opus backend](/quest/m2/audio-opus-backend.md) - measures DTX on the native side
