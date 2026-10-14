# [M] Transcoder honours warmup: join earlier, withhold output until recovery

## Goal

The transcoder, given a source rendition with `warmup` set, never encodes a
picture its decoder has not fully recovered, and never pays for that with
extra startup delay: it subscribes `warmup` further back than its latency
target, decodes the extra frames as fast as they arrive, and forwards from the
first recovered frame. A transcoder already running that skips a group,
whether by a latency skip or because the relay shed it, holds its last good
frame for one cycle instead of encoding corrupt stripes. A group whose first
frame is a true IDR forwards at once. Players stay live with stripes instead,
via the browser [dual decoder](/quest/m2/intra-refresh/dual-decoder.md) and
the [native dual decoder](/quest/m2/intra-refresh/native-dual-decoder.md).
Renditions without `warmup` behave exactly as today.

## Plan

This absorbs the old gradual-recovery quest, which planned to carry
`recovery_frame_cnt` on the wire and count `frame_num`. The catalog duration
replaces both: the rule is timestamp arithmetic on the group start.

The 2026-10-14 planning interview moved every player to two decoders, so this
quest is the transcoder only. The transcoder drives `decode::Sink` directly
in `rs/moq-transcode/src/feed.rs` and the rung pipeline in
`rs/moq-transcode/src/rung.rs`, not `decode::Consumer`, so the rule lives in
those loops.

- Withhold rule, keyed on the first frame a loop reads and on the gap item the
  container read yields after any skip (a join yields no gap): `Read::Gap`
  from the `moq_mux::container::Consumer` it reads. A declared end is not a
  gap and keeps everything. For the first group read and the first group
  after a gap, every frame is decoded (the decoder needs them to build
  reference state) and frames stamped below `group.start + warmup` are not
  forwarded; frames stamped at or above that boundary are, so the recovery
  picture itself is never withheld. Share the rule between the feed and rung
  loops rather than writing it twice.
- IDR exception: before withholding, check the first slice NAL type of the
  group's first frame (H.264 type 5; H.265 `IDR_W_RADL`, `IDR_N_LP`, the BLA
  types), with the NAL walkers in `rs/moq-mux/src/codec/{h264,h265}`. Other
  codecs never set `warmup`, so no check is needed there.
- Join earlier: the subscription's maximum delay becomes the latency target plus
  `warmup`, so the group start lands `warmup` before the target and the first
  forwarded frame is on time. Set it through `Subscription::with_max_delay`,
  not `Subscription::start`, which is aggregated across subscribers and
  rewinds the track for everyone.
- Max-delay skipping must not shed the warmup span it deliberately joined:
  the max delay budget (`Consumer::poll_read`, set by `set_max_delay`)
  compares the buffered span against the target, and frames still inside a
  withheld warmup count as decode-only, not buffered.
- Regression tests in `rs/moq-transcode` over a synthetic three-group track
  with `warmup`: a cold join forwards nothing before start plus `warmup` and
  everything after; a continuous feed forwards every frame; a skip into a
  later group holds the last forwarded frame through that group's warmup; a
  group opening on an IDR forwards immediately; a rendition without `warmup`
  is untouched.

## Required

- [Plan: recovery boundaries](/quest/m2/intra-refresh/plan-boundary.md) - consumers need a safe boundary before presenting the affected pictures

## Related

- [Open-GOP leading pictures](/quest/m1/open-gop-leading-pictures.md) - trims frames stamped before the keyframe where this trims frames after the start
- [Native dual decoder](/quest/m2/intra-refresh/native-dual-decoder.md) - the player path, which shows stripes instead of freezing
