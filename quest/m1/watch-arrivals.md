# [S] Per-frame arrivals in @moq/watch

## Goal

The audio and video decoders in `@moq/watch` each expose an `arrivals`
signal: a bounded window of recent frames as `{timestamp, arrival, late,
skipped}`, where `arrival` is when the frame came off the transport, `late`
means it arrived after its sync deadline (not that it rendered late), and
`skipped` marks content the consumer dropped to stay within the latency
budget. A page can draw per-frame arrival against playout without patching
`Sync.received`.

## Plan

Decided 2026-10-09:

- **A signal, not a callback.** A signal holding only the newest arrival
  would coalesce and lose frames between reads, so it holds a window that
  updates per frame. It must reach back at least as far as the span the
  first consumer draws (a transcript word); time or count bound is chosen
  when implementing.
- **Feed it from where the data already exists.** `Sync.received()`
  computes lateness per frame and only logs it today; the container consumer
  knows which groups it skipped and only warns. Neither has a signal.
- **`arrival` is the ingress time.** `Container.Consumer` stamps each frame
  as `readFrame()` returns, but the `Frame` it returns drops that stamp, and
  `Sync.received()` samples the clock later, after reordering and any
  decoder backpressure. Carry the ingress stamp through to the decoder and
  compute lateness from it.
- **Skipped content never reaches the decoder.** A latency-skipped group's
  frames are not decoded, so they cannot appear as ordinary entries. Decide
  how the window represents the gap (a mark on the first frame after it, or
  a separate entry), keeping it distinct from other `continuous: false`
  causes such as markers.
- The first consumer is moq.dev's Voice AI demo, which draws each word's
  frames and marks the ones a WebRTC jitter buffer would have dropped
  ([moq.dev's condition quest](https://github.com/moq-dev/moq.dev/blob/main/quest/m1/watch-arrivals-release.md)).
- Test with recorded arrival traces through `js/watch/src/audio/replay.ts`,
  including a case where delivery is delayed but the recorded arrival is
  unchanged.

## Related

- [Browser stats](/quest/m1/stats/js.md) - its late-frame and skip counters share these sources, but count at the producer since a reader of an evicting window can miss entries
