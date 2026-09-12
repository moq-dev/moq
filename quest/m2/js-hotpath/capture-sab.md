# [M] Reduce capture worklet PCM message copies

## Goal

Reduce measured capture PCM messaging cost in cross-origin-isolated browsers
without increasing overruns, dropped samples, or capture-to-encode latency.
The postMessage fallback remains supported.

## Plan

`js/publish/src/audio/capture-worklet.ts` sends planar channels with
postMessage each render quantum without transferring the buffers. Measure
those copies and main-thread allocation before adding shared storage.

Use a bounded single-producer/single-consumer ring if measurements justify
it. Reuse the watch ring's proven mechanics where suitable, but do not couple
publish to the watch package or assume a playback timeline is a capture FIFO.
Keep any shared helper internal. Publish availability only after all planes
are written and never block the audio thread waiting for the main thread.

Use coalesced message notifications to wake the reader without cloning PCM;
prove that a write racing the reader becoming idle cannot lose its wakeup.
Specify bounded overflow handling with observable dropped-sample counts and
preserve timestamp discontinuities. Cover channel/sample-rate changes,
shutdown and restart, stale wakeups after context closure, reader stalls, and
unavailable cross-origin isolation.
Do not assume render quantum size is fixed; retain actual frame counts.

Measure browser audio-thread CPU, allocation, copied bytes, queue occupancy,
and capture-to-encode latency for isolated and fallback paths. Include a
stalled-reader case and verify output samples/counts so discarded work cannot
look like an optimization. Wire bounded correctness coverage into CI;
retain the current path if the shared ring does not yield a useful win.
No public API or wire change belongs to this quest.

## Required

- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - shared measurement and browser CI harness

## Related

- [Capture frame buffers](/quest/m2/capture-frame-buffers.md) - native capture storage
- [Watch audio copies](/quest/m2/js-hotpath/watch-audio.md) - playback ring mechanics
