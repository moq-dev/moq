# [M] One route cost

## Goal

A route carries one static cost. Warm and Cold Route Cost, and the
cache-state accounting behind Warm, are gone from the next wip lite version
and from moq-net's model. lite-06 peers keep working.

## Plan

Decided in the 2026-09-30 wildcard audit (cache tiers): with configured edge
and core tiers and static core links, nothing switches routes on cache
state, so Warm has no job.

- The next wip lite version (`moq-lite-07-wip` while it is unpublished)
  carries a single route cost in ANNOUNCE_START and ANNOUNCE_UPDATE. Update
  `drafts/draft-lcurley-moq-lite.md` in the same PR.
- lite-06 sessions keep parsing both fields: read Warm as the cost and
  ignore Cold, and write the one cost as Warm with Cold at the saturation
  ceiling.
- The single cost still prices a standby claim above a live one, per the
  standby floor Wildcard landed in #4403. Update anything naming
  `Cost { warm, cold }`.
- Replace `Cost { warm, cold }` in `rs/moq-net/src/model/origin.rs` and its
  JS mirror, and delete what computes Warm from cache state.
- The moq-transport cluster extension already carries one cost.
- Once [Routes and announces](/quest/m1/cluster-routing/routes.md) lands, this
  single cost stays on ANNOUNCE as the origin's per-prefix seed and stops
  accumulating per hop; link costs move to the ROUTE metric.

Public API: `Cost` changes shape (a break). Wire: the
wip version drops a field; lite-06 is unchanged.
