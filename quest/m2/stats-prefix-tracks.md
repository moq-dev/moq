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
- Counters follow presence (revised 2026-10-16, #5719): each drain sums the
  live members' readouts plus the closing readouts of members that ended
  while the prefix was held. Live members carry their history from before
  the hold, so absolute values are not authoritative and readers take
  differences within one subscription; the group totals carry the real sums.
  Every `active` (started less ended) is exact for a prefix held mid-stream.
  Test a prefix held while 300 subscriptions are live, then watch them leave.
- A request for a prefix with no members is held open at zero and reclaimed
  when its last consumer leaves.
- A child leaves the one-level map after the frame carrying its closing
  readout; its tail stays in the parent's rollup.
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
- **Drain-time sums** (revised 2026-10-16, #5719). Sum held prefixes in
  `moq-stats` per drain with `Cumulative`, like presence: walk each report
  row's ancestors and look each up in the held set, so the cost scales with
  rows times path depth, not held prefixes, and nothing touches the per-frame
  path. No `moq-net` API change: registry-level prefix accumulators would
  push request lifecycle into the collection layer and put shared atomics on
  every frame bump under a held prefix. Extend `producer_tick` to sweep rows,
  held prefixes, and depth.
- Held prefixes have no cap: a hold is reclaimed when its last reader leaves,
  like presence tracks, whose cap was dropped for that reason (decided
  2026-10-12 in #5084).
- Update the aggregator, `doc/concept/stats.md`, and `doc/bin/inspect.md`.

Decided 2026-10-12 (#5084): the map tracks are kept, not retired. Prefix
tracks are added beside them so existing readers keep working, the same
backwards-compatible shape stats-split took when it kept `sessions.json`.
This supersedes the 2026-10-05 plan to retire the maps in the same release.

Decided 2026-10-16 (#5719):

- **Name.** `<path>/traffic.json`, where `<path>` is the full broadcast
  prefix, as presence takes the full root, and a prefix outside the group is
  refused. The last segment picks the kind and everything before it is the
  key, like `<root>/presence.json`, so the name is injective without a
  namespace segment. Tiers ride the payload, never the name, so one
  subscription returns the whole prefix and no escaping rule is needed.
- **Payload.** Keyed by tier like `totals.json`. Each tier entry carries the
  rollup's `publisher` and `subscriber` at its top level, so a totals reader
  reads the rollup unchanged, plus `self: {publisher, subscriber}` and
  `children: {"<segment>": {publisher, subscriber}}`, keyed by the child's
  next path segment.
- **Encodings.** Plain and `.json.z` siblings, like every other stats track.
- **No cap or paging on children.** A cap would make the rollup disagree with
  its visible children, and paging adds ordering and consistency rules no
  measurement asks for yet. A child entry carries both roles per tier, so the
  frame reaches the 32 MB `MAX_CACHE_BYTES` limit sooner than `publisher.json`
  does (roughly ten thousand children across four tiers), far past the
  hundreds a channel holds. An oversized frame must fail loud: end the prefix
  track with `FrameTooLarge` instead of the existing tracks' debug log, which
  leaves readers on a stale snapshot. Test a prefix whose frame overflows.

Public API: `moq-stats` producer and consumer types. Wire: stats track names
and payloads.

## Related

- [Viewer lag histogram](/quest/m2/qos/lag-histogram.md) - lands on the totals
  and these prefix tracks
- [Publisher timeliness at relay ingest](/quest/m2/qos-publisher-timeliness.md) - the drift
  histogram lands on the same tracks
