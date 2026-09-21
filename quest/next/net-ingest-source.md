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

The fact is origin bookkeeping, not a chain fact. Within an origin the
origin's own hop is the implicit last hop of every route: on lite-05 and
later the sender reports its id once through ANNOUNCE_OK and the receiver
appends it, and `outgoing` drops any chain that already names the sender as
a reflection. Stamping the local hop on ingest would therefore drop every
locally ingested broadcast at the forwarding step, or name the relay twice
downstream. So:

- The origin records which announce producer inserted each route (a
  session, an in-process producer, or a peer session). The event field
  `source: Source::{Local, Peer(Hop)}` extends the announce event shape
  landed in [#3770](https://github.com/moq-dev/moq/pull/3770). This quest
  fills it in.
- `origin::Consumer::local()` is a view of the routes that entered here, so
  the ingest filter is one call.
- A sidecar reading over the wire sees `[.., x, relay]` and cannot tell a
  client `x` from a peer `x`; the relay publishes its cluster peer set in
  its stats track so a wire consumer can, and moq.pro's Python sidecar reads
  that instead of a bit prefix.

Public API: additive on moq-net and @moq/net. Wire: the relay's stats track gains a
cluster peer-set frame; name its shape in `doc/bin/relay/config.md` (stats
section) in the same PR so the Python sidecar and the producer agree.

## Required

- [Merge dev](/quest/dev/merge-dev.md) - starts on main

## Related

- [Route cost](/quest/next/route-cost.md) - the other route fact JS lacks
