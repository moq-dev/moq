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
  per tier and role, cumulative within the [stats epoch](/quest/m1/stats-epoch.md).
  A node starts a new epoch on every startup and counts from zero; nothing is
  serialized to disk. `Registry` already keeps unpruned lifetime totals
  (`rs/moq-net/src/stats.rs`); publish them per group.
- **Per-broadcast tracks.** A reader that wants one broadcast subscribes to
  its track; nothing is produced for a broadcast no one requests. The track
  carries that broadcast's cumulative counters and finishes after its closing
  readout. Pick the track naming while implementing; the existing
  `MAX_REQUESTED_TRACKS` and `MAX_PARKED_REQUESTS` caps apply.
- **Retire the map tracks** (`publisher.json`, `subscriber.json`, and their
  `.json.z` siblings) in the same release.
- Ship in the same breaking release as stats epochs, so consumers take one
  stats path and wire change, not two.
- Update the aggregator and `doc/concept/stats.md` with the change.

Public API: `moq-stats` producer and consumer types. Wire: stats track names
and payloads.

MoQ Pro adopts it when it pins the release: billing reads totals, its
Broadcasts page subscribes per visible broadcast, and its `announced` probe
reads totals only.

## Required

- [Stats epochs](/quest/m1/stats-epoch.md) - totals are cumulative within an epoch

## Related

- [Bounded stats aggregate](/quest/m1/stats-aggregate-bound.md) - retired
  nodes fold into a bounded total; per-epoch totals feed it
- [Media stats](/quest/m1/stats/README.md) - publisher and viewer media stats
  stay hang tracks, separate from the relay's stats
