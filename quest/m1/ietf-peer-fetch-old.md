# [M] Cross-relay FETCH serves held groups on any peer link

## Goal

With the cluster peer link on moq-lite or moq-transport, a cross-relay FETCH
for a group the origin still holds is served, not refused as `old`. The burst
drill gains a moq-transport peer link as a third `PeerLink` variant, its
impaired lane passes, and the lite seed below no longer loses a group.

## Plan

Found by #4972: in `rs/moq-relay/tests/drills.rs::cross_cluster`, pinning the
edge's peer link with `config.connect.version = ["moq-transport-19"]` makes
the steady impaired drill fail 5 of 5 runs with FETCHes refused `old` for
groups the origin still held, e.g. `groups lost: [(5, Failed("old")),
(37, Failed("old")), (41, Failed("old"))]`. Seeds: 11977074354257116273,
6755536760138253279, 17984197590923068922, 10974424043511566981. The
loopback lane and the flapping drill passed.

Lite too (decided 2026-10-08, widening this quest rather than opening
another): [Cross-relay bursts](/quest/m1/cross-relay-bursts.md) saw
`bursts_cross_a_cluster` on a lite peer link, on `main` at `fe0113ff2`, lose
group 89 in 1 of 40 runs to a live `Old` reset whose FETCH was also refused
`old` (seed 11025055814286623932). It may share the moq-transport cause or
not; reproduce both before fixing either.

Facts (2026-10-07): the IETF FETCH path (`run_fetch_stream`,
`rs/moq-net/src/ietf/publisher.rs`) has no explicit `Old` check. An `Old` can
come from reading a group the cache's wall-clock expiry aborts mid-read
(`expire_closed`, `evict_expired_scan` in `model/track.rs`), and possibly
from a lite peer's `StreamError::Old` reset passing through (unconfirmed: no
caller chain into this FETCH path was traced). `Old` has no moq-transport
code, so it goes out as INTERNAL_ERROR and comes back as `Error::Remote`.

- Reproduce with the seeds on both peer link versions, find which path
  refuses, and fix it at the cause. A held group must not be refused.
- Keep the lite seed as a regression case beside the drill.
- Add the moq-transport peer link case to the drill.

Public API: none expected. Wire: none expected.

## Related

- [Cross-relay bursts re-run](/quest/m1/cross-relay-bursts.md) - the #4349 `Stream(Old)` stalls and the lite seed this quest now owns
