# [M] The lite-07 wire addresses groups by start

## Goal

moq-lite-07 carries no group sequence numbers: a GROUP names its start and
its predecessor's start, GROUP_END says where a whole group ends, and every
subscription bound, drop, and datagram is a time. Rust speaks it, the lite
draft specifies it, and lite-01..06 are unchanged.

## Plan

Decided 2026-10-25 (maintainer), from the spike:

- **GROUP** carries the start and the previous group's start, absent for the
  first group of an epoch, so a cache or reader sees a hole without a round
  trip.
- **GROUP_END** carries the group's exclusive end timestamp, meaning the
  group is whole. A bare FIN means an incomplete group whose frames all
  arrived, and a reset aborts. A relay closes an expired or partial group
  instead of resetting it, which changes the draft's reset rule for those
  groups (around line 760).
- SUBSCRIBE and SUBSCRIBE_UPDATE bounds, SUBSCRIBE_START and its largest,
  SUBSCRIBE_END, SUBSCRIBE_DROP, and datagrams address by start. Drops name
  time spans with no group to deliver, not sequence ranges. `Relative(n)`
  stays a group count.
- Positions stay (start, frame index), so
  [frame offsets](/quest/m1/transport-frame-offsets.md) carry over.

Update `drafts/draft-lcurley-moq-lite.md` (fields, the group-end rules, and the
lite-07 changelog) and `doc/concept/moq-lite.md`, and run `just drafts check`
and `just test interop --all`.

Public API: none beyond [the model](/quest/m1/time-addressing/model.md).
Wire: lite-07-wip GROUP, GROUP_END, SUBSCRIBE, SUBSCRIBE_UPDATE,
SUBSCRIBE_START, SUBSCRIBE_END, SUBSCRIBE_DROP, and datagrams change.

## Required

- [Rust groups by start](/quest/m1/time-addressing/model.md) - the model this wire carries
