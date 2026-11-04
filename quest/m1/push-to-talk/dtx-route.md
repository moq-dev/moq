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

Measured 2026-11-04 (repro in the Chromium bug tracked by
[Chromium's Opus encoder stamps chunks from their
input](/quest/m2/chromium-opus-timestamps.md)): with
`usedtx: true`, Chrome 156 (Linux and macOS) ends 14 s of input at 8.92 s, and it
also ignores a jump in input timestamps. Firefox 157 keeps the timeline under
DTX but ignores input jumps. Safari 26.6.2 follows input jumps but never
applies `usedtx`. WebCodecs §5.6 already requires the input's timestamp, so
this is a Chromium bug, not a spec gap; the maintainer files it.

Decided 2026-11-04 (maintainer): measure before choosing. The routes:

1. **Gate before the encoder, stamp from our own inputs ✅ as the workaround.**
   The publisher decides silence itself (energy threshold or VAD) and neither
   encodes nor sends silent frames. Inputs are already cut to one codec frame
   each (`js/publish/src/audio/encoder.ts`, "Align those inputs to codec
   frames"), and without DTX every browser emits one chunk per input in order,
   so each chunk is stamped from a queue of the input timestamps fed, not from
   `chunk.timestamp`. That sidesteps both Chromium bugs with no encoder
   restart (today's code restarts the encoder on any input gap to re-base,
   which also works but costs a re-prime per utterance). Works in every
   browser.
2. **WASM libopus.** Ship an Opus encoder with real DTX and exact timestamps.
   Overlaps the m2 [Opus backend](/quest/m2/audio-opus-backend.md) quest.
3. **Native WebCodecs DTX** where it keeps the timeline: only Firefox today,
   so at best a later optimization once Chromium is fixed, with route 1 or 2
   as the fallback.

Measure each on Chromium, Firefox and Safari (kixtop): timeline drift over a
tone/silence/noise pattern, bytes sent during silence, onset latency of the
first word after silence (no clipped head), CPU, bundle size, and a listening
check. Record the numbers in the PR and put the pick to the maintainer.

## Related

- [Push-to-talk uses DTX](/quest/m1/push-to-talk/dtx.md) - implements the route this picks
- [Opus backend](/quest/m2/audio-opus-backend.md) - the native side's Opus and DTX measurements
