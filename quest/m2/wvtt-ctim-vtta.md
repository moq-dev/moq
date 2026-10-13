# [S] fMP4 WebVTT ctim and vtta

## Goal

An fMP4 `wvtt` sample whose cue carries a `ctim` CueTimeBox, or which
interleaves `vtta` additional-text boxes, imports as WebVTT text instead of
failing the import. Today both are refused with `InvalidSubtitle`, as is any
inline timestamp in a cue payload.

## Plan

Both boxes are defined by ISO/IEC 14496-30 §6.6.

- `ctim` records the cue's start on the original WebVTT timeline. Rebase each
  inline `<hh:mm:ss.ttt>` timestamp in the payload by `inline - ctim` onto the
  sample start, reusing the Matroska rebase in `rs/moq-mux/src/container/text.rs`.
  Refuse inline timestamps that arrive without `ctim`, since their origin is
  unknown.
- `vtta` holds text between cues (such as `NOTE` blocks). Serialize it in sample
  order, validated like the Matroska cue-comment path.

Cover an inline timestamp with `ctim` on a shifted clock, one without `ctim`,
and a sample mixing `vtta` and `vttc`.
