# [S] Browser capture delivers audio frames on a steady cadence

## Goal

`@moq/publish`'s capture graph hands encoded audio frames to the network at
the frame rate, not in bursts, so the listener's `"auto"` jitter target
measures the network instead of the capture graph. On an ideal path the
per-press arrival spread drops well below the ~46 ms measured on
[moq-dev/moq#7760](https://github.com/moq-dev/moq/pull/7760); the PR records
before and after on Chromium and on kixtop.

## Plan

Planned 2026-11-04 with the maintainer. Measured on #7760: the capture graph
delivered 60 ms frames 25-109 ms apart, and capture plus framing took 29 ms
median, 110 ms max. In `"auto"` that spread became about 46 ms of measured
jitter per press, padding the target.

Decided 2026-11-04 (maintainer): reduce it, not just report it. Start by
finding where the bursts form: the AudioWorklet's posting cadence, the capture
AudioContext's callback size (already `latencyHint: "interactive"` in
`js/publish/src/audio/capture.ts`), or batching between the worklet and the
encoder. Fix at the source; don't re-pace on a timer.

## Related

- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the estimator that reads this spread
