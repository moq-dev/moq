# [M] Native players stay live through refresh recovery with two decoders

## Goal

A moq-ffi (every binding) or moq-c player of a video rendition with `warmup`
set holds its latency target through a cold join or a skip, the same as the
browser [dual decoder](/quest/m2/intra-refresh/dual-decoder.md). A min-delay
decoder may show refresh stripes. A max-delay decoder with one more sweep of
budget keeps continuity, and its pictures replace the min-delay ones once it
catches up. A rendition without `warmup` uses one decoder exactly as today.

## Plan

Decided in the 2026-10-14 planning interview and its review:

- Scope: `moq_video::decode::Consumer` (`rs/moq-video/src/decode/consumer.rs`),
  whose only users are the players in `rs/moq-ffi/src/video.rs` and
  `rs/moq-c/src/video.rs`. It always runs the pair for a `warmup` rendition.
  There is no new public API and no knob, so no binding or doc/lib changes.
  `moq play` and the transcoder drive `decode::Sink` directly and are planned
  separately: [play dual decoder](/quest/m2/intra-refresh/play-dual-decoder.md)
  and [consumer warmup](/quest/m2/intra-refresh/consumer-warmup.md).
- Budgets: the min-delay decoder uses the caller's `Options::max_delay`, which
  both players already set from the app. The max-delay decoder uses
  `max_delay + warmup` and joins one group earlier. Each opens its own
  subscription with `Subscription::with_max_delay`; the origin dedups them
  into one upstream request. The max-delay decoder reads groups oldest first,
  so it never opens mid-sweep. If a newest-first cold join drops the earlier
  group, fix that at the source in `moq_mux::container::Consumer`.
- Switch rule. `read()` is pull-based with no presentation clock, so it cannot
  wait indefinitely to see whether the max-delay decoder will produce a
  timestamp. In steady state both decoders race on the same frames, so taking
  whichever is newest would flicker stripes between clean frames during
  catch-up. Instead the source is sticky:
  - emit min-delay frames until the max-delay decoder's newest decoded
    timestamp reaches the min-delay decoder's newest, then emit max-delay
    frames;
  - while max-delay is the source, hold a newer min-delay frame instead of
    dropping it. Emit it, and fall back to min-delay, when it leads the
    max-delay decoder by more than one frame interval, or has been held for
    one frame interval measured as `max(wall, pts)`, or the max-delay decoder
    ends or fails. A publisher pause therefore never hides the newest picture
    for longer than a frame;
  - returned timestamps strictly increase: a frame at or below the last one
    returned is dropped, so the cutover never repeats a picture.
- `Sink` is not cancellation-safe on non-Apple backends: a cancelled decode
  poisons it. Drive both decoders without ever cancelling a losing in-flight
  read; keep each pending operation until it completes.
- Tests beside the existing decode tests in `rs/moq-video/src/decode`, with
  a synthetic `warmup` catalog and track: a cold join emits min-delay frames
  and then max-delay ones once caught up; a stall within one sweep makes
  min-delay skip while max-delay stays continuous and takes over again
  without alternating; a stall beyond one sweep skips both; output timestamps
  strictly increase across every switch; exactly one newer min-delay frame
  followed by a publisher pause, by end of track, and by a max-delay decoder
  failure each emit that frame within the hold bound; the end-of-track tail
  drains from both decoders; a rendition without `warmup` opens one
  subscription and one decoder.
- Measure the cost of two decoders against one: CPU, decoder memory, and
  hardware decoder instances on at least one hardware backend, recorded in the
  PR.

## Related

- [Dual decoder](/quest/m2/intra-refresh/dual-decoder.md) - the browser version this mirrors
- [H.264 import](/quest/m2/intra-refresh/h264-import.md) - publishes the real `warmup` for an end-to-end check
