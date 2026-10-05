# [L] Stats totals and per-broadcast tracks

## Goal

Each node's stats broadcast publishes per-group cumulative totals that are
never pruned, plus one stats track per broadcast, served only when requested.
The per-path map tracks are gone. A reader that misses a group loses nothing:
the next totals frame still carries everything since the epoch began.

## Plan

Decided 2026-10-05 (planned from moq-dev/moq.pro#2202):

- **Why.** Every counter is cumulative per entry, and an entry is pruned right
  after the frame carrying its closing readout. A broadcast that starts and
  ends inside a group a reader missed shows in no later frame, so a lagging
  reader undercounts. The maps also grow with every live broadcast, and
  readers like billing want only per-project sums.
- **Totals.** Per group broadcast (`<prefix>[/<group>]/node/<node>/@<epoch>`),
  per tier and role, cumulative within the [stats epoch](/quest/m0/broadcast-epoch/stats-epoch.md).
  A node starts a new epoch on every startup and counts from zero; nothing is
  serialized to disk. `Registry`'s unpruned lifetime totals
  (`rs/moq-net/src/stats.rs`) are node-wide, per tier and role only, so they
  can't be published as-is: keep the totals per group, folding an entry into
  its group's total when it is pruned, or two projects sharing a tier would
  merge.
- A group's totals outlive its entries. Today a group broadcast disappears
  when it has none (`rs/moq-stats/src/produce.rs`); its totals must survive
  that, so a group that goes idle and returns in the same epoch continues
  from where it stopped. Test two groups sharing a tier, and an idle group
  returning.
- The totals map grows with every distinct group a node sees in its epoch.
  Accept that: an entry is a few counters per tier and role, groups are
  projects, and a restart starts a new epoch from empty.
- **Per-broadcast tracks.** A reader that wants one broadcast subscribes to
  its track; nothing is produced for a broadcast no one requests. The track
  carries that broadcast's cumulative counters and finishes after its closing
  readout. Pick the track naming while implementing, but it must be
  injective: tiers and broadcast paths both contain `/`, so no plain
  concatenation of the two parses back to one pair.
- A broadcast's counters start at zero. A request for a broadcast with no
  entry (never seen, or already pruned) is held open with zeroed counters
  until one appears, as an unrecorded tier is today, and is reclaimed when
  its last consumer leaves.
- The tier-sized caps (`MAX_REQUESTED_TRACKS` is 64) are too small for a page
  of visible broadcasts. Give per-broadcast tracks their own cap, sized for
  one page, and refuse requests beyond it so the reader sees an error rather
  than a parked track.
- **Retire the map tracks** (`publisher.json`, `subscriber.json`, and their
  `.json.z` siblings) in the same release.
- Ship in the same breaking release as stats epochs, so consumers take one
  stats path and wire change, not two.
- Update the aggregator and `doc/concept/stats.md` with the change.

Open:

- **Idle groups.** A group broadcast still unannounces once it has no
  entries, so a reader that misses its last totals frame lacks those
  increments until the group returns, contradicting the Goal. Either keep a
  group with nonzero totals announced for the whole epoch, or keep the
  unannounce (after [#4843](https://github.com/moq-dev/moq/pull/4843)'s
  linger) and soften the Goal.
- **`sessions.json`.** It is keyed by auth root and loses pruned roots the
  same way. Either fold per-tier `Presence` into the totals and retire it
  with the other maps, or keep it as is.

Public API: `moq-stats` producer and consumer types. Wire: stats track names
and payloads.

MoQ Pro adopts it when it pins the release: billing reads totals, its
Broadcasts page subscribes per visible broadcast, and its `announced` probe
reads totals only.

## Required

- [Stats epochs](/quest/m0/broadcast-epoch/stats-epoch.md) - totals are cumulative within an epoch

## Related

- [Bounded stats aggregate](/quest/m0/broadcast-epoch/stats-aggregate-bound.md) - retired
  nodes fold into a bounded total; per-epoch totals feed it
- [Media stats](/quest/m1/stats/README.md) - publisher and viewer media stats
  stay hang tracks, separate from the relay's stats
