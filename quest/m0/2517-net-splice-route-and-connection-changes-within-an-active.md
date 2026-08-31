# [M] net: splice route and connection changes within an active group

## Goal

Implement and verify the behavior tracked in [#2517](https://github.com/moq-dev/moq/issues/2517)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

Connection migration from #2241 splices per-session tracks at group boundaries. `resume::Producer::takeover` starts the replacement segment one sequence after the newest group produced by the old route.

That works for segmented media, but not for tracks whose current group can remain open indefinitely:

- `moq-json::stream` intentionally carries the entire append log in one group.
- `moq-json::snapshot`, including catalog tracks, keeps a snapshot group open while appending deltas. A quiet catalog may never roll to another group.

If the serving route or connection disappears after part of such a group has been delivered, the logical subscriber cannot make progress until a later group appears. For a single-group JSON stream that later group never exists. For a catalog it may not appear for a long time. Route migration therefore is not transparent for these tracks.

This is separate from discovering and selecting an alternate route (#2461). Even when a replacement route is available and selected, the current splice granularity prevents it from continuing an active long-lived group.

#### Goal

Allow a logical track to switch routes or connections within an active group, resuming at a frame boundary instead of waiting for the next group sequence.

The model should preserve the logical group sequence and continue from the first missing frame without surfacing duplicate frames or a route-change error to the application. The design also needs to account for compressed catalog and JSON tracks, whose DEFLATE state spans frames within the group.

#### Acceptance criteria

- A route or connection can fail after frame N of group G, and an existing logical subscriber continues group G from a replacement route.
- Frames already surfaced before the switch are not surfaced again.
- `moq-json::stream` continues after migration even though the track uses one group for its lifetime.
- Catalog snapshot/delta tracks continue after migration without waiting for the publisher to roll the group.
- Compressed `.json.z` variants preserve decoder continuity or resume from an explicit safe point.
- Add model-level regression coverage for a mid-group takeover and end-to-end coverage that drops the serving connection while consuming a long-lived JSON or catalog group.

#### References

- \#2241 introduced transparent connection migration with group-boundary splicing.
- `rs/moq-net/src/model/resume.rs` owns the current group-segment splice.
- `rs/moq-json/src/stream.rs` keeps the whole log in one group.
- `rs/moq-json/src/snapshot.rs` keeps snapshot groups open for deltas.

## Closes

- [#2517](https://github.com/moq-dev/moq/issues/2517) - close this issue when the quest finishes
