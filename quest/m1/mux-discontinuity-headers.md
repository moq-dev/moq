# [S] A discontinuity before the first frame keeps the init's parameter sets

## Goal

An avc3, hev1, or av01 track that marks a discontinuity (or seeks) before its
first frame still publishes that first keyframe with the parameter sets its
init's extra data supplied. Any caller that drives `moq_mux` this way, through
moq-ffi, moq-c, OBS, or Rust, gets a decodable first group without repeating
the headers in band. A discontinuity after the first frame keeps today's
behavior: it drops the partial access unit and its timestamps.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7029](https://github.com/moq-dev/moq/pull/7029).

Milestone: ✅ m1. Not chosen: m2.

- `import::Track::discontinuity` and `seek`
  (`rs/moq-mux/src/import/track.rs:458`, `:465`) call `reset_split`, which
  calls each splitter's `reset`: `rs/moq-mux/src/codec/h264/split.rs:245`,
  `codec/h265/split.rs:250`, `codec/av1/split.rs:162`. `reset` clears the
  in-flight access unit (`current`, `tail`). Before the first frame that unit
  holds only the SPS/PPS (or AV1 sequence header) fed from extra data, which
  a keyframe has not adopted into the retained parameter-set cache yet. An
  encoder that sends headers only as extra data, such as OBS's x264, never
  repeats them, so the first group is undecodable.
- Fix it in the splitters, not in callers: on `reset`, keep header NALs (or
  OBUs) that no slice has followed, or adopt them into the retained cache, so
  the next keyframe re-injects them. Drop only slice data and pending frames.
- [moq-dev/moq#7029](https://github.com/moq-dev/moq/pull/7029) worked around it in OBS by gating the discontinuity on `Track::started`
  (`cpp/obs/src/moq-output.cpp`). Once the splitters keep the headers, that
  gate can stay as a no-op guard or go; keep its regression test either way.
- Test each codec in `moq_mux`: extra-data headers, a discontinuity, then a
  bare keyframe, asserting the published frame carries the parameter sets.

Public API: none. Wire: none.
