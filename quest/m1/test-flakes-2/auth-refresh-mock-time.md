# [XS] moq-relay auth refresh tests mock time

## Goal

Three moq-relay auth tests that measure a grant's lifetime pass under any
load, because they read one mocked clock instead of the host's wall clock:

- `an_outage_refuses_transiently_and_never_extends_the_expiry` and
  `renewal_outlives_the_initial_credential_and_withdrawal_ends_only_its_grant`
  (`rs/moq-relay/src/auth/refresh.rs`)
- `a_fixed_lease_still_expires` (`rs/moq-relay/tests/auth_lifetime.rs`)

## Plan

All three failed under load on 2026-10-13 (seen on #5148, #5462, and
#5446) and passed alone. One run reported the session outliving its grant
at 28.5 s against the outage test's 29 s floor. AGENTS.md requires unit
tests to mock time.

Cause: a grant's `expires` is `SystemTime::now() + N`, but
`Grant::deadline()` (`rs/moq-auth/src/grant.rs`) converts the remaining wall
time onto Tokio's clock only when the lease snapshots it. The refresh tests
already run on paused Tokio time, which stops while wall time keeps running,
so a scheduling delay before the snapshot shortens the grant on the Tokio
clock. `a_fixed_lease_still_expires` runs on real sockets and real time with
a 1 s grant, so a slow connect and round trip races the expiry.

Decided (maintainer, 2026-10-13): mock the clock so the tests are
deterministic, rather than widen a bound.

- If moq-clock ([#5465](https://github.com/moq-dev/moq/pull/5465)) has
  landed, read the grant's wall time through it (`Grant::deadline()` and
  `Grant::valid()`), and the tests stamp `expires` from its simulated clock,
  which moves wall and monotonic time together. Nothing reads the host clock.
- Otherwise stay on paused Tokio time and take the wall reading on the
  paused clock's terms: bracket the snapshot the way
  `an_outage_keeps_the_session_until_expires` already does, so the bounds hold
  however long admission takes.
- `a_fixed_lease_still_expires` moves off real sockets onto
  `memory_sessions()` and `moq_relay::supervise` under the same clock, like
  `an_outage_keeps_the_session_until_expires`. If no other real-socket test
  proves the tcp and ws paths hand their lease to `supervise`, keep one
  real-socket case that asserts only the close, under a watchdog.

Reproduce first by delaying wall time between `grant()` and admission.

Public API: none. Wire: none.

## Related

- [Controlled time](/quest/m1/time/README.md) - the systemic fix: every crate on one controlled clock, and relay tests on the sim through [the relay seam](/quest/m1/time/relay-seam.md)
