# [M] Stats groups as tracks of one node broadcast

## Goal

A stats producer can publish one `<prefix>/node/<node>` broadcast per node
and carry each group as its own produced track, listed and withdrawn through
TRACKS, instead of announcing one broadcast per group per node. A reader
discovers a node's active groups with one Tracks stream and subscribes only
to the groups it wants. The node's announcement stays stable for the
producer's life, so group churn costs no announce traffic across a cluster.
The existing depth layouts keep their meaning.

## Plan

Decided 2026-10-22 (maintainer, from a downstream clustering audit): depth N
announces one broadcast per group per node, so every group that goes idle
past its linger and returns costs a broadcast end and start on every relay
in a cluster, and a reader following every group holds one subscription per
tier and role per group. TRACKS (lite-07-wip) already lists a broadcast's
produced tracks with live adds and removes, so groups become tracks.

- A new producer layout, selected by config beside `depth`. The broadcast
  path is depth 0's `<prefix>/node/<node>`. Each group is one produced track
  carrying every tier, role, and the sessions counts for that group, so a
  reader holds one subscription per group. Use the
  [compressed track](/quest/m1/compress/README.md) flag rather than a `.z`
  sibling if it has landed; otherwise keep today's encodings and say so.
- A group's track is listed when it first has traffic and removed after the
  linger, as its broadcast is today. A name listed again means the same
  content (TRACKS contract), so a reader must be able to tell continuation
  from a counter reset without double counting cumulative values. Settle
  and document which: counters kept across removal for the producer's life,
  or a reset the frame makes explicit.
- Frame size limits apply per group track, so one oversize group no longer
  ends every group's stats on that node.
- Discovery is TRACKS only. Over a hop that cannot carry TRACKS the listing
  is empty; the docs say so rather than adding a second index.
- `totals.json` and the existing depth layouts are unchanged. Removing depth
  N is a later decision, not this quest's.

Tests: groups appear and disappear in a listing through a relay; a group
removed and re-listed reads continuous or explicitly reset counters, never
double; an oversize group leaves other groups readable; a reader of one
group never receives another's entries.

Docs: `doc/concept/stats.md` gains the layout. Public API: a producer config
option. Wire: none beyond TRACKS.

## Related

- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - TRACKS ships in the finalized wire
