# [M] Consumers see the producer's real error, never Dropped

## Goal

A track or broadcast that ends because its source ended reports the source's
own error to every consumer, locally and across a relay. `Dropped` means only
that a handle was dropped without an end, which a correct producer never does.
The open #4179 fixes revoked upstream subscriptions. The merged #4120 preserves
session death and resumed-track errors; remaining paths still need verification.

## Plan

- Origin broadcasts now preserve an aborted source's cause through local and
  routed fronts, including a concurrent route withdrawal and later track lookup.
  A closed source's standing route is excluded from that front's failover.
- Rust maps IETF `PUBLISH_DONE` Unauthorized to `Error::Unauthorized`.
- After #4179 lands, verify the combined track paths locally and over
  mock sessions, and map JS `PUBLISH_DONE` Unauthorized to the shared error from
  #4179. Do not duplicate the resume changes those PRs own.

- Complete the remaining source-close, route-removal, and broadcast-withdrawal
  track regressions locally and over a mock session. Preserve causes at the
  source rather than remapping `Dropped` at consumers.

Public API: none expected; error values consumers observe change. Wire: none.

## Required

- [Unauthorized](/quest/m1/auth/unauthorized.md) - #4179 supplies shared Unauthorized errors and revoked-stream handling

## Related

- [#4179](https://github.com/moq-dev/moq/pull/4179) - owns the revoked-upstream path
