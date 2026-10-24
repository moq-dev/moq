# [XS] Stats group linger ends held tracks

## Goal

At `stats.depth >= 1`, the documented contract matches the code: a group's
broadcast lives while it has traffic, and once it lingers out, every track it
carries ends, including presence, prefix, and requested tier tracks a
subscriber still holds. A reader follows the group's next announcement.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer.
`GroupPublisher::lingered` (`rs/moq-stats/src/produce.rs`) counts only
traffic and session rows, and `finish` ends every track. This is the intended
behavior. Subscribers do not keep a group announced, which matches announcements
being online or offline and keeps a reader from pinning a group's announcement
across a cluster. `doc/concept/stats.md` describes held presence and prefix
tracks as lasting until the last subscriber leaves, which is true only for
depth 0's permanent node broadcast.

- Update `doc/concept/stats.md`: at depth >= 1, held and requested tracks
  end with their group's linger.
- Add depth-1 tests (mocked time): a held idle presence track, a held
  prefix track, and a requested tier track each finish when the group
  lingers out, and are served again after the group's next announcement.

Public API: none. Wire: none.

## Related

- [Stats groups as tracks](/quest/m1/stats-group-tracks.md) - the new layout keeps the node broadcast stable instead
