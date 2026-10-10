# [M] JS consumes use the identity of the serving announcement

## Goal

`@moq/net` requests name the epoch of the announcement serving their path,
including covering prefixes. A newly winning announcement never gives a new
consumer a cached broadcast from the previous source. Existing consumers
remain on their instance until they drop it or its source ends.

## Plan

Reproduced in the 2026-10-10 epoch audit in `js/net/src/lite/subscriber.ts`:

- Announce `pool` with epoch A, then consume `pool/job`: TRACK has no epoch
  because the subscriber looks up only the exact path. Rust already uses
  the resolved route's epoch.
- Consume `pool/job` through `pool`, then announce the more-specific
  `pool/job` with epoch B: another consume returns the old cached broadcast.
  Restart and End evict cached consumes, but the new Start does not.

Bind request identity and cache reuse to the serving announcement. Reuse the
existing route-resolution rules rather than adding another interpretation of
prefix precedence. A more-specific epochless announcement must not inherit
the epoch of a broader one. Inspect the shared `BroadcastCache` and IETF
subscriber for the same stale-cache behavior; IETF has no wire epoch today,
but a different epochless source still cannot reuse the old instance.

Add regressions for both failures, with exact-path requests as a control,
more-specific epochless routes, fallback after withdrawal, and old consumers
remaining readable while new consumers select the replacement. Same explicit
identity may reuse a broadcast; equal absent epochs do not prove identity.
Use the existing JS test lane and run cross-language interop. If routing or
invalidation work changes fanout costs, extend the relevant benchmark over
announcements and consumers instead of accepting a whole-table scan.

Decided 2026-10-10: these correctness fixes gate m0. Seamless JS track
handover remains separate in m1; fixing request identity does not require
building its resume pump. Update stale comments and existing concept docs
inline, with no separate guide.

Public API: corrected request and cache behavior, no new exported surface
expected. Wire: existing lite-07 epoch fields carry the resolved identity;
no framing change, and older versions stay epochless.

## Related

- [JS track handover](/quest/m1/js-group-handover.md) - preserves open tracks across routes of the same explicit identity
