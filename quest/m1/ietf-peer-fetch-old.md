# [M] Cross-relay FETCH over moq-transport serves held groups

## Goal

With the cluster peer link on moq-transport, a cross-relay FETCH for a group
the origin still holds is served, not refused as `old`. The burst drill then
gains a moq-transport peer link as a third case and passes it.

## Plan

Found by #4972: with the edge pinned to `moq-transport-19`, the existing
steady impaired drill (`bursts_cross_a_cluster`, `rs/moq-relay/tests/drills.rs`)
fails 5 of 5 runs. Seeds: 11977074354257116273, 6755536760138253279,
17984197590923068922, 10974424043511566981.

Facts (2026-10-07): the IETF FETCH path (`run_fetch_stream`,
`rs/moq-net/src/ietf/publisher.rs`) has no explicit `Old` check. An `Old` can
come from reading a group the cache's wall-clock expiry aborts mid-read
(`expire_closed`, `evict_expired_scan` in `model/track.rs`), and possibly
from a lite peer's `StreamError::Old` reset passing through (unconfirmed: no
caller chain into this FETCH path was traced). `Old` has no moq-transport
code, so it goes out as INTERNAL_ERROR and comes back as `Error::Remote`.

- Reproduce with the seeds, find which path refuses, and fix it at the
  cause. A held group must not be refused.
- Add the moq-transport peer link case to the drill.

Public API: none expected. Wire: none expected.

## Related

- [Flapping peer drill](/quest/m1/cross-relay-flap-drill.md) - the drill this extends
- [Cross-relay bursts re-run](/quest/m1/cross-relay-bursts.md) - the field report of lost cross-relay groups
