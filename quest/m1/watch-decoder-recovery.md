# [M] Watch decoders recover from an error

## Goal

One malformed packet no longer ends audio or video for the rest of a
subscription. Today a WebCodecs error closes the decoder, both audio loops
`break` (added in #2415 to stop an `InvalidStateError` loop), and video calls
`effect.close()`; nothing rebuilds the decoder, and playback silently stops.

## Plan

Decided: rebuild inside the loop, for audio and video alike. On a decoder
error, rebuild through one shared `build()` helper, skip to the next keyframe,
and give up after three rebuilds in a row with no output. A rebuilt legacy
audio decoder re-runs its warmup priming. Cover the legacy and CMAF loops.

Tests: a corrupt packet mid-stream is followed by decoded output from the next
keyframe; a stream of only corrupt packets gives up after three rebuilds
instead of spinning.

## Closes

- [#4324](https://github.com/moq-dev/moq/issues/4324) - close this issue when the quest finishes

## Related

- [Audio warmup](/quest/m1/audio-warmup.md) - the same audio loops and priming
