# [M] A lite-06 reader keeps open-GOP leading pictures across a group step

## Goal

On moq-lite-06, `import ts` → relay → `export ts` carries every picture of a
field-coded open-GOP feed, as moq-lite-07 and moq-transport already do. A
group step that loses nothing logs no `media skipped upstream`, for video and
for MP2 audio, while a real loss still arms the export's recovery.

## Plan

Reported in [#9081](https://github.com/moq-dev/moq/issues/9081), caused by
[#8638](https://github.com/moq-dev/moq/pull/8638) (groups addressed by their
start). lite-07 proves a step with GROUP_END's End. lite-06 carries no End
(`has_group_end`) and declares no skipped starts, so `container::Consumer`
marks the step `unproven` and judges it by `pts_contiguous(presented_end,
first)` (`rs/moq-mux/src/container/consumer.rs`). `first` is the next group's
first frame in decode order, its keyframe, and `presented_end` is the previous
group's highest PTS, since legacy frames carry no duration. An open GOP's
leading pictures arrive after the keyframe but present before it, so the
check sees a hole of their span plus one picture, `lose()` fires, and the TS
export trims them (`leading_floor` in `container/ts/export.rs`). This is the
reporter's 331 of 331 missing PES.

Decided 2026-11-21 (maintainer, /quest-import of #9081):

- ✅ Fix it on lite-06 (rejected: MP2 only, leaving the video loss until
  lite-07 becomes the default; won't fix). lite-06 stays in use at least a
  cycle past the cut, since the newest and previous releases interoperate.
- ✅ Judge an unproven step by the next group's lowest PTS against the
  previous group's highest (rejected: trusting lite-06 SUBSCRIBE_DROP, so an
  unnamed gap is no loss, which misses a group reset before its header and
  third-party publishers; never trimming on lite-06, which corrupts the
  leading pictures after a real loss). Open GOP itself stays as decided in
  [producer-spans](https://github.com/moq-dev/moq/pull/9317): a span is
  [Group Start, End), so group starts and frame order are unchanged.

Recommended, not decided:

- Don't hold the keyframe for the verdict. Emit it at once, and judge when
  the next frame (or the group's end) shows the group's lowest PTS, reporting
  any gap before that frame. The trim acts only on the leading pictures, which
  follow the keyframe anyway. Readers of the gap event, the TS export first,
  must then accept a gap after a group's first frame. If that proves
  unworkable, hold the keyframe until the next frame arrives instead, which
  costs only that frame's arrival on the same stream.
- The previous group's highest PTS is one picture short of its end. Allow one
  picture, taken as the largest gap between consecutive presentation times in
  the previous group. That needs no parser and handles a field/frame mix
  (20 and 40 ms), while a lost group leaves a gap of a whole group. Codec
  timing from the SPS VUI was considered: it is optional, and field pictures
  need per-picture parsing.
- MP2 gets a codec-duration arm in `Audio::new`
  (`rs/moq-mux/src/container/legacy/mod.rs`) from its 1152 samples per frame
  (`codec/mp2.rs`), as Opus, AAC-LC and PCM have. That ends the 24 ms skip
  logged at every MP2 frame.
- Amend hang's consumer rule ("contiguous within 1 ms",
  `drafts/draft-lcurley-moq-hang.md`) for a legacy video step with no end
  marker, in the same PR.

Tests: the reporter's `an_open_gop_that_continues_the_previous_video_group_is_no_gap`
from #9081, which reads `"gap 120000+160000"` on `main`. A step that does lose
a group still reports the gap and trims. A progressive closed-GOP stream with
B-frames logs no skip. MP2 groups bridge as Opus does in
`codec_durations_bridge_undeclared_runs_between_audio_groups`. Validate on the
reporter's capture, which they offered to run.

Public API: none. Wire: none; hang's consumer rule changes in the draft.

## Closes

- [#9081](https://github.com/moq-dev/moq/issues/9081) - export ts drops about a fifth of a field-coded feed's pictures since #8638

## Related

- [TS open-GOP recovery tests](/quest/m5/ts-open-gop-join.md) - the join-side trim of the same leading pictures
