# [S] Nightly's relay session benchmarks fit their hour

## Goal

`Nightly (Relay session benchmarks)` (`just gh nightly rust-relay`, a paired
group that measures this commit and the previous Nightly's) finishes inside
its 60-minute `timeout-minutes` on a scheduled run and keeps its paired
regression gate.

## Plan

The job was cancelled at the timeout on the 2026-11-08, 11-09, and 11-10
scheduled runs. Unlike the other cancelled jobs it does not hang:
[the 11-10 run](https://github.com/moq-dev/moq/actions/runs/42842827718/job/129922278379)
was still finishing `relay_session_join_broadcasts` cells seconds before the
cancel, so the sweep has outgrown the hour (it took 14 minutes in
[the m3 CI audit](/quest/m3/audit-ci.md)'s sample). Find what grew first, then
trim the sweep or split the group across matrix entries, as
[the Rust benchmark tests quest](/quest/m0/ci/nightly-rust-test-hour.md) does
for its job. The sweep still covers both fan-out axes.

## Related

- [Rust benchmark tests fit their hour](/quest/m0/ci/nightly-rust-test-hour.md) - the same timeout, caused by builds
