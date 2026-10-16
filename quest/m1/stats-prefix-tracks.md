# [L] Stats prefix tracks

## Goal

A reader of a node's stats broadcast can request a stats track for any prefix
within the group and gets that prefix's rollup, its self counters, and a
one-level map of its direct children, produced only while requested. The
per-path map tracks (`[<tier>/]publisher.json`, `[<tier>/]subscriber.json`, and
their `.json.z` siblings) stay beside them, unchanged, for existing readers.

## Plan

Split from stats-split (decided 2026-10-08), which landed the per-group
`totals.json` and the requested per-root `<root>/presence.json` beside the
existing tracks. Reuse what it built: the `<key>/<kind>.json` naming (the last
segment picks the kind, everything before it is the key), the requested-track
hold and reclaim in `rs/moq-stats/src/produce.rs`, and the aggregator's
per-node reset baseline in `rs/moq-stats/src/aggregate.rs`.

Decided 2026-10-05 and 2026-10-07:

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
- Held prefixes have no cap: a hold is reclaimed when its last reader leaves,
  like presence tracks, whose cap was dropped for that reason (decided
  2026-10-12 in #5084).
- Decide whether prefix tracks keep `.json.z` siblings. Update the
  aggregator, `doc/concept/stats.md`, and `doc/bin/inspect.md`.

Decided 2026-10-12 (#5084): the map tracks are kept, not retired. Prefix
tracks are added beside them so existing readers keep working, the same
backwards-compatible shape stats-split took when it kept `sessions.json`.
This supersedes the 2026-10-05 plan to retire the maps in the same release.

Public API: `moq-stats` producer and consumer types. Wire: stats track names
and payloads.

### Open decisions

Implementation is blocked on the public wire shape. Recommended choices are
listed first so the draft can resume without rediscovery:

1. **One namespaced track per prefix (recommended):**
   `prefix/<path>/traffic.json[.z]`, with tiers and both roles in the payload.
   The fixed `prefix/` namespace makes path and tier labels unambiguous and one
   subscription returns the complete prefix view. Alternatives are one track
   per tier and role (more subscriptions and an escaping rule) or percent/base64
   encoding either the tier or path (opaque names and another canonicalization
   contract).
2. **Keep both plain and compressed siblings (recommended):** match every
   existing stats track and let old/simple readers use JSON while fleet readers
   use the compressed snapshot. The alternative removes plain JSON for this new
   track, reducing producer work but making inspection and consumers special
   cases.
3. **Do not truncate or page children initially (recommended):** benchmark the
   complete map against fan-out and rely on the existing hard frame-size error
   to fail loud. A deterministic cap silently makes the rollup disagree with
   its visible children; paging adds ordering, snapshot-consistency, and request
   lifecycle semantics before measurements show they are needed.

After these are decided, put held-prefix accumulation in `moq-net`'s registry
rather than walking every report row per held prefix in `moq-stats`: each
`(path, tier)` entry should cache the currently held ancestors and child slots,
so payload bumps stay lock-free and scale with path depth. The publisher owns
the request lifecycle and serializes those snapshots; the aggregator applies
its existing per-node reset baseline to each cumulative prefix value.

## Related

- [Viewer lag histogram](/quest/m1/qos/lag-histogram.md) - lands on the totals
  and these prefix tracks
- [Publisher timeliness at relay ingest](/quest/m2/qos-publisher-timeliness.md) - the drift
  histogram lands on the same tracks
