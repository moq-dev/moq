# [XS] Auth refresh measures a grant's life on one clock

## Goal

moq-relay's `an_outage_refuses_transiently_and_never_extends_the_expiry`
(`rs/moq-relay/src/auth/refresh.rs`) passes under a loaded `just check`,
fixed at its cause.

## Plan

Seen 2026-10-13 on #5446's first `just check`: the session lived 28.862 s
against the test's 29 s floor, then the test passed alone and on rerun.

The test's `grant()` stamps the expiry with `SystemTime::now() + 30s`, but
`Grant::deadline()` (`rs/moq-auth/src/grant.rs`) converts the remaining wall
time onto Tokio's paused clock only when the lease snapshots it. Wall time
keeps running under load while the paused clock does not, so a 1.14 s
scheduling delay before the snapshot leaves 28.86 s on the Tokio clock.
Moving `start` earlier does not fix that; `sessions()` uses an in-memory
duplex, so no real socket is involved.

Measure against one clock: assert the session ends at the deadline the lease
captured, or build the grant's expiry from the moment that deadline is
taken, keeping the bounds. Reproduce first by delaying wall time between
`grant()` and admission.

Public API: none. Wire: none.
