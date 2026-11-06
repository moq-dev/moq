# [M] moq-relay tests run on a mocked clock

## Goal

moq-relay's tests stop proving a session stayed open with
`timeout(200ms, session.closed()).is_err()` on the real clock, and stop
sleeping to settle. See the [epic](/quest/m1/mock-clock-tests/README.md)
for the rules.

## Plan

As of 2026-11-06, 28 tests: `tests/integration/auth_lifetime.rs` 9,
`session_revalidate.rs` 4, `public_rules.rs` 3, `goaway_cluster.rs` 2,
`runtime_uring.rs` 2, `tenant_matrix.rs` 2, `web.rs` 2, and one each in
`cluster_unknown.rs`, `socket_auth.rs`, `src/auth.rs`, and `src/session.rs`.
Many run over real sockets: assert on the event the relay emits (a
revalidation, a close reason), or drive the deadline with the
[simulator or a `Manual` clock](/quest/m0/test-flakes-2/README.md), rather
than waiting out a window.

Public API: none. Wire: none.
