# [M] An announce says whether the broadcast entered here

## Goal

A consumer of an origin knows whether a route came from a session on this
relay or from a peer, without decoding hop-id bit prefixes. moq.pro answers
"am I the ingest edge?" by reverse-engineering the hop chain in Rust
(`rs/edge/src/hop.rs`, 708 lines with tests pinning migration semantics)
and again in Python with the opposite predicate, and filters an origin down
to what this relay ingests with 734 lines of `Dynamic` mirrors
(`rs/edge/src/sync/ingest.rs`).

## Plan

The origin already knows which routes are local. Add
`source: Source::{Local, Peer(Hop)}` to the announce event settled in
[announce event](/quest/m1/api-net-announce.md), stamp the relay's own hop
on local ingest so a sidecar and an in-process worker read the same chain,
and give `origin::Consumer::local()` (a view of the routes that entered
here) so the ingest filter is one call. Decide whether a `Route` carries
the same fact for `routed_broadcast` callers.

Public API: additive on moq-net and @moq/net (a new event field). Wire:
none.

## Required

- [Announce event](/quest/m1/api-net-announce.md) - the event shape this extends
- [Merge dev](/quest/m1/merge-dev.md) - starts on main

## Related

- [Route cost](/quest/m2/route-cost.md) - the other route fact JS lacks
