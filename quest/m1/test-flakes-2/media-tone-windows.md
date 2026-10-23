# [S] Media playback keeps its tone through the existing grading windows

## Goal

The browser media interop test passes its existing audio-tone agreement checks
under build load, fixed at the cause of the missing tone samples. Keep its
window lengths, agreement threshold, and negative controls unchanged.

## Plan

Recurred on 2026-10-22 while investigating resource instrumentation at
[fd59eb18a5](https://github.com/moq-dev/moq/commit/fd59eb18a5), using Chromium
156.0.8078.4 through `nix develop --command just test media`:

- An unconstrained full run failed the positive lifecycle's coded-size-growth
  window with 69 matching audio-tone samples out of 80. All five negative
  controls passed, including the leaked-session and lagging-latecomer controls.
- A full run restricted to two available CPUs passed the positive lifecycle
  and the leaked-session control, then the lagging-latecomer control failed
  earlier than intended: its cold-start audio had 70 matching samples out of
  82. The host diagnostic recorded 0.1% to 1.4% idle CPU in the failed window;
  the player recorded 19 long animation frames, the longest 576 ms, while
  the fixture recorded none. These observations do not identify a cause.

[moq-dev/moq#6302](https://github.com/moq-dev/moq/pull/6302) recorded the earlier
cold-start recurrence and added the media-window diagnostics. Use those
diagnostics to distinguish capture, delivery, decoding, playout, and sampling;
reproduce a causal transition and test it with controlled time where possible.
Do not infer a production ring or estimator bug from low tone agreement alone.

Existing work has different proof targets: the
[manual playout target proof](/quest/m1/audio-jitter-target/watch.md) requires
real microphone measurements on Chrome and Safari, while
[moq-dev/moq#6425](https://github.com/moq-dev/moq/pull/6425) fixes buffered audio
refill. Neither has been shown to own this media-window recurrence. Check them
before changing shared playback code, and reuse a demonstrated fix.

Public API: none expected. Wire: none expected.

## Related

- [Media resource instrumentation](/quest/m1/test-flakes-2/resource-instrumentation.md) - five resource checks passed during these runs; the tone failures are separate
