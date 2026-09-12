# [M] Capture worklet writes PCM into a SharedArrayBuffer

## Goal

The COOP/COEP capture path has no per-quantum structured clone of PCM.
The postMessage fallback still works. xruns do not increase.

## Plan

`js/publish/src/audio/capture-worklet.ts` `postMessage`s planar channels
every ~128 samples (~2.7 ms at 48 kHz) with no transfer. Watch already has
a lock-free SAB ring (`js/watch/src/audio/shared-ring-buffer.ts`).

Reuse (or share) the watch SAB writer in the capture worklet; main thread
reads. Keep postMessage fallback when COOP/COEP is off (same split as
watch).

Acceptance: browser publish, COOP on/off: audio-thread CPU, main-thread
allocations, capture-to-encode latency. SAB path has no per-quantum clone.

## Related

- [Capture frame buffers](/quest/m2/capture-frame-buffers.md) - native X11/GDI, not this
- [Watch audio copies](/quest/m2/js-hotpath/watch-audio.md) - the playback SAB
