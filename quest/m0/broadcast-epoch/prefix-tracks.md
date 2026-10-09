# [L] Stats prefix tracks

## Goal

A reader of a node's stats broadcast can request a stats track for any prefix
within the group and gets that prefix's rollup, its self counters, and a
one-level map of its direct children, produced only while requested. The
per-path map tracks (`[<tier>/]publisher.json`, `[<tier>/]subscriber.json`, and
their `.json.z` siblings) are gone.

## Plan

Split from stats-split (decided 2026-10-08), which landed the per-group
`totals.json` and the requested per-root `<root>/presence.json` and retired
`sessions.json`. Reuse what it built: the `<key>/<kind>.json` naming (the last
segment picks the kind, everything before it is the key), the requested-track
hold and reclaim in `rs/moq-stats/src/produce.rs`, the cap refused with
`TooManyRequests`, and the aggregator's per-node reset baseline and refusal
retry in `rs/moq-stats/src/aggregate.rs`.

Decided 2026-10-05 and 2026-10-07 (planned from moq-dev/moq.pro#2202 and
moq-dev/moq.pro#2165):

- **Prefix tracks.** Routing is by prefix, so a reader requests any prefix of
  a member path (depth bounded by `Path::MAX_PARTS` less the group's own
  segments) and gets one track: the prefix's rollup, its self counters, and a
  one-level map of each direct child's rollup (a channel's broadcasts).
  Nothing is produced for a prefix no one holds. The relay's `--stats-depth`
  only places the group broadcasts; requests are not capped by it. Track names
  must stay injective: tiers and paths both contain `/`.
- Flow counters (bytes, frames, groups, fetches) count from zero when the
  prefix is first held and fold in members that end while it is held. Their
  absolute values are not authoritative, so readers take differences within
  one subscription; the group totals carry the real sums.
- Started/ended pairs (announces, broadcasts, subscriptions) are seeded at
  registration: `started` with each current member's `started - ended`,
  `ended` at zero, so every `active` stays exact for a prefix held
  mid-stream. Test a prefix held while 300 subscriptions are live, then watch
  them leave.
- A request for a prefix with no members is held open at zero and reclaimed
  when its last consumer leaves.
- A child leaves the one-level map after the frame carrying its closing
  readout; its tail stays in the parent's rollup. Decide while implementing
  whether a map with hundreds of children needs a cap or paging.
- **Self counters.** A prefix track also carries cumulative counters for
  members at exactly the prefix, so a reader can show a broadcast that also has
  nested broadcasts. A reader can't derive them as the rollup less the current
  children: a pruned child's tail stays in the rollup, so the difference jumps
  when the child leaves the map. They follow the rollup's rules. Test a nested
  broadcast ending and leaving the map while the parent prefix stays held: the
  parent's self counters stay flat and its rollup keeps the child's tail.
- The aggregator merges a prefix track across nodes with a baseline per
  upstream (node, epoch) hold and treats a decrease as that upstream's reset,
  so a merged prefix track never regresses within one downstream
  subscription. Test a join, a leave, and a restart under a held prefix.
- **Hot path.** Keep it lock-free: a held prefix registers atomic
  accumulators, each entry caches its held ancestors (and the matching child
  slot in each), and an update bumps them in a loop, about twice the held
  depth per update. Registration and release reach existing entries without a
  lock or a walk of every entry, for example an `ArcSwap` ancestor list or a
  generation counter rechecked on bump. Benchmark across held-prefix count and
  depth.
- Held prefixes get their own cap per group broadcast, refused with a typed
  error the reader retries, like presence tracks.
- Retire the map tracks in the same release, and decide whether prefix tracks
  keep `.json.z` siblings. Update the aggregator, `doc/concept/stats.md`,
  `doc/bin/inspect.md`, and `demo/web/src/stats.ts` (which reads both maps).

MoQ Pro's Broadcasts page reads one prefix per visible row and group header
the moment the maps retire, so the maps and prefix tracks change in one
release.

Public API: `moq-stats` producer and consumer types. Wire: stats track names
and payloads.

## Related

- [Viewer lag histogram](/quest/m1/qos/lag-histogram.md) - lands on the totals
  and these prefix tracks
- [Publisher timeliness at relay ingest](/quest/m1/qos/publisher-timeliness.md) - the drift
  histogram lands on the same tracks
