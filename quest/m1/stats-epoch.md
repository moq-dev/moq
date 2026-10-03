# [S] Stats epochs

## Goal

Each stats producer publishes under its broadcast epoch,
`<prefix>[/<group>]/node/<node>/@<epoch>`, so a restarted node never reuses a
broadcast name or the group numbers a relay cached under it.

## Plan

- Use the shared `moq_net::Epoch`, not a stats-local UUID (decided
  2026-10-02). One epoch per `Producer`, shared by all its group broadcasts.
- [#4739](https://github.com/moq-dev/moq/pull/4739)'s original commits are
  prior art: path syntax, `Producer::epoch()`, `parse_node_path`, the
  aggregator's restarted-epoch handling, and the tests. The PR landed only the
  producer-wide group allocator.
- Decide what an unset node becomes once the epoch follows it (#4739 used
  `local`).
- The aggregator treats a new epoch as a new node: its counters add to the
  retired total from the [bounded aggregate](/quest/m1/stats-aggregate-bound.md)
  instead of regressing the merged total.
- MoQ Pro wants to share the same epoch with its storage stats, so expose it
  from `Producer`.
- demo/web stats keys include the epoch. Update `doc/concept/stats.md`,
  `doc/bin/relay/config.md`, and the relay stats config docs.

Public API: path shape change for every stats consumer. Wire: stats broadcast
names gain a trailing epoch segment.

## Required

- [Epoch primitive](/quest/m1/epoch.md) - the shared `Epoch` type and path split
- [Bounded stats aggregate](/quest/m1/stats-aggregate-bound.md) - retired nodes fold into a bounded total, which epochs churn
