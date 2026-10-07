# [S] Stats epochs

## Goal

Each stats group broadcast at `<prefix>[/<group>]/node/<node>` announces under
its own epoch, minted each time the group is announced, so neither a restarted
node nor a group returning from idle reuses a broadcast identity or the group
numbers a relay cached under it.

## Plan

- Use the shared `moq_net::Epoch`, not a stats-local UUID (decided
  2026-10-02).
- One epoch per group announcement (decided 2026-10-05, replacing 2026-10-02's
  one epoch per `Producer`). A group that goes idle unannounces after its
  linger and drops its totals; when it returns it announces under a new epoch
  counted from zero. Why: a producer never holds an idle group's state, so its
  memory is bounded by active and lingering groups, and readers see every
  reset as a new broadcast rather than detecting one. The cost: a reader that
  misses every frame across the linger loses that epoch's tail; billing
  under-bills by that tail, consistent with the 0-bill baseline.
  At depth 0 the single broadcast never unannounces, so its epoch still lasts
  the producer's life.
- [#4739](https://github.com/moq-dev/moq/pull/4739)'s original commits are
  prior art for the aggregator's restarted-epoch handling and the tests; its
  path syntax is superseded, since the epoch is on the route. The PR landed only the
  producer-wide group allocator.
- The aggregator treats a new epoch as a new node: its counters add to the
  merged total instead of regressing it, and the old epoch folds into the
  retired total once `aggregate::Config::grace` elapses. Test it.
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

Public API: stats consumers read the epoch from the route. Wire: none beyond
the route epoch.
