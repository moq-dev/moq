# [S] Decide how a redundant publisher pair fails over

## Goal

The maintainer picks how a broadcast survives losing one of two redundant
publishers, and the planned TS redundancy work is kept, rewritten, or deleted
to match. The two candidates:

- **Active-active:** both publishers share one epoch and derive identical
  groups, timestamps, and catalogs from the epoch and the source, so a relay
  resumes a subscription across them mid-group. This is what the m2 chains
  below build.
- **Switchover:** each publisher mints its own epoch, the newest route wins
  the path, and viewers follow the new epoch as a fresh instance with a
  bounded gap, on the routing and takeover work already in m1.

## Plan

Reopened 2026-10-19 (maintainer): active-active was decided on 2026-10-13
while planning #5412's leftovers. It is now challenged on whether the feature
is worth supporting at all, not only on how to build it.

Gather the facts, then ask the maintainer with a recommendation:

- What each costs: the code and invariants active-active needs (epoch-derived
  clocks and eras, deterministic group cuts, identical catalogs, and their
  tests) versus what switchover adds on top of
  [announcement takeover](/quest/m1/announce-takeover.md) and
  [cluster routing](/quest/m1/cluster-routing/README.md).
- What a viewer sees on failover under each, measured on the bufferbloat
  harness: gap length, and whether playback recovers without a reload.
- Which consumers need hitless output (ST 2022-7 style merging of identical
  legs) that switchover cannot give, and whether one exists today.
- What breaks if the source feeds the two publishers differently (a forked
  encoder, a dropped packet on one side), since active-active silently
  assumes identical input.

Delete or rewrite every quest that depends on this one once it is decided,
and update the identity guide if the contract changes.

## Related

- [Feature review](/quest/m2/release/feature-review.md) - re-judges the rest of the TS work before the cut
