# [M] Watch video waits before decode, not after

## Goal

`@moq/watch` keeps its video lookahead encoded, as audio already does. Only
frames within the decoder's latency of presentation are decoded, so a long
`buffer` holds encoded chunks instead of one decoded `VideoFrame` per frame.

## Plan

Today the video decoder's output callback (`js/watch/src/video/decoder.ts`,
around line 380) awaits `sync.wait` before presenting and closing each frame,
so every buffered frame is held decoded. `Sync.received` caps this at
delay + buffer, so it only bites with a large `buffer`, where it can starve
the hardware decoder's frame pool (2026-10-07 audit).

Decided (2026-10-07): wait before decode, like the audio `ring.wait`. Feed a
chunk to the decoder only once its presentation time is within the decoder's
measured output latency. Re-check against the watch worker plan, which moves
`Sync` off the main thread.

Public API: none. Wire: none.

## Related

- [Watch worker](/quest/m1/watch-worker.md) - moves decode and `Sync` into a worker
- [A/V clock](/quest/m1/av-clock.md) - changes what `Sync` waits on
