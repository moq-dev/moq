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
  retired total from the [bounded aggregate](/quest/m0/broadcast-epoch/stats-aggregate-bound.md)
  instead of regressing the merged total.
- MoQ Pro wants to share the same epoch with its storage stats, so expose it
  from `Producer`.
- `release` carries a temporary seed instead
  ([#4810](https://github.com/moq-dev/moq/pull/4810)): a producer's first group
  number is wall-clock microseconds, so a restarted node numbers above its
  previous run. Epochs replace it, so it never reaches `main`: release-to-main
  back-merges keep `main`'s `rs/moq-stats` and `doc/concept/stats.md`. Drop the
  seed and its `doc/concept/stats.md` sentence from `release` when this ships
  there. MoQ Pro's VOD `storage.json` seeds the same way and moves to the
  shared epoch with it.
- demo/web stats keys include the epoch. Update `doc/concept/stats.md`,
  `doc/bin/relay/config.md`, and the relay stats config docs.

Public API: path shape change for every stats consumer. Wire: stats broadcast
names gain a trailing epoch segment.

## Required

- [Bounded stats aggregate](/quest/m0/broadcast-epoch/stats-aggregate-bound.md) - retired nodes fold into a bounded total, which epochs churn
