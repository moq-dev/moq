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
- **Totals.** Per group broadcast (`<prefix>[/<group>]/node/<node>/@<epoch>`),
  per tier and role, cumulative within the [stats epoch](/quest/m0/broadcast-epoch/stats-epoch.md).
  Every group announcement, and so every restart, starts a new epoch counted
  from zero; nothing is serialized to disk. `Registry`'s unpruned lifetime totals
  (`rs/moq-net/src/stats.rs`) are node-wide, per tier and role only, so they
  can't be published as-is: keep the totals per group, folding an entry into
  its group's total when it is pruned, or two projects sharing a tier would
  merge.
- **Idle groups** (decided 2026-10-05). Today a group broadcast disappears
  the moment it has no entries (`rs/moq-stats/src/produce.rs`). Instead it
  stays announced for the stats linger (`quest/m0/stats-linger.md`, an m0
  quest planned in [#4843](https://github.com/moq-dev/moq/pull/4843); list it
  under Related once it lands, not Required, so it does not gate the release)
  after its last entry ends, so a group that returns within the linger
  continues its totals. A zero linger is valid: a returning group then always
  takes a new epoch. After the linger it unannounces and drops its
  totals; a return announces under a new [epoch](/quest/m0/broadcast-epoch/stats-epoch.md)
  counted from zero. Totals are cumulative and a lingering group's frames all
  repeat its final totals, so a reader that misses every frame across the
  linger loses that epoch's tail; billing under-bills by that tail,
  consistent with the 0-bill baseline. Document that. Memory is bounded by active and
  lingering groups. Test two groups sharing a tier, a group returning within
  the linger (same epoch, totals continue), and one returning after it (new
  epoch from zero).
- **Prefix tracks** (decided 2026-10-05, replacing per-broadcast tracks).
  Routing is by prefix, and with epochs a broadcast is `<name>/@<epoch>`, so
  a reader requests any prefix within a group, down to `Path::MAX_PARTS`
  segments, and gets one track: the prefix's rollup plus a one-level map of
  each direct child's rollup (a broadcast's epochs, a channel's broadcasts).
  Nothing is produced for a prefix no one holds. The relay's `--stats-depth`
  only places the group broadcasts; requests are not capped by it.
- A prefix track counts from zero when first held and folds in members that
  end while it is held; a request for a prefix with no members is held open
  at zero and reclaimed when its last consumer leaves. Its absolute values
  are not authoritative, so readers take differences within one
  subscription; the group totals carry the real sums. Track names must stay
  injective: tiers and paths both contain `/`.
- Keep the hot path lock-free: a held prefix registers atomic accumulators,
  each entry caches its held ancestors', and an update bumps them in a loop.
  Benchmark across held-prefix count and depth.
- Held prefixes get their own cap per group broadcast, sized (or
  configurable) for every reader the aggregator fans in. Refuse requests
  beyond it with a typed error the reader retries, rather than a parked
  track that reads as zero. Tier requests park instead
  (`MAX_PARKED_REQUESTS`) because a collector treats a refusal as final;
  prefix readers must not.
- **Sessions** (decided 2026-10-05). `sessions.json` is keyed by auth root
  and loses pruned roots the same way, so fold per-tier `Presence` into the
  totals (session counts) and serve per-root detail as a requested track,
  the same model as prefix tracks.
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

Public API: `moq-stats` producer and consumer types. Wire: stats track names
and payloads.

MoQ Pro adopts it when it pins the release: billing reads totals, its
Broadcasts page reads one prefix per visible row and group header, its `announced` probe
reads totals only, and its customer stats feed serves this format summed
across nodes.

## Required

- [Stats epochs](/quest/m0/broadcast-epoch/stats-epoch.md) - totals are cumulative within an epoch

## Related

- [Bounded stats aggregate](/quest/m0/broadcast-epoch/stats-aggregate-bound.md) - retired
  nodes fold into a bounded total; per-epoch totals feed it
- [Media stats](/quest/m1/stats/README.md) - publisher and viewer media stats
  stay hang tracks, separate from the relay's stats
