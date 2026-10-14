# [M] Native players stay live through refresh recovery with two decoders

## Goal

A Rust player (moq-ffi and every binding, moq-c, and `moq play`) of a video
rendition with `warmup` set holds its latency target through a cold join or a
skip, the same as the browser
[dual decoder](/quest/m2/intra-refresh/dual-decoder.md). A min-delay decoder
may show refresh stripes. A max-delay decoder with one more sweep of budget
keeps continuity, and its pictures replace the min-delay ones once it catches
up. The transcoder keeps the freeze from
[consumer warmup](/quest/m2/intra-refresh/consumer-warmup.md). A rendition
without `warmup` uses one decoder exactly as today.

## Plan

Decided in the 2026-10-14 planning interview:

- API: `moq_video::decode::Options` gains `recovery: Recovery`, an enum with
  `Freeze` (the default) and `Live`. `Live` runs two decoders when `warmup`
  makes the budgets differ. `Freeze` is today's single decoder until
  consumer-warmup lands the withhold rule behind it; document that on the
  variant. The players set `Live` internally: `rs/moq-ffi/src/video.rs`,
  `rs/moq-c/src/video.rs`, and `rs/moq-cli/src/play/media.rs`. It is not
  exposed through moq-ffi or `moq.h`, so no binding or doc/lib changes.
- Budgets: the min-delay decoder uses the caller's `Options::max_delay`, which
  every player already sets from the app. The max-delay decoder uses
  `max_delay + warmup` and joins one group earlier. Each opens its own
  subscription with `Subscription::with_max_delay`; the origin dedups them
  into one upstream request. The max-delay decoder reads groups oldest first
  (`Start` and `set_groups` in `rs/moq-video/src/decode/consumer.rs`), so it
  never opens mid-sweep. If a newest-first cold join drops the earlier group,
  fix that at the source in `moq_mux::container::Consumer`.
- Switch rule. `read()` is pull-based with no presentation clock, so it cannot
  wait to see whether the max-delay decoder will produce a timestamp. In
  steady state both decoders race on the same frames, so taking whichever is
  newest would flicker stripes between clean frames during catch-up. Instead
  the source is sticky:
  - emit the min-delay frames until the max-delay decoder's newest decoded
    timestamp reaches the min-delay decoder's newest, then emit only max-delay
    frames and drop min-delay output;
  - fall back to min-delay when it decodes a frame more than one frame interval
    newer than the max-delay decoder's newest;
  - never emit a timestamp below the last one returned.
- Tests beside the existing decode tests in `rs/moq-video/src/decode`, with
  a synthetic `warmup` catalog and track: a cold join emits min-delay frames
  and then only max-delay ones once caught up; a stall within one sweep makes
  min-delay skip while max-delay stays continuous and takes over again
  without alternating; a stall beyond one sweep skips both; output timestamps
  are strictly monotonic across every switch; `Freeze` and a rendition without
  `warmup` open one subscription and one decoder.
- Measure the cost of two decoders against one: CPU, decoder memory, and
  hardware decoder instances on at least one hardware backend, recorded in the
  PR.

## Related

- [Dual decoder](/quest/m2/intra-refresh/dual-decoder.md) - the browser version this mirrors
- [Consumer warmup](/quest/m2/intra-refresh/consumer-warmup.md) - fills in `Recovery::Freeze`
- [H.264 import](/quest/m2/intra-refresh/h264-import.md) - publishes the real `warmup` for an end-to-end check
