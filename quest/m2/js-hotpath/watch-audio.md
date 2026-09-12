# [S] Copy decoded audio into the ring and idle the worklet when muted

## Goal

The SAB playback path allocates 0 PCM arrays per packet and copies at most
once (twice on wrap). A muted or paused watch does not run
`AudioWorkletProcessor.process`. Unmute stays under 50 ms.

## Plan

`#emit` in `js/watch/src/audio/decoder.ts` does `new Float32Array(frames)`
per channel, `AudioData.copyTo`, then a JS loop into the SAB. Two full PCM
copies plus allocs at packet rate.

`copyTo` into a wrap-aware SAB view. Keep the alloc path for the
postMessage transport (it transfers those buffers).

`#runWorklet` always builds `AudioContext` + worklet. Mute sets `enabled`
false (stops download) but the worklet stays connected. Disconnect the
worklet or suspend the context when `enabled` is false; keep the module
registered so unmute stays instant. The file already notes this.

Acceptance: browser stereo 48 kHz: allocation volume and copy bytes/s on
the SAB path. Playwright, tab audible vs muted 10 s:
`AudioWorkletProcessor.process` count ~0 when muted; unmute <50 ms.

## Related

- [Time stretch](/quest/m2/watch-audio-time-stretch.md) - WSOLA in the worklet, not ingest copies
- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - harness
