# [L] Transcode rungs cold-start without a decoded-frame burst or a blind bounded window

## Goal

A cold `moq-transcode` rung serves a bounded `Start::Time` subscription under a `Sessions` budget whenever the source publishes inside that window, and a cold start's backlog costs device memory bounded by what the rungs actually encode, not a fixed decoded-frame queue per source.

## Plan

Accepted as trade-offs in [moq-dev/moq#9706](https://github.com/moq-dev/moq/pull/9706) (decided 2026-11-26, maintainer): both are the choices that quest's Plan made, and fixing either is a redesign beyond it.

1. **Bounded time window on a cold rung (`rs/moq-transcode/src/rung.rs`, `reaches_live`):** before the feed has seen a source group, only an open range or a `Start::Live` join counts as reaching the live edge. That keeps a paused source from holding the only session in a live encode its own fills then wait behind. The cost: under a budget, a cold rung whose only request is a bounded `Start::Time` subscription never starts the live path, so it gets nothing even when the source publishes inside the window. Learn the source's newest group without reserving an encoder (a source subscription or track query that opens no decoder or encoder session), then judge the range against that edge.
2. **Decoded cold-start backlog (`rs/moq-transcode/src/feed.rs`, `CAPACITY`):** the shared feed queues up to 128 decoded frames per listener so a cold start survives the cached-GOP burst. On NVDEC that grows the CUDA frame pool to about 1.5 GiB per 4K source, and `rs/moq-video/src/frame/pool.rs` keeps dequeued buffers allocated afterwards. Keep the backlog as compressed source groups instead, and pace decode by the slowest attached rung's encoder progress, so the decoded queue stays a few frames deep. A genuinely slow rung still skips to the next group rather than stalling the others.

Verify each with a mock-time test: a bounded window the source publishes into after a cold start, under a 1-session budget; and a 60-frame cached GOP whose decoded frames in flight stay bounded while a slow rung opens its encoder. Measure the pool's peak on a 4K NVDEC source before and after (`just rs gpu`).

Public API / wire: none expected.

## Related

- [Transcode live on subscribers](/quest/m4/transcode-live-on-subscribers.md) - the same `reaches_live` gate, applied without a budget too
