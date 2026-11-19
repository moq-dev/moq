# [S] moq-tokio's fleet drain test holds under load

## Goal

`moq-tokio`'s `connection::tests::a_fleet_drain_redials_through_a_fresh_resolve`
passes however loaded the machine is.

## Plan

It failed once under a full `just check` on 2026-11-19
([moq-dev/moq#9154](https://github.com/moq-dev/moq/pull/9154)): the
subscription ended with `Session(GoawayTimeout)` instead of reading `g1`.
Alone and under 96 parallel copies it passed 816 of 816.

The test runs on real TCP and the wall clock: the drained session closes at a
500ms handover cap, and the test assumes the client dials the second relay and
moves the track onto it within that. Cutting the cap to 1ms reproduces the
failure every time, on `main` and on #9154 alike, so a loaded machine that
takes longer than 500ms to dial loses the race.

Recommended: drive the handover cap off a paused or manual clock (see this
epic's rules), or assert on the second session serving the track before the
cap may run out, rather than raising the cap.
