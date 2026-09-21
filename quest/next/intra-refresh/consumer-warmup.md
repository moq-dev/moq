# [M] Viewers honour warmup: join earlier, withhold display until recovery

## Goal

A viewer of a rendition with `warmup` set never displays a picture that its
decoder has not fully recovered, and never pays for that with extra startup
delay: it subscribes `warmup` further back than its latency target, decodes
the extra frames as fast as they arrive, and presents from the first recovered
frame. A viewer already playing that skips a group, whether by a latency skip
or because the relay shed it, freezes on its last good frame for one cycle
instead of showing corrupt stripes. A group whose first frame is a true IDR
shows at once. This holds in `js/watch`, in the Rust decode path that native
playback and the transcoder share, and therefore in every binding. Renditions
without `warmup` behave exactly as today.

## Plan

This absorbs the old gradual-recovery quest, which planned to carry
`recovery_frame_cnt` on the wire and count `frame_num`. The catalog duration
replaces both: the rule is timestamp arithmetic on the group start.

- Withhold rule, keyed on the same non-continuous signal in both consumers:
  `js/hang/src/container/consumer.ts` `next()` reports `continuous: false`
  after a subscribe, a declared discontinuity, or any skip (`#gap`). The Rust
  `rs/moq-mux/src/container` `Consumer` has no such signal: `read()` returns a
  bare frame and `discontinuity()` covers only empty groups and rewinds, so
  this quest adds a per-delivery continuity flag there, set on a sequence gap
  and on a latency skip, and `rs/moq-video/src/decode/consumer.rs` propagates
  it. For the first
  group after that signal, every frame is decoded (the decoder needs them to
  build reference state) and frames stamped below `group.start + warmup` are
  not presented; frames stamped at or above that boundary are, so the recovery
  picture itself is never withheld. `js/watch/src/video/decoder.ts` and
  `rs/moq-video/src/decode` are where presentation happens; the transcoder
  gets the skip for free because it consumes the same decode path. Keep the
  last painted frame on screen, as `#onDiscontinuity` already does.
- IDR exception: before withholding, check the first slice NAL type of the
  group's first frame (H.264 type 5; H.265 `IDR_W_RADL`, `IDR_N_LP`, the BLA
  types). Rust has the NAL walkers in `rs/moq-mux/src/codec/{h264,h265}`;
  JS needs a small length-prefixed walker beside the codec description parsing
  in `js/hang`. Other codecs never set `warmup`, so no check is needed there.
- Join earlier: the subscription's maximum age becomes the latency target plus
  `warmup`, so the group start lands `warmup` before the target and the first
  presented frame is on time. JS sets `Subscription.latencyMax` in `js/net`;
  Rust sets `latency_max` on the decode consumer's subscription
  (`rs/moq-video/src/decode/consumer.rs`), not `Subscription::group_start`,
  which is aggregated across subscribers and rewinds the track for everyone.
- Latency skipping must not shed the warmup span it deliberately joined:
  `#checkLatency` in the JS container consumer and `with_latency` in Rust
  compare the buffered span against the target, and frames still inside a
  withheld warmup count as decode-only, not buffered.
- Tests in both languages: a synthetic three-group track with `warmup` where a
  cold join presents nothing before start plus `warmup` and everything after;
  a continuous viewer presents every frame; a viewer that latency-skips into a
  later group freezes on the last presented frame through that group's warmup;
  a group opening on an IDR presents immediately; a rendition without
  `warmup` is untouched.

## Required

- [Catalog warmup](/quest/next/intra-refresh/catalog-warmup.md) - the field this reads

## Related

- [Open-GOP leading pictures](/quest/next/open-gop-leading-pictures.md) - trims frames stamped before the keyframe; this trims frames after the start, on the same signal
