# [M] 64-bit Hop IDs

## Goal

A moq-transport peer that declares or advertises any non-zero 64-bit Hop ID,
as draft-lcurley-moq-cluster lets it pick at random, keeps its session: today
`Hop::from_wire` refuses anything at or above 2^62 with `invalid value`, so
about three in four moqx `main` relays can't connect on draft 18. Hop IDs are
`u64` end to end in Rust and JS, on every wire that can carry them.

## Plan

✅ Decided 2026-10-23 (maintainer, importing #6545): widen `Hop` to 64 bits
rather than cap the draft at 2^62 - 1. Why: the cluster draft already encodes
Hop IDs as vi64 and promises a 64-bit space, and a fold into 62 bits breaks
loop detection, since a relay never recognizes its own folded ID.

✅ Decided 2026-10-23 (maintainer): a lite-01..06 link, whose QUIC varint
stops at 2^62 - 1, carries a hop it cannot encode as 0, the reserved unknown
hop. Why: the route still reaches the old peer; loop detection through that
hop is lost on that link only, and `max_hops` and path cost still bound it.

- `moq-lite-07-wip` and IETF draft 17+ already use the leading-ones varint up
  to `u64::MAX`; no wire layout changes. `Hop::from_wire` and `Hop::new`
  accept any `u64`, and only the QUIC-varint encoders map an oversized hop
  to 0. Our own hop declaration on an old lite link follows the same rule.
- `Hop::random` keeps its 2^53 range so older `@moq/lite` clients, which read
  the exclude hop as a JavaScript number, still accept our IDs.
- `js/net` carries hop IDs as `bigint` wherever it decodes or compares them.
- draft-lcurley-moq-cluster and draft-lcurley-moq-lite say Hop IDs span 64
  bits and that a link whose varint can't carry one sends 0 in its place.
- Test: a draft-18 session with a peer Hop ID of `u64::MAX` connects, and a
  path holding one crosses a lite-06 link as 0.

Public API: `Hop::new` accepts the full `u64` range (additive). Wire: none on
current versions; old lite links gain the 0 substitution.

## Closes

- [#6545](https://github.com/moq-dev/moq/issues/6545) - moq-net refuses a HOP_ID at or above 2^62
