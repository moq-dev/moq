# [L] Stats totals and prefix tracks

## Goal

Each node's stats broadcast publishes per-group cumulative totals that are
never pruned while the group is announced, plus a stats track for any
prefix within the group, served only when requested.
The per-path map tracks are gone. While a group stays announced, a reader
that misses a frame loses nothing: the next totals frame still carries
everything since the epoch began.

## Plan

Decided 2026-10-05 (planned from moq-dev/moq.pro#2202):

- **Why.** Every counter is cumulative per entry, and an entry is pruned right
  after the frame carrying its closing readout. A broadcast that starts and
  ends inside a group a reader missed shows in no later frame, so a lagging
  reader undercounts. The maps also grow with every live broadcast, and
  readers like billing want only per-project sums.
- **Totals.** Per group broadcast (`<prefix>[/<group>]/node/<node>`),
  per tier and role, cumulative within the [stats epoch](/doc/concept/stats.md#broadcasts).
  Every group announcement, and so every restart, starts a new epoch counted
  from zero; nothing is serialized to disk. `Registry`'s unpruned lifetime totals
  (`rs/moq-net/src/stats.rs`) are node-wide, per tier and role only, so they
  can't be published as-is: keep the totals per group, folding an entry into
  its group's total when it is pruned, or two projects sharing a tier would
  merge.
- **Idle groups** (decided 2026-10-05). A group broadcast already stays
  announced for the stats linger (`produce::Config::linger`, landed in
  [#4871](https://github.com/moq-dev/moq/pull/4871)) after its last entry
  ends. Today a path that left drops out of frames while the group lingers;
  with totals, a group that returns within the linger continues its totals. A zero linger is valid: a returning group then always
  takes a new epoch. After the linger it unannounces and drops its
  totals; a return announces under a new [epoch](/doc/concept/stats.md#broadcasts)
  counted from zero. Totals are cumulative and a lingering group's frames all
  repeat its final totals, so a reader that misses every frame across the
  linger loses that epoch's tail; billing under-bills by that tail,
  consistent with the 0-bill baseline. Document that. Memory is bounded by active and
  lingering groups. Test two groups sharing a tier, a group returning within
  the linger (same epoch, totals continue), and one returning after it (new
  epoch from zero).
- **Prefix tracks** (decided 2026-10-05, replacing per-broadcast tracks).
  Routing is by prefix, so a reader requests any prefix of a member path (so
  its depth is bounded by `Path::MAX_PARTS` less the group's own segments) and
  gets one track: the prefix's rollup, its self counters, and a one-level
  map of each direct child's rollup (a channel's broadcasts).
  Nothing is produced for a prefix no one holds. The relay's `--stats-depth`
  only places the group broadcasts; requests are not capped by it.
- Flow counters (bytes, frames, groups, fetches) count from zero when the
  prefix is first held and fold in members that end while it is held. Their
  absolute values are not authoritative, so readers take differences within
  one subscription; the group totals carry the real sums.
- Started/ended pairs (announces, broadcasts, subscriptions, sessions) are
  seeded at registration: `started` with each current member's
  `started - ended`, `ended` at zero, so every `active` derived from a pair
  stays exact for a prefix held mid-stream. Test a prefix held while 300
  subscriptions are live, then watch them leave.
- A request for a prefix with no members is held open at zero and reclaimed
  when its last consumer leaves. Track names must stay injective: tiers and
  paths both contain `/`.
- A child leaves the one-level map after the frame carrying its closing
  readout; its tail stays in the parent's rollup. So the map holds live and
  just-ended children, not every child the prefix has ever seen. Decide
  while implementing whether a map with hundreds of children needs a cap or
  paging.
- **Self counters** (decided 2026-10-07, planned from
  moq-dev/moq.pro#2165). A prefix track also carries cumulative counters for
  members at exactly the prefix, not its descendants, so a reader can show a
  broadcast that also has nested broadcasts. A reader can't derive them as the
  rollup less the current children: a pruned child's tail stays in the
  rollup, so the difference jumps when the child leaves the map (self 10 and
  a child at 100 reads 110 less 100, then 110 less nothing) and reads as a
  rate spike. They follow the rollup's rules: held from zero, pairs seeded,
  merged by the aggregator. Test a nested broadcast ending and leaving the
  map while the parent prefix stays held: the parent's self counters stay
  flat and its rollup keeps the child's tail.
- The aggregator merges a prefix track across nodes with a baseline per
  upstream (node, epoch) hold, and treats a decrease as that upstream's
  reset: a node restart, a new group epoch, or the aggregator re-holding the
  prefix. A merged prefix track then never regresses within one downstream
  subscription. Test a join, a leave, and a restart under a held prefix.
- Keep the hot path lock-free: a held prefix registers atomic accumulators,
  each entry caches its held ancestors' (and the matching child slot in
  each), and an update bumps them in a loop, about twice the held depth per
  update. Registration and release reach existing entries without a lock or
  a walk of every entry, for example an `ArcSwap` ancestor list or a
  generation counter rechecked on bump. Benchmark across held-prefix count
  and depth.
- Held prefixes get their own cap per group broadcast, sized (or
  configurable) for every reader the aggregator fans in. Refuse requests
  beyond it with a typed error the reader retries, rather than a parked
  track that reads as zero. Tier requests park instead
  (`MAX_PARKED_REQUESTS`) because a collector treats a refusal as final;
  prefix readers must not.
- **Sessions** (decided 2026-10-05). `sessions.json` is keyed by auth root
  and loses pruned roots the same way, so fold per-tier `Presence` into the
  totals (session counts) and serve per-root detail as a requested track,
  the same model as prefix tracks. Open PR #5046 (session outcomes) adds to
  `sessions.json`, which this retires; it is rewritten against these totals
  and the per-root requested track, and lands after this (decided
  2026-10-08).
- **Retire the map tracks** (`publisher.json`, `subscriber.json`,
  `sessions.json`, and their `.json.z` siblings) in the same release. Decide
  while implementing whether totals and prefix tracks keep `.json.z`
  siblings.
- Ship in the same breaking release as stats epochs, so consumers take one
  stats path and wire change, not two.
- Update the aggregator, `doc/concept/stats.md`, and `demo/web/src/stats.ts`
  (which reads the three maps) with the change. The
  aggregator merges totals across nodes, and merges a prefix track across
  nodes when a reader requests it, so a multi-node consumer serves the
  same format it reads.

Decided 2026-10-08: if prefix tracks and self counters hold up the rest,
split them into a follow-up quest and land totals, sessions, and the map
retirement first.

Public API: `moq-stats` producer and consumer types. Wire: stats track names
and payloads.

MoQ Pro adopts it when it pins the release: billing reads totals, its
Broadcasts page reads one prefix per visible row and group header, its `announced` probe
reads totals only, and its customer stats feed serves this format summed
across nodes.

## Related

- [Media stats](/quest/m1/stats/README.md) - publisher and viewer media stats
  stay hang tracks, separate from the relay's stats
- [QoS](/quest/m1/qos/README.md) - stats-split lands first; the egress lag histogram split out of #4133 requires it and rebases onto the totals and prefix tracks
