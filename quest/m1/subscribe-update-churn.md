# [S] A viewer's subscription updates stop chattering

## Goal

A `js/watch` viewer sends a SUBSCRIBE_UPDATE only when its subscription
meaningfully changes, not several times a second per track. In a reported
`moq.dev/publish` session, the publisher logged an endless alternating
`subscribe update` for audio and video.

## Plan

Cause, from reading the code on 2026-10-20: `js/watch/src/media.ts` pushes
`maxDelay` into `subscriber.update` on every change, and the subscriber loop
(`js/net/src/lite/subscriber.ts`, `#sameSubscription`) compares it exactly.
Video's `maxDelay` follows `sync.out.maxDelay`, which moves with the per-frame
arrival spread (20 ms steps, at most every 500 ms) and every catalog estimate
republish. Audio only joins once `maxDelay` passes `AUTO_MAX_DELAY`.

Settled: measure first with the bounded publisher encode queue. Count updates
per track for a live camera publish and `bbb.hang`, then pick a cut with the numbers, and
close this as unneeded if they are already quiet. Options weighed when
planning:

- Round the subscription's `maxDelay` up to the next power of two ms in
  `js/watch`, so it changes only when the target crosses a doubling.
- Rate-limit sends to one per smoothed RTT per track in `js/net` and
  `rs/moq-net`, latest value wins.

Coalescing to one update "in flight" was ruled out: moq-lite's
SUBSCRIBE_UPDATE has no acknowledgement, and the sender already sends only the
latest value whenever the stream applies backpressure.
