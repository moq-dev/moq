# [S] Plan per-track stats tracks

## Goal

Decide whether a media publisher's stats ride one track per catalog track
instead of one entry per broadcast, and rewrite the stats quests to match.
This quest writes quests, not code.

## Plan

Run `/quest-plan` against this line and #4145. Raised while planning TR 101
290 monitoring (#4496): `hang::Stats { audio, video, transport }` sums every
rendition of a kind, so a simulcast ladder is indistinguishable, and a
per-rendition map would repeat the catalog's structure in a parallel type.

The proposal: a catalog track gains `stats: "video/hd.stats"`, a separate track
served on request, and a container section does the same (`mpegts.stats` from
the catalog's `mpegts` section). Stats stay off the catalog track itself,
which would churn every viewer's catalog on each interval.

For it: no parallel structure, a dashboard subscribes only to the renditions
it watches and an unrequested track costs nothing, each section owns its
counters with no flattened extension or `Merge` wrapper, and moq-mux
importers already count per track.

Against it, and what the session must answer:

1. Subscriber reports: a viewer cannot be referenced from the publisher's
   catalog, so its received, decoded and stalled counters still need its own
   `.stats` broadcast. Does that broadcast mirror per-track tracks, or keep the
   moq-stats entry?
2. The relay is media-agnostic and keeps moq-stats' per-broadcast `Traffic`;
   is splitting media publishers onto a second mechanism acceptable against
   this line's "one consumer reads both halves" goal?
3. Aggregation: N broadcasts × M tracks subscriptions against one per node.
4. If moq-stats entries stay, key media counters per track name rather than
   per kind.

## Related

- [Schema and library](/quest/m1/qos/stats/schema.md) - the shape this may replace
- [TS health stats](/quest/m2/ts-health-stats.md) - waits on this for its section's shape
