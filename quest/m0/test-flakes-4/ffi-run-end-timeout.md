# [S] moq-ffi's run-end tests pass on a loaded host

## Goal

`server_client_run_end_draft18` and its siblings in `rs/moq-ffi/src/test.rs`
(`server_client_run_end` for lite-07 and moq-transport 18, 20, and 22) pass
every run of a loaded `just ci`, without a real-clock deadline deciding the
result.

## Plan

Found 2026-11-26 while gating
[moq-dev/moq#9665](https://github.com/moq-dev/moq/pull/9665):
`server_client_run_end_draft18` hit its 10 s `TIMEOUT` (`kio::clock::timeout`
on real sockets, `rs/moq-ffi/src/test.rs`) while other agents loaded the host,
and passed on rerun.

Decided 2026-11-26 (maintainer, /quest-run): ✅ add it to this epic (rejected:
dropping it). Reproduce it under load first (for example `stress-ng` beside
`cargo nextest run -p moq-ffi run_end` repeated), find which step waits past
10 s and why, and fix that per the epic's rules: an event or a paused or
`kio::clock::Manual` clock instead of the wall-time deadline, or the slow step
itself if the code under test is slow. Delete this quest if it no longer
reproduces on `main`, naming the PR that fixed it.
