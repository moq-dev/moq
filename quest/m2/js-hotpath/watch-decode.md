# [M] Bound watch video decode and stop cloning presented frames

## Goal

Delayed playback holds O(1) decoded `VideoFrame`s, not O(delay × fps).
Instant mode does not regress time-to-first-paint. The present path keeps
at most one live frame plus at most one for TTV.

## Plan

`js/watch/src/video/decoder.ts`: the WebCodecs output callback is `async`
and `await`s `sync.wait(timestamp)` while the original `VideoFrame` is
still open (`close()` is in `finally`). The decoder does not wait on that
promise. The submit loop calls `decoder.decode(chunk)` with no
`decodeQueueSize` cap. Audio already parks encoded frames via `ring.wait()`
before `AudioDecoder.decode`.

Per presented frame the output callback clones (sometimes twice: first
frame plus after wait), `Decoder.#runActive` clones again, and the renderer
clones again for `out.frame`.

Keep encoded chunks until the playhead is near (same backpressure as
audio). Output callback should clone-or-drop the latest frame and return
synchronously. Transfer ownership down the chain; the renderer draws then
closes. Optionally pause submit when `decodeQueueSize` is above a small
bound. Keep the first-frame clone for TTV.

Acceptance: real-browser WebCodecs (Playwright / `just test` driver):
1080p30, `delay=2s` and `instant`. Record `decodeQueueSize`, live
`VideoFrame` count, heap, dropped frames, TTF. Delayed mode holds O(1)
decoded frames. Instant mode TTF does not regress.

## Related

- [#3056](/quest/m1/3056-watch-video-decoder-captures-the-rewind-generation-at.md) - rewind/reset, not this queue
- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - harness
