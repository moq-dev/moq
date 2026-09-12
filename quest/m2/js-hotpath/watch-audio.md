# [M] Reduce decoded audio copies into the playback ring

## Goal

Reduce measured PCM allocation and copying on SAB playback without changing
ring timing, channel behavior, or the postMessage fallback.

## Plan

The decoder copies each AudioData plane into a temporary Float32Array before
inserting it into the shared ring. Measure that cost separately from decode
and worklet processing at representative channel counts and sample rates.

Design a ring-owned write operation rather than exposing its storage.
Preserve trimming, gaps, late-packet handling, channel fill, and wrap logic.
Map each planar channel explicitly through planeIndex, source frameOffset,
frameCount, and the destination ring offset, including both wrap segments.
Use distinct left/right samples to detect channel swaps in the wrap test.
Publish the atomic write position only after all channel planes are complete;
failed writes must not expose partially initialized audio. Verify whether
AudioData.copyTo accepts the intended shared destination in supported browsers
before choosing direct copies or a reusable staging buffer.

Keep the transferring postMessage path working when cross-origin isolation
is unavailable. Cover wrap, partial capacity, discontinuities, missing
channels, and concurrent worklet reads with correctness tests in CI. Use
browser measurements for allocation volume, copied bytes, glitches, and
latency; retain the existing path if no useful improvement is measured.

Mute/pause already disconnects the gain node from the destination in
`audio/emitter.ts`. Additional suspension or graph teardown is not part of
this copy optimization; require a reproduced residual CPU problem before
planning that separately.

## Required

- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - shared measurement and browser CI harness

## Related

- [Time stretch](/quest/m2/watch-audio-time-stretch.md) - playback processing
