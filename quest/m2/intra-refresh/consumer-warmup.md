# [M] Rust viewers honour warmup: join earlier, withhold display until recovery

## Goal

A Rust viewer of a rendition with `warmup` set never displays a picture that
its decoder has not fully recovered, and never pays for that with extra
startup delay: it subscribes `warmup` further back than its latency target,
decodes the extra frames as fast as they arrive, and presents from the first
recovered frame. A viewer already playing that skips a group, whether by a
latency skip or because the relay shed it, freezes on its last good frame for
one cycle instead of showing corrupt stripes. A group whose first frame is a
true IDR shows at once. This holds in the Rust decode path that native
playback and the transcoder share, and therefore in every binding. Browsers
show live stripes instead, per
[dual decoder](/quest/m2/intra-refresh/dual-decoder.md). Renditions without
`warmup` behave exactly as today.

## Plan

This absorbs the old gradual-recovery quest, which planned to carry
`recovery_frame_cnt` on the wire and count `frame_num`. The catalog duration
replaces both: the rule is timestamp arithmetic on the group start.

The 2026-10-14 planning interview moved js/watch to two decoders, so this
quest is Rust-only. The transcoder never wants stripes, and native playback
keeps the freeze until a Rust two-decoder path is planned.

- Withhold rule, keyed on the first frame a reader reads and on the gap item
  the container read yields after any skip (a join yields no gap): `Read::Gap`
  from the `moq_mux::container::Consumer`, which
  `rs/moq-video/src/decode/consumer.rs` reads directly. A declared end is not
  a gap and keeps everything. For the first group read and the first group
  after a gap, every frame is decoded (the decoder needs them to build
  reference state) and frames stamped below `group.start + warmup` are not
  presented; frames stamped at or above that boundary are, so the recovery
  picture itself is never withheld. `rs/moq-video/src/decode` is where
  presentation happens; the transcoder gets the skip for free because it
  consumes the same decode path.
- IDR exception: before withholding, check the first slice NAL type of the
  group's first frame (H.264 type 5; H.265 `IDR_W_RADL`, `IDR_N_LP`, the BLA
  types), with the NAL walkers in `rs/moq-mux/src/codec/{h264,h265}`. Other
  codecs never set `warmup`, so no check is needed there.
- Join earlier: the subscription's maximum delay becomes the latency target plus
  `warmup`, so the group start lands `warmup` before the target and the first
  presented frame is on time. Add it to the decode consumer's
  `Options::max_delay`, which reaches the subscription through
  `Subscription::with_max_delay` (`rs/moq-video/src/decode/consumer.rs`), not
  `Subscription::start`, which is aggregated across subscribers and rewinds
  the track for everyone.
- Max-delay skipping must not shed the warmup span it deliberately joined:
  the max delay budget (`Consumer::poll_read`, set by `set_max_delay`)
  compares the buffered span against the target, and frames still inside a
  withheld warmup count as decode-only, not buffered.
- Tests: a synthetic three-group track with `warmup` where a cold join
  presents nothing before start plus `warmup` and everything after; a
  continuous viewer presents every frame; a viewer that latency-skips into a
  later group freezes on the last presented frame through that group's
  warmup; a group opening on an IDR presents immediately; a rendition without
  `warmup` is untouched.

## Required

- [Plan: recovery boundaries](/quest/m2/intra-refresh/plan-boundary.md) - consumers need a safe boundary before presenting the affected pictures

## Related

- [Open-GOP leading pictures](/quest/m1/open-gop-leading-pictures.md) - trims frames stamped before the keyframe where this trims frames after the start
- [Dual decoder](/quest/m2/intra-refresh/dual-decoder.md) - the browser path, which shows stripes instead of freezing
