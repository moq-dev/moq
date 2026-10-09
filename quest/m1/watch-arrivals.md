# [S] Per-frame arrivals in @moq/watch

## Goal

The audio and video decoders in `@moq/watch` each expose an `arrivals`
signal: a bounded window of recent frames as `{timestamp, arrival, late,
skipped}`, where `arrival` is when the frame came off the transport, `late`
means it missed its playout time, and `skipped` means the consumer dropped
its group to stay within the latency budget. A page can draw per-frame
arrival against playout without patching `Sync.received`.

## Plan

Decided 2026-10-09:

- **A signal, not a callback.** A signal holding only the newest arrival
  would coalesce and lose frames between reads, so it holds a window (time-
  or count-bounded, chosen when implementing) that updates per frame.
- **Feed it from where the data already exists.** `Sync.received()`
  computes lateness per frame and only logs it today; the container consumer
  knows which groups it skipped and only warns. Neither has a signal.
- The first consumer is moq.dev's Voice AI demo, which draws each word's
  frames and marks the ones a WebRTC jitter buffer would have dropped
  ([moq.dev's condition quest](https://github.com/moq-dev/moq.dev/blob/main/quest/m1/watch-arrivals-release.md)).
- Test with recorded arrival traces through `js/watch/src/audio/replay.ts`.

## Related

- [Browser stats](/quest/m1/stats/js.md) - its late-frame and skip counters can be derived from this window
