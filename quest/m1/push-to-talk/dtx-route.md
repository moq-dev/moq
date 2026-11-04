# [S] Pick how the browser publisher suppresses silence

## Goal

A measured comparison picks the route `@moq/publish` uses to stop sending
silent Opus frames while keeping every chunk on the capture timeline, and the
maintainer approves the pick in the PR. Done when the PR records each route's
numbers and the chosen route with ✅.

## Plan

Planned 2026-11-04 with the maintainer, from the
[moq-dev/moq#7760](https://github.com/moq-dev/moq/pull/7760) latency
breakdown. Push-to-talk moves from the catalog toggle to DTX (see
[Push-to-talk uses DTX](/quest/m1/push-to-talk/dtx.md)), so the publisher needs
a silence suppression that works in every browser.

Why WebCodecs DTX alone can't be used today: with `usedtx: true`, Chromium
stamps each Opus chunk as the first input timestamp plus the samples it
emitted, so every suppressed frame pulls later audio earlier (14 s of input
ended at 8.92 s), and no WebCodecs output maps a chunk back to its input
([moq-dev/moq#4783](https://github.com/moq-dev/moq/issues/4783),
[moq-dev/moq#4902](https://github.com/moq-dev/moq/pull/4902)). Firefox kept
the timeline; Safari wasn't measured. `usedtx` was removed in
[moq-dev/moq#4919](https://github.com/moq-dev/moq/pull/4919).

Decided 2026-11-04 (maintainer): measure before choosing. The routes:

1. **Gate before the encoder.** The publisher decides silence itself (energy
   threshold or VAD) and neither encodes nor sends silent frames, so it keeps
   stamping from the input timestamps it already controls. Works with any
   encoder.
2. **WASM libopus.** Ship an Opus encoder with real DTX and exact timestamps.
   Overlaps the m2 [Opus backend](/quest/m2/audio-opus-backend.md) quest.
3. **Native WebCodecs DTX** in the browsers that keep the timeline, if the
   WebCodecs DTX repro (see the epic) shows any do, with route 1 or 2 as the
   fallback elsewhere.

Measure each on Chromium, Firefox and Safari (kixtop): timeline drift over a
tone/silence/noise pattern, bytes sent during silence, onset latency of the
first word after silence (no clipped head), CPU, bundle size, and a listening
check. Record the numbers in the PR and put the pick to the maintainer.

## Related

- [Push-to-talk uses DTX](/quest/m1/push-to-talk/dtx.md) - implements the route this picks
- [Opus backend](/quest/m2/audio-opus-backend.md) - the native side's Opus and DTX measurements
