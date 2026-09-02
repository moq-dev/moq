# [M] Open-GOP gradual recovery trims through the recovery picture

## Goal

A viewer tuning in at an H.264 recovery point with `recovery_frame_cnt > 0`
never hands the decoder a picture before recovery completes, and a viewer
already playing keeps every frame. Today such a stream tunes in with the same
glitches as any open GOP, and the leading-picture rule cannot help because the
unsafe pictures sit at or after the keyframe timestamp.

## Plan

`rs/moq-mux/src/codec/h264/split.rs` treats every recovery-point SEI as a
keyframe and drops the count (`sei_has_recovery_point` returns a bool), so
nothing downstream can tell an immediately usable recovery point from a
gradual one, where recovery lands `recovery_frame_cnt` frames later in output
order.

- Retain the count from the SEI in the splitter and carry it on the group in
  the container framing, so a consumer knows how far past the keyframe the
  first safe picture is. That is a framing change, so it lands with a
  `draft-lcurley-moq-hang` update.
- On the first group after a non-continuous transition, the consumer skips
  every picture until the recovery picture, in JS and in the Rust decode path,
  using the same non-continuous signal the leading-picture rule keys on.
- Tests: a fixture with `recovery_frame_cnt = 2`, trimmed through the
  recovery picture on tune-in and untouched when continuous, in both
  languages; a zero-count stream stays on the leading-picture rule alone.

## Required

- [Open-GOP leading pictures](/quest/m0/open-gop-leading-pictures.md) - the non-continuous signal and first-group trimming this extends
