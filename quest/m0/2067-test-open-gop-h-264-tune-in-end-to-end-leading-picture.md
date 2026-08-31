# [M] Test open-GOP H.264 tune-in end to end (leading-picture handling)

## Goal

Implement and verify the behavior tracked in [#2067](https://github.com/moq-dev/moq/issues/2067)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Context

[#2066](https://github.com/moq-dev/moq/pull/2066) fixes [#2050](https://github.com/moq-dev/moq/issues/2050): the H.264 splitter now recognizes recovery-point SEI random access, so open-GOP broadcast H.264 (non-IDR I-slice keyframes) resolves a video rendition and round-trips. That fix is about **catalog/rendition resolution and group boundaries**  -  it makes the group start at the recovery point.

What it does **not** do is validate or smooth out playback behavior at a cold tune-in into an open GOP. This issue tracks that testing (and any follow-up work it surfaces).

#### What needs testing

Two distinct playback situations:

1. **Continuous playback across a recovery point.** All reference frames are already decoded, so this should be clean. Confirm.
2. **Cold tune-in at a recovery point** (fresh subscriber, first group received). This splits further:
   - **Clean recovery point** (I-frame, `recovery_frame_cnt = 0`, no leading pictures  -  common for broadcast contribution). Expected to decode cleanly through WebCodecs.
   - **True open GOP with leading pictures** (B-frames after the I in decode order that reference the *previous* GOP). The I-frame decodes, but those leading B-frames have missing references at tune-in. WebCodecs marks them `"delta"` and behavior is decoder-dependent (Chrome software path typically drops/garbles for a fraction of a GOP; a stricter hardware decoder could error). Neither the Rust splitter nor the JS consumer (`js/hang/src/container/consumer.ts`) identifies/drops leading pictures, so any glitch is exposed to the renderer.

#### Concrete tasks

- \[ ] Determine whether the reference open-GOP capture (`~/CNNiEMEA2.ts` from #2050) actually uses leading pictures  -  inspect POC / `frame_num` ordering around the recovery points  -  so we know which case is real in practice.
- \[ ] Add an open-GOP source to the `test/ts` harness as a regression fixture (real capture or a synthetic clip with recovery-point SEI and no IDR). #2050 notes `test/ts` now has a `--via-srt` mode to build on.
- \[ ] End-to-end browser playback of an open-GOP broadcast via `<moq-watch>` (Chrome + WebCodecs): verify continuous playback is clean, and characterize cold tune-in (clean recovery point vs leading-picture case)  -  glitch duration, dropped vs corrupt frames, any decoder error.
- \[ ] If the leading-picture case causes a real problem, decide whether to drop leading pictures at tune-in (skip B-frames whose references precede the group start until the recovery point is reached), and where that logic should live (Rust ingest vs JS consumer).

#### References

- Splitter fix: `rs/moq-mux/src/codec/h264/split.rs` (recovery-point SEI keyframe detection).
- JS keyframe/group invariant: `js/hang/src/container/consumer.ts` (forces group index 0 to `keyframe`), `js/watch/src/video/decoder.ts` (`type: frame.keyframe ? "key" : "delta"`).
- Related: #2050, #2066, #1979.

## Closes

- [#2067](https://github.com/moq-dev/moq/issues/2067) - close this issue when the quest finishes
