# [S] TS passthrough jump test holds under load

## Goal

`publish::tests::ts_passthrough_crosses_a_relay_through_a_flagged_jump`
(`rs/moq-cli`) passes under a loaded `just check`, fixed at its cause.

## Plan

Seen 2026-10-09 by three PRs on unmodified `main` (#5146, #5155, #5153): the
assertion "moq-transport-14: both copies crossed" fails, so the recording
held at most one copy of the fixture. It failed 2 of 3 loaded runs and
passes alone. The test came in with #5003. It paces its input with a 15 ms
wall-clock sleep and waits up to 10 s.

Decided 2026-10-09: find why the second copy goes missing under load before
changing anything, then replace the wall-clock pacing with mocked time or
observable events where the fixture allows. Never widen the wait or add a
retry, per this questline's rules.

Public API: none. Wire: none.

## Required

- [The moq-time crate](/quest/m1/time/crate.md) - the controlled clock and sim this tests on
