# [M] Watch video waits before decode, not after

## Goal

`@moq/watch` keeps its video lookahead encoded, as audio already does. Only
frames within the decoder's latency of presentation are decoded, so a long
`buffer` holds encoded chunks instead of one decoded `VideoFrame` per frame.

## Plan

Today the video decoder's output callback (`js/watch/src/video/decoder.ts`)
awaits `sync.wait` before presenting and closing each frame,
so every buffered frame is held decoded. `Sync.received` caps this at
delay + buffer, so it only bites with a large `buffer`, where it can starve
the hardware decoder's frame pool (2026-10-07 audit).

Decided (2026-10-07): wait before decode, like the audio `ring.wait`. Feed a
chunk to the decoder only once its presentation time is within the decoder's
measured output latency.

Decided 2026-10-08: the watch worker lands first. It moves decode and `Sync`
into a worker and rewrites the same `sync.wait`, so this builds on its shape.

Public API: none. Wire: none.

## Required

- [Watch worker](/quest/m1/watch-worker.md) - moves decode and `Sync` into a worker

## Related

- [A/V clock](/quest/m1/av-clock.md) - changes what `Sync` waits on
