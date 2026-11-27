# [S] The MediaCodec keyframe test gets its keyframe on a loaded emulator

## Goal

`encode::backend::mediacodec::tests::encodes_a_keyframe_with_parameter_sets_inline` passes on
every run of Nightly's `android / Android emulator` (`just rs android-test`), fixed at its cause.

## Plan

Seen once in 23 Nightly runs from 11-15 to 11-27, at `180304252a`
([45961851844](https://github.com/moq-dev/moq/actions/runs/45961851844)):

```
a keyframe carrying its own parameter sets: H264(NotInitialized)
```

So the encoder returned no SPS at all, most likely no output. The likely cause, not yet
confirmed: the test feeds one frame to a freshly started codec. `submit` waits at most
`INPUT_TIMEOUT` (10 ms) for an input buffer and drops the frame when none frees up. `flush`
then finds nothing pending and returns, so nothing comes out. A cold software encoder on a
loaded emulator can take longer than 10 ms to hand out its first input buffer.

1. Confirm the drop: run the test in a loop on an emulator under CPU load, with `RUST_LOG` at
   debug for `no input buffer available, dropping a frame`.
2. If it is the drop, the encoder is right to drop a frame it has no buffer for (a live source
   does not wait), and it already keeps a requested keyframe pending across a dropped frame.
   Recommended: the test feeds frames at the configured rate, as a live source does, and asserts
   that the first output is a keyframe carrying its parameter sets. That is the behavior under
   test, not a retry. If a dropped first frame turns out to cost real streams their first group,
   say so in the PR instead.

Public API: none. Wire: none.
