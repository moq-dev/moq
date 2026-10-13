# [XS] Auth refresh measures a grant's life from its admission

## Goal

moq-relay's `an_outage_refuses_transiently_and_never_extends_the_expiry`
(`rs/moq-relay/src/auth/refresh.rs`) passes under a loaded `just check`,
fixed at its cause.

## Plan

Seen 2026-10-13 on #5446's first `just check`: the session lived 28.862 s
against the test's 29 s floor, then the test passed alone and on rerun.

The test runs on a paused clock but takes `start` after `admission()` and
`sessions()`. The grant's 30 s starts at admission, and the paused clock
auto-advances whenever the runtime idles in between, so under load `start`
lands late and the measured life comes up short. Measure from the instant
the grant is admitted (take `start` before `admission()`, or read the
grant's own start), keeping the same bounds. Find what idles the runtime in
`sessions()`; if a real socket is involved, apply the epic's rules rather
than widening the window.

Public API: none. Wire: none.
