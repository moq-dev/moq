# [M] Bound decoded video waiting for presentation

## Goal

Delayed playback retains a bounded amount of decoded video independent of
the requested playback delay. Preserve public frame ownership and retention
contracts, discontinuity handling, and instant-mode time to first paint.

## Plan

The video decoder's async output callback can wait for presentation while
holding a VideoFrame, but WebCodecs does not await that callback. Measure
submitted chunks, decodeQueueSize, pending outputs, and retained frames
separately to reproduce accumulation before changing scheduling.

Keep encoded work upstream until it approaches the playhead and bound decode
submission as well as pending decoded output. Define mandatory chunk-count,
encoded-byte, and decoder-queue limits; stop pulling upstream when full and
resume on progress or cancellation rather than accumulating a second queue. Account for codec dependencies,
decoder reordering, and asynchronous output; decodeQueueSize alone is not the
number of live VideoFrames. State the resulting bound, including required
reorder/lookahead frames, rather than requiring exactly one decoded frame.

Preserve the retained out.frame signals exposed by the decoder and renderer.
Keep the first-frame/instant behavior and close internal frames exactly once
on presentation, replacement, failure, cancellation, and reset. Public clone
removal or ownership redesign is outside this quest. Do not turn delayed
playback into latest-frame dropping or deadlock a decoder awaiting more input.

Use real WebCodecs with representative codecs, 1080p30 delayed playback,
instant mode, and a delay sweep. Measure live frames, memory, decode queue,
presentation cadence, dropped frames, and time to first paint. Include
pause/resume, end of input, codec change, and discontinuity while output is
parked. Wire bounded correctness cases into browser CI and retain paired
performance results; skipping frames cannot count as a performance win.

Coordinate with #3056's reset/generation protection without absorbing its
separate timeline change or weakening the current discontinuity behavior.

## Required

- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - shared measurement and browser CI harness

## Related

- [#3056](/quest/m1/3056-watch-video-decoder-captures-the-rewind-generation-at.md) - discontinuity reset
