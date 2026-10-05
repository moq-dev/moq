# [L] Stats totals and per-broadcast tracks

## Goal

Each node's stats broadcast publishes per-group cumulative totals that are
never pruned while the group is announced, plus one stats track per broadcast, served only when requested.
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
- **Idle groups** (decided 2026-10-05). A group broadcast already stays
  announced for the stats linger (`produce::Config::linger`, landed in
  [#4871](https://github.com/moq-dev/moq/pull/4871)) after its last entry
  ends. Today a path that left drops out of frames while the group lingers;
  with totals, a group that returns within the linger continues its totals. A zero linger is valid: a returning group then always
  takes a new epoch. After the linger it unannounces and drops its
  totals; a return announces under a new [epoch](/quest/m0/broadcast-epoch/stats-epoch.md)
  counted from zero. Totals are cumulative and a lingering group's frames all
  repeat its final totals, so a reader that misses every frame across the
  linger loses that epoch's tail; billing under-bills by that tail,
  consistent with the 0-bill baseline. Document that. Memory is bounded by active and
  lingering groups. Test two groups sharing a tier, a group returning within
  the linger (same epoch, totals continue), and one returning after it (new
  epoch from zero).
- **Per-broadcast tracks.** A reader that wants one broadcast subscribes to
  its track; nothing is produced for a broadcast no one requests. The track
  carries that broadcast's cumulative counters and finishes after its closing
  readout. A request after the closing readout gets the zeroed track below,
  so a zeroed per-broadcast track is not authoritative; totals carry the
  real sums. Pick the track naming while implementing, but it must be
  injective: tiers and broadcast paths both contain `/`, so no plain
  concatenation of the two parses back to one pair.
- A broadcast's counters start at zero. A request for a broadcast with no
  entry (never seen, or already pruned) is held open with zeroed counters
  until one appears, as an unrecorded tier is today, and is reclaimed when
  its last consumer leaves.
- The tier-sized caps (`MAX_REQUESTED_TRACKS` is 64) are too small for a page
  of visible broadcasts. Give per-broadcast tracks their own cap per group
  broadcast, sized (or configurable) for every reader the aggregator fans in,
  not one page. Refuse requests beyond it with a typed error the reader
  retries, rather than a parked track that reads as zero. Tier requests park
  instead (`MAX_PARKED_REQUESTS`) because a collector treats a refusal as
  final; per-broadcast readers must not.
- **Sessions** (decided 2026-10-05). `sessions.json` is keyed by auth root
  and loses pruned roots the same way, so fold per-tier `Presence` into the
  totals (session counts) and serve per-root detail as a requested track,
  the same model as per-broadcast tracks.
- **Retire the map tracks** (`publisher.json`, `subscriber.json`,
  `sessions.json`, and their `.json.z` siblings) in the same release. Decide
  while implementing whether totals and per-broadcast tracks keep `.json.z`
  siblings.
- Ship in the same breaking release as stats epochs, so consumers take one
  stats path and wire change, not two.
- Update the aggregator, `doc/concept/stats.md`, and `demo/web/src/stats.ts`
  (which reads the three maps) with the change. The
  aggregator merges totals across nodes, and merges one broadcast's track
  across nodes when a reader requests it, so a multi-node consumer serves the
  same format it reads.

Public API: `moq-stats` producer and consumer types. Wire: stats track names
and payloads.

MoQ Pro adopts it when it pins the release: billing reads totals, its
Broadcasts page subscribes per visible broadcast, its `announced` probe
reads totals only, and its customer stats feed serves this format summed
across nodes.

## Required

- [Stats epochs](/quest/m0/broadcast-epoch/stats-epoch.md) - totals are cumulative within an epoch

## Related

- [Stats linger](/quest/m0/stats-linger.md) - the idle-group window these totals continue across
- [Media stats](/quest/m1/stats/README.md) - publisher and viewer media stats
  stay hang tracks, separate from the relay's stats
