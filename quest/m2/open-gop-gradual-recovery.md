# [M] Open-GOP gradual recovery withholds display until the recovery picture

## Goal

A viewer tuning in at an H.264 recovery point with `recovery_frame_cnt > 0`
never displays a picture before recovery completes, and a viewer already
playing shows every frame. Today such a stream tunes in with the same
glitches as any open GOP, and the leading-picture rule cannot help because the
unsafe pictures sit at or after the keyframe timestamp. Recovery points with
`broken_link_flag` set are out of scope: there even a continuous viewer must
withhold display (H.264 D.2.8), which is a different rule with its own plan.

## Plan

`rs/moq-mux/src/codec/h264/split.rs` treats every recovery-point SEI as a
keyframe and drops the count (`sei_has_recovery_point` returns a bool), so
nothing downstream can tell an immediately usable recovery point from a
gradual one, where recovery lands `recovery_frame_cnt` frames later in output
order.

- Retain the count from the SEI in the splitter and carry it on the group in
  the container framing, so a consumer knows recovery is gradual and how far
  it runs. That is a framing change, so it lands with a
  `draft-lcurley-moq-hang` update.
- Gradual refresh is different from leading pictures: the decoder must
  consume every picture from the recovery-point access unit onward, because
  those pictures build the reference state the recovery picture needs
  (H.264 D.2.8, RFC 6184 section 8.5.2). So on the first group after a
  non-continuous transition the consumer decodes everything and withholds
  *display* until the recovery picture, in JS and in the Rust decode path,
  keyed on the same non-continuous signal the leading-picture rule uses.
- Identify the recovery picture the way the spec does: `recovery_frame_cnt`
  counts in `frame_num` units from the recovery point, in output order, not
  in container frames, so the consumer tracks `frame_num` progression (or the
  decoder's output order) rather than counting samples.
- Tests: a fixture with `recovery_frame_cnt = 2` whose pictures before the
  recovery picture are decoded but not presented on tune-in and all presented
  when continuous, in both languages; a zero-count stream stays on the
  leading-picture rule alone.

## Required

- [Open-GOP leading pictures](/quest/m2/open-gop-leading-pictures.md) - the non-continuous signal and first-group trimming this extends
