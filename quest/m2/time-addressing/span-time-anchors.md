# [M] A time anchor lands by group spans, and a cache proves a time from a whole group

## Goal

On lite-07, a time anchor resolves against group spans, in Rust, `@moq/net`,
and the moq-lite draft. A time inside a skipped span belongs to no group: a
time start there begins at the next group, and a time end there stops after
the previous one. A time inside a held whole group's span resolves to that
group, and a relay's cache answers it from that group alone, without its
successor. Today's Time Anchor rule ("the group whose start is the highest at
or below it") changes to match.

## Plan

Left from [moq-dev/moq#9263](https://github.com/moq-dev/moq/pull/9263), which
made GROUP_END carry a whole group's End and has the publisher name a
shortfall, [End, next start), with SUBSCRIBE_SKIP. The maintainer recommended
both rules for review on 2026-11-20; neither is decided.

- A copy records the starts strictly inside a whole group's span as skipped
  when its GROUP_END arrives (`lite/subscriber.rs`, JS `runGroup`), and a
  publisher's append records them the same way, so the track model already
  tells a span from a pause. A pause past End is a skip run whose start is
  some group's End: the anchor needs to tell "inside a span" from "inside a
  pause", which only the End says.
- The cache proof is AGENTS.md's Required rule: answer only what upstream
  would answer. A held whole group proves every time in [start, End).
- A span runs from Group Start, [Group Start, End) (maintainer, 2026-11-21).
  An open-GOP group's leading pictures present below its start, inside the
  lower group's span, so a time among them resolves to the lower group: a
  subscription starts at a group's first frame, and those pictures decode
  only from there.
- It overlaps the live join of lite-07's anchors
  ([moq-dev/moq#9265](https://github.com/moq-dev/moq/pull/9265)), which
  resolves as a time start, and
  [moq-dev/moq#9172](https://github.com/moq-dev/moq/pull/9172).

Where it lives: Rust `model/track.rs` (time starts, `find_landable`,
`outside_spans`), `model/resume.rs`, `lite/publisher.rs` (pinning a time
anchor), JS `js/net/src/track.ts`, and the draft's Time Anchor text.

Public API: none expected. Wire: none; the draft's Time Anchor rule changes.

## Related

- [First-party producers keep lite-07 group spans apart](/quest/m2/producer-spans.md) - the non-overlap rule this resolution relies on
