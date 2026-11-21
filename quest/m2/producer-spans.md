# [M] First-party producers keep lite-07 group spans apart

## Goal

moq-mux's `container::Producer` and hang's legacy `Producer`, in Rust and JS,
keep the lite-07 draft's publisher MUST: a group's span never overlaps
another's, and no group starts after a whole group's start and before its
End. Neither producer closes a group without the End it meant to write. A
lite-07 copy then never refuses a group a first-party producer published.

## Plan

Left from group times
([moq-dev/moq#9263](https://github.com/moq-dev/moq/pull/9263)), which put the
span rule in the draft and made `group::Producer::finish_at` refuse an End
below a group's highest frame. [Groups never overlap](/quest/m2/groups-never-overlap.md)
([moq-dev/moq#9264](https://github.com/moq-dev/moq/pull/9264)) keeps every
frame of a group after every frame of a lower one, but not the End. Three
cases remain:

- **The guards.** moq-mux's `close_group` (`rs/moq-mux/src/container/producer.rs`)
  and hang's legacy `#close` (`js/hang/src/container/legacy.ts`) close a group
  without an End when theirs would fall below the newest frame or the duration
  marker. Before #9264, a keyframe below the newest frame hit them
  (`h264_join_below_a_p_frame_publishes` and the TS export's unmarked-step
  tests fail without the Rust guard). With #9264 merged, every moq-mux test
  passes without it (checked on 2026-11-20); only a caller's `cut(Some(end))`
  below a frame it wrote still reaches it, for audio, data, or reordered
  video, which `close` doesn't refuse. Recommended: delete both guards and
  refuse that cut with `InvalidEnd` for every kind, as ordered video already
  is.
- **The discontinuity marker.** A discontinuity after `cut(end)` stamps its
  marker group one unit below `end`, inside the span of the group `cut` just
  ended whole there (moq-mux `Producer::discontinuity`, hang's legacy
  `discontinuity`). A lite-07 copy records that span skipped once its
  GROUP_END arrives, so it refuses the marker if it arrives second.
  `container::ts::recovery_test::a_lost_discontinuity_marker_still_breaks_the_clock`
  exercises it locally. Recommended: when the closing group ended whole,
  stamp the marker at its End and key the next group strictly after it, as a
  seek's marker already does (`crate::start::next`).
- **Open-GOP leading pictures.** `container::Producer` finishes a group whole
  at the next keyframe's timestamp, above that keyframe's leading pictures.
  With a span of [Group Start, End), as decided below, those pictures present
  inside the lower group's span without breaking the rule, and moq-net's
  accounting already counts only the starts strictly inside a span. Nothing
  changes in the producers for this case; the test below keeps it.

Decided 2026-11-21 (maintainer, /quest-complete):

- ✅ A group's span runs from Group Start, so a span is [Group Start, End)
  (rejected: from the lowest frame, naming the leading pictures' gap with
  SUBSCRIBE_SKIP; from the lowest frame, holding GROUP_END until the next
  group's lowest frame is known). A subscription starts at a group's first
  frame, so a time among the next group's leading pictures must resolve to the
  lower group for those pictures to decode. This reverses the 2026-11-20
  "lowest frame" decision; group times
  ([moq-dev/moq#9263](https://github.com/moq-dev/moq/pull/9263)) puts
  [Group Start, End) in the lite-07-wip draft.

Tests: a cut below a written frame is refused for audio and reordered video;
a discontinuity after `cut(end)` publishes a marker a lite-07 copy accepts in
either arrival order; an open-GOP stream publishes groups whose spans hold no
other group's start, and whose frames all present before every frame of a
higher group.

Public API: `cut(Some(end))` below a written frame returns `InvalidEnd` for
every kind. Wire: none.

## Required

- [Groups never overlap](/quest/m2/groups-never-overlap.md) - the producers keep every frame of a group after every frame of a lower one, which leaves the guards only a caller's bad cut

## Related

- [A time anchor lands by group spans](/quest/m2/time-addressing/span-time-anchors.md) - resolves a time against the spans this keeps apart, including a leading picture's
