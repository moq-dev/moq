# [S] The FFI concurrent-ready test wakes both calls under load

## Goal

`concurrent_ready_calls_all_wake` in `rs/moq-ffi/src/test.rs` passes
reliably in loaded runs, fixed at its cause under the
[epic](/quest/m0/test-flakes-2/README.md)'s rules, and its `#[ignore]` and
`sh/gh/test-inventory.allow` entry are removed. Done when the PR explains
the stall and shows the loaded run that reproduced it now passing.

## Plan

Added 2026-11-06 (maintainer's standing rule that flakes join this epic,
`/quest-complete`). The `/quest-iterate` agent on
[moq-dev/moq#7981](https://github.com/moq-dev/moq/pull/7981) saw it time out
in a local `quest gate just ci` while draining the reader during the two
concurrent `ready` calls; it passed on unchanged `main`, and #7981 doesn't
touch moq-ffi. Quarantined in
[moq-dev/moq#8024](https://github.com/moq-dev/moq/pull/8024) with every
assertion kept.

Lead, unverified: the drain loop reads until both parked `ready` calls have
finished, but it checks them only between reads. If the reader takes the
last of the 25 frames before both calls finish (their wakes run on the FFI
runtime, which lags under load), the next `read_frame` waits on a group that
gets no more frames and never finishes, so it times out as "the read
stalled". Check whether that is the failure before choosing between waiting
on the calls once the window has room and fixing a lost wake in the group.

Reproduce first: run the test repeatedly with `--run-ignored all` beside
`just check --all` load under `quest gate`, and keep the failing output.

Public API: none. Wire: none.
