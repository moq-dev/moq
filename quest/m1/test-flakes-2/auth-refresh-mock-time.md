# [XS] moq-relay auth refresh tests mock time

## Goal

The moq-relay auth tests that measure a grant's lifetime pass under any
load, because they read one mocked clock instead of the host's wall clock:

- `an_outage_refuses_transiently_and_never_extends_the_expiry` and
  `renewal_outlives_the_initial_credential_and_withdrawal_ends_only_its_grant`
  (`rs/moq-relay/src/auth/refresh.rs`)
- `a_fixed_lease_still_expires` and
  `an_outage_keeps_the_session_until_expires`
  (`rs/moq-relay/tests/auth_lifetime.rs`)

## Plan

These tests failed under load on 2026-10-13 (seen on #5148, #5462, and
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

- Read the grant's wall time through `moq_clock::wall()` in both
  `Grant::deadline()` and `Grant::validate()` (`rs/moq-auth/src/grant.rs`),
  which each compare `expires` with `SystemTime::now()` today, and the tests
  stamp `expires` from the `moq_clock::sim` clock, which moves wall and
  monotonic time together. Nothing reads the host clock.
- Paused Tokio time alone is not enough (review on #5474): it does not pause
  `SystemTime`, so under load a short grant can still expire before admission,
  and bracketing the bounds the way `an_outage_keeps_the_session_until_expires`
  does cannot stop that. That test also unwraps a wall expiry that may
  already be past, so it moves onto the mocked clock here too. Hence the
  existing moq-clock crate is used directly, rather than a paused-Tokio
  fallback. This quest owns the targeted moq-auth grant clock conversion;
  it does not wait for the whole workspace sweep or future relay seam.
- `a_fixed_lease_still_expires` moves off real sockets onto
  `memory_sessions()` and `moq_relay::supervise` under the same clock. If no
  other real-socket test proves the tcp and ws paths hand their lease to
  `supervise`, keep one real-socket case that asserts only the close, under a
  watchdog.

Reproduce first by delaying wall time between `grant()` and admission.

Public API: none. Wire: none.

## Related

- [Controlled time](/quest/m1/time/README.md) - the systemic fix: every crate on one controlled clock, and relay tests on the sim through [the relay seam](/quest/m1/time/relay-seam.md)
