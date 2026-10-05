# [S] Stats epochs

## Goal

Each stats group broadcast publishes under its own epoch,
`<prefix>[/<group>]/node/<node>/@<epoch>`, minted each time the group is
announced, so neither a restarted node nor a group returning from idle reuses
a broadcast name or the group numbers a relay cached under it.

## Plan

- Use the shared `moq_net::Epoch`, not a stats-local UUID (decided
  2026-10-02).
- One epoch per group announcement (decided 2026-10-05, replacing 2026-10-02's
  one epoch per `Producer`). A group that goes idle unannounces after its
  linger and drops its totals; when it returns it announces under a new epoch
  counted from zero. Why: a producer never holds an idle group's state, so its
  memory is bounded by active and lingering groups, and readers see every
  reset as a new path rather than detecting one. The cost: a reader that
  misses a group's last frame before the unannounce loses those increments.
  At depth 0 the single broadcast never unannounces, so its epoch still lasts
  the producer's life.
- [#4739](https://github.com/moq-dev/moq/pull/4739)'s original commits are
  prior art: path syntax, `parse_node_path`, the
  aggregator's restarted-epoch handling, and the tests. The PR landed only the
  producer-wide group allocator.
- Decide what an unset node becomes once the epoch follows it (#4739 used
  `local`).
- The aggregator treats a new epoch as a new node: its counters add to the
  retired total from the [bounded aggregate](/quest/m0/broadcast-epoch/stats-aggregate-bound.md)
  instead of regressing the merged total.
- Expose each group broadcast's epoch (a per-group accessor replacing
  #4739's `Producer::epoch()`) so a consumer can tell epochs apart.
  MoQ Pro's VOD `storage.json` mints its own epoch rather than sharing a
  producer-wide one, which no longer exists.
- `release` carries a temporary seed instead
  ([#4810](https://github.com/moq-dev/moq/pull/4810)): a producer's first group
  number is wall-clock microseconds, so a restarted node numbers above its
  previous run. Epochs replace it, so it never reaches `main`: release-to-main
  back-merges keep `main`'s `rs/moq-stats` and `doc/concept/stats.md`. Drop the
  seed and its `doc/concept/stats.md` sentence from `release` when this ships
  there. MoQ Pro's VOD `storage.json` seeds the same way and moves to its
  own epoch with it.
- demo/web stats keys include the epoch. Update `doc/concept/stats.md`,
  `doc/bin/relay/config.md`, and the relay stats config docs.

Public API: path shape change for every stats consumer. Wire: stats broadcast
names gain a trailing epoch segment.

## Required

- [Bounded stats aggregate](/quest/m0/broadcast-epoch/stats-aggregate-bound.md) - retired nodes fold into a bounded total, which epochs churn
