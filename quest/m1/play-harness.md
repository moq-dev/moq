# [M] moq play's task logic runs in CI without a device

## Goal

`moq play`'s media tasks (`rs/moq-cli/src/play/media.rs`: tune-in, rendition
switches, drains, and the playout queue) run in CI tests on every PR, without
a speaker, window, or display. Today the module sits behind the `play`
feature, which CI only compiles in the nightly clippy run, so its regressions
(#3946's tune-in burst, #3966's rendition-switch gap) are measured by hand.

## Plan

- Separate the task logic from the device: the tasks talk to a sink and a
  wake handle the test replaces with a fake that records what was played and
  when, on a paused tokio clock. Keep the seam private to `moq-cli`; no
  test-only hooks in production paths beyond it.
- Run `cargo test -p moq-cli --features play` in the per-PR Test job, headless.
- Land the rendition-switch gap test on it, failing without #3966's fix.
  The tune-in burst test moves onto the harness with
  [Play tune-in backpressure](/quest/m1/play-tunein-backpressure.md), which
  owns that fix.

## Required

- The rendition-switch gap fix (#3966) has merged
