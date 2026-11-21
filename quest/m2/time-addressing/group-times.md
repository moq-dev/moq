# [L] lite-07 Group Streams carry each time once, and GROUP_END the group's end

## Goal

On lite-07-wip, a Group Stream carries each time once:

- GROUP keeps `Group Start` and loses `Previous Start`.
- The first FRAME's `Timestamp Delta` counts from the Group Start (on a Fetch
  Stream, from the fetched group's start), so frame 0's delta is 0.
- A FRAME on a scale-less track has no `Timestamp Delta`; its time is its
  group's start.
- GROUP_END carries the group's `End` as a delta from its highest frame
  timestamp.

A group's span runs from its lowest frame timestamp to its End and never
overlaps another group's span. The publisher names any span between one
group's End and the next group's start with SUBSCRIBE_SKIP: a discontinuity
that no group contains. Rust, `@moq/net`, the moq-lite draft, and the
subscribe model change together.

## Plan

Decided 2026-11-20 (maintainer, /quest-plan of a pre-cut audit of the lite-07
and hang-04 wire changes). This reverses the 2026-11-19 decisions, made on
closed [moq-dev/moq#9192](https://github.com/moq-dev/moq/pull/9192), that made
GROUP_END a bare whole marker and aged a group with no successor from its
frames; [moq-dev/moq#9221](https://github.com/moq-dev/moq/pull/9221), which
implemented them, is closed.

- ✅ GROUP_END keeps an end, as a delta from the group's highest frame
  timestamp, 0 or more, or from Group Start in a group with no frame
  (rejected: a bare whole marker; main's absolute End with its rule relaxed to
  "at or above"; a delta from the last frame). The end lets a relay prove a
  time falls inside a held whole group without its successor. It also ages a
  group exactly when its successor is dropped.
  - The base is the highest frame, not the last, because B-frames already send
    timestamps backwards within a group (FRAME's delta is signed). A group's
    duration is its End minus its lowest frame timestamp.
  - A delta of 0 or more also fixes main's rule that End exceed every frame
    timestamp. moq-mux broke that rule by writing hang's end marker at T and
    finishing the group at T (`container/producer.rs` `close_group`).
- ✅ Previous Start goes, and gaps are covered by End (rejected: a Previous
  Gap, as moq-transport's Prior Group ID Gap; a producer finishing a group at
  its successor's start). For continuous audio and video, each successor
  starts at the previous End and nothing is left to account for.
  - ✅ The publisher always names a shortfall, [End, next start), with
    SUBSCRIBE_SKIP when it creates the next group, as a discontinuity. In the
    maintainer's words, that is "more correct than implying the previous group
    contained those timestamps". The cases are a pause, a sparse-keyed data
    track (one small SKIP per group), and a group closed without GROUP_END,
    where the starts after its own up to the next are skipped.
  - `track::Producer::append_group` already records that span as skipped.
    The publisher's elision of skips that a GROUP's Previous Start passed over
    (`lite/publisher.rs` `TrackRun::elided`) goes.
- ✅ GROUP keeps Group Start, and the first FRAME's delta counts from it. The
  maintainer left this to whatever reads better (rejected: frame 0 carrying
  the start, since a resumed stream with Frame Start above 0, and an unsent
  group, have no frame 0; counting from the previous group's last frame, since
  each stream must decode alone). It costs one byte per group over a GROUP
  with no start.
- ✅ A scale-less FRAME carries no Timestamp Delta, as an uncompressed track's
  carries no Inflated Size (rejected: its own timestamp, which nothing reads,
  and which moq-archive's untimed format and libmoq would have to store; a
  delta that must be 0). TIMESTAMP_MISMATCH's scale-less case goes.
  - moq-transport Object IDs don't ride as frame timestamps. Group and object
    numbers are separate spaces there, and an untimed track's Group IDs are
    dense, so the `ietf` extension's Object Record keeps `Skip`.

Recommended, for review:

- A time inside a skipped span belongs to no group. A time start there begins
  at the next group, and a time end there stops after the previous one.
  Today's Time Anchor rule ("the group whose start is the highest at or below
  it") changes to match.
- A cache proves a time inside a held whole group's span from that group
  alone.
- Aging: a whole group's reach is its own End, and any other group's is
  bounded by its successor's start, as today.
- With open-GOP leading pictures, a group's lowest frame sits below its
  start. Check the Time Anchor rule against spans there. Since [moq-dev/moq#9264](https://github.com/moq-dev/moq/pull/9264),
  moq-mux keeps every frame of a group after every frame of a lower one, but
  `container::Producer` still finishes a group whole at the next keyframe's
  timestamp, above that keyframe's leading pictures, so the two spans
  overlap.
- `group::Producer::finish_at(end)` and `group::Consumer::end()` stay, since
  the end is what GROUP_END encodes.

Where it lives:

- Rust wire: `lite/publisher.rs` (`buffer_group_end`, GROUP encoding,
  `Stamp`, `TrackRun::elided`, `previous_start`), `lite/subscriber.rs`
  (`FrameIngest`, the GROUP decode, `IngestPhase::GroupEnd`), and
  `lite/version.rs` (`has_group_end`, which also gates Previous Start).
- JS wire: `js/net/src/lite/group.ts` (`frameDecoder`, `readFrames`),
  `js/net/src/lite/publisher.ts` (`groupEnd`, `wireTime`, the elided runs),
  and `js/net/src/lite/subscriber.ts`.
- Aging: Rust `model/track.rs` (`reach`, `own_end`, `ended_at`) and JS
  `js/net/src/track.ts` (`#endedAt`).
- Draft `drafts/draft-lcurley-moq-lite.md`: Frame, Group, Expiration (reach),
  the Time Anchor, GROUP, FRAME, GROUP_END, SUBSCRIBE_SKIP, and the changelog.
- The subscribe model, `rs/moq-net/verify/subscribe/SubscribeAccounting.tla`,
  models GROUP's Previous Start passing over skipped runs. Update it and run
  `/verify-subscribe`.

Tests: GROUP, FRAME, and GROUP_END codec round trips and fuzz; a resumed
stream (Frame Start above 0) and a Fetch Stream decode their first delta from
the group's start; a group with B-frames ends at a delta from its highest
frame; a pause and a sparse data track are named in SUBSCRIBE_SKIP while
contiguous groups send none; a group whose successor is dropped ages from its
own End. Run `just test interop --all`.

Public API: none expected beyond moq-net internals. Wire: lite-07-wip only,
which is unpublished.

## Related

- [lite-07 has no relative anchors](/quest/m2/time-addressing/no-relative-anchors.md) - its live join resolves as a time start, which this changes inside a skipped span
- [A scale-less track ages by arrival](/quest/m2/time-addressing/scale-less-arrival.md) - lite-05/06 still stamp a scale-less frame, with its group's start
