# [XS] A numeric delay change keeps the audio decoder

## Goal

Changing `@moq/watch`'s numeric audio delay (for example 100 to 120 ms) moves
the playout target without resubscribing, rebuilding the decoder, or
truncating the ring, so a delay step is never an audible gap.

## Plan

`js/watch/src/audio/decoder.ts` reads `effect.get(this.sync.in.delay) ===
"instant"` in both `#runDecoder` and the worklet effect. The effect tracks the
whole signal, so every numeric change reruns both, and the rerun's handover
truncates the ring. `"auto"` never changes, so only numeric callers are hit.

Decided (2026-10-04): derive one `computed` for "is instant" and read it in
both places. Numeric changes already reach the ring through
`#runLatency`'s `setLatency`. It lands ahead of the jitter target because that
quest's preset testing steps the delay and would count these gaps as underruns.

Test: a decoder fed frames keeps its subscription and ring contents across a
numeric delay change, and still rebuilds on a switch to or from `"instant"`.

## Closes

- [#4777](https://github.com/moq-dev/moq/issues/4777) - close this issue when the quest finishes

## Related

- [Watch jitter target](/quest/m0/audio-jitter-target/watch.md) - edits the same decoder; its delay presets hit this bug
- [Audio graph lifetime](/quest/m1/watch-audio-graph.md) - the other needless audio teardown in the same file
