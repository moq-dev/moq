# [XS] A clean close is never reported as a revocation

## Goal

A session that closes on its own is recorded as a session close, never as
`Unauthorized` with an application failure, even when the close also
withdraws every in-band credential at the same moment.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer.
`supervise` (`rs/moq-relay/src/connection.rs`) selects `session.closed()`
and `lease.ended()` without bias. A transport close also closes each in-band
credential's issued stream, so `Refresh::ended`
(`rs/moq-relay/src/auth/refresh.rs`) can report `Reason::Dropped` in the same
poll. If the lease wins, the relay aborts with `Unauthorized`
(`auth::error`) and `Lease::close` records `sessions_failed.application`,
contradicting `Lease::close`'s own rule that `Dropped` is an effect of the
close, not a verdict.

- Add `biased;` to that `select!` and poll `session.closed()` first.
- `Lease::close` folds in a non-`Dropped` reason from one poll of
  `Refresh::ended`, but `Credential::changed` polls `issued.closed()` ahead
  of expiry and `consumer.changed()`, so after a close it reports `Withdrawn`
  even when the decider's revocation is already pending. Poll expiry and
  `consumer.changed()` ahead of the withdrawal: a verdict the decider already
  gave outranks the close that withdrew the credential, and a credential that
  merely refreshed is still withdrawn on the next poll.
- A peer that withdraws its last credential on a live session still ends it
  as today.

Tests (mocked time, as the refresh tests do): a session whose only remaining
credential is in band closes cleanly, and the lease reports the session reason
and no application failure. A revocation of that credential already pending
when the transport closes is still recorded as the revocation, with its
failure kind.

Public API: none. Wire: none.

## Related

- [In-band auth](/quest/m1/auth/README.md) - owns the refresh and lease contract
