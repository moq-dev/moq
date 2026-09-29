# [M] Sparse FETCH ranges

## Goal

A FETCH's cost follows the groups it can return, not the span of its range.
A track whose sequences are sparse, or whose history is long gone, answers a
wide range without spinning a worker or probing upstream once per missing
sequence.

## Plan

With no fetch handler, the standalone FETCH walk already skips a hole to the
next cached group, so a publisher answering from its own cache pays for the
groups it returns (`fetch/span` and `fetch/present` in `rs/moq-net/benches/fetch.rs`).

A relay with a handler still steps by one: every missing sequence below the
newest group is its own upstream FETCH. FETCH is authoritative, so a local
hole cannot be answered as absent. Resolve each run of local misses with one
upstream request instead, the way moxygen's `MoQCache` fetches one interval
per cache gap. That needs a range request on `track::Dynamic` (or
`group::Request`), a multi-group receive path in the IETF subscriber, and a
decision for moq-lite, whose FETCH names a single group. Settle the shape
with the maintainer first.

## Related

- [Moxygen compatibility](/quest/m1/moxygen/README.md) - the line whose FETCH walk this bounds
- [Fetch without SUBSCRIBE](/quest/m1/ietf-fetch-only.md) - also changes how a relay reaches upstream for fetches
