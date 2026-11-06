# [S] io_uring tests fit the shared locked-memory budget

## Goal

moq-relay's and moq-uring's io_uring tests stop failing ring setup with
`ENOMEM` on the user's shared `RLIMIT_MEMLOCK` when several test runs share
one host, as they do under parallel quest sessions. The fix works at the
cause, by bounding how much locked memory the tests take at once across
invocations or by sizing their rings to fit, not by raising the limit
blindly. Done when the PR shows concurrent runs that failed before now
passing on a host with the default 8 MiB limit.

## Plan

Added 2026-11-05 (maintainer, /quest-complete follow-ups).

What [moq-dev/moq#7593](https://github.com/moq-dev/moq/pull/7593) (merged
2026-11-02 17:04Z) did: it widened the `io-uring` nextest group in
`.config/nextest.toml` to every moq-relay test naming `uring` as a whole
segment, with `package(moq-uring)` already in it. It also made a refused
relay worker report its `RLIMIT_MEMLOCK` error through `Ready::wait`
instead of a timeout. It kept the group at `max-threads = 4`. Its quest
scoped out the rest on purpose: "The group caps rings only within one
nextest invocation; separate invocations still share the budget, as
`rs/moq-uring/README.md` documents, and this quest does not change that."
It gives no reason beyond that scope. A nextest group also does nothing
under plain `cargo test`, which the land fixers run.

So the budget is still shared by:

- separate nextest invocations from the same user (several checkouts or
  sessions running `just check` at once), each allowed four ring tests;
- plain `cargo test`, which runs moq-uring's and moq-relay's ring tests in
  parallel with no group at all.

`sh/rs/nextest.sh` raises hosted CI to 64 MiB, and no CI or merge-queue run
from 2026-11-03 to 2026-11-06 failed on it. The one io_uring failure in that
window,
[run 41736459964](https://github.com/moq-dev/moq/actions/runs/41736459964)'s
`embed::uring_custom_route_and_quic`, was the socket-ownership check
[moq-dev/moq#7823](https://github.com/moq-dev/moq/pull/7823) fixed. The
failures below are local runs, on hosts with an 8 MiB limit where the
report names it, and all but the last are on bases that include #7593:

- [moq-dev/moq#7951](https://github.com/moq-dev/moq/pull/7951) (PR body,
  2026-11-05): two `quest gate just ci` runs on `eb0c56312b` "failed only in
  io_uring tests in moq-relay and moq-uring: `io_uring setup failed: Cannot
  allocate memory ... RLIMIT_MEMLOCK (8192 KiB), shared by every process of
  this user`". The third run passed unchanged.
- [moq-dev/moq#7375](https://github.com/moq-dev/moq/pull/7375) (PR body,
  merged 2026-11-03): `quest gate just check` failed once each in
  `bitrate_caps::uring_session_is_held_to_its_caps` and
  `uring::tests::a_group_stopped_before_serving_joins`, both `ENOMEM` on the
  8 MiB limit.
- [moq-dev/moq#7313](https://github.com/moq-dev/moq/pull/7313) (PR body,
  merged 2026-11-03): a moq-relay uring test in `just check`. Then 11
  moq-uring integration tests failed in a single
  `just rs test --workspace --no-fail-fast`. One invocation stays within its
  four-test group, so other processes of the same user must have held part
  of the budget.
- [moq-dev/moq#7508](https://github.com/moq-dev/moq/pull/7508) (PR body,
  merged 2026-11-03): `moq-uring::integration mtu::discovery_finds_a_larger_mtu`
  with `Cannot allocate memory`. The PR doesn't say whether that run
  included #7593.

Reproduce first: on a host with an 8 MiB limit, run two or three
`just rs test -p moq-uring` or relay `--features io-uring` invocations at
once (and one plain `cargo test`) until a ring setup fails. Then measure what
one ring test locks. `rs/moq-uring/src/worker.rs` counts 16 bytes per CQ
entry, but the registered buffer rings (`register_buf_ring_with_flags` in
`udp.rs`) may be most of it. Integration tests keep the production
256/2048 geometry, while unit tests use 128/512.

Choose the fix from that measurement and prefer the simplest. Options
include serializing ring tests across invocations through a user-wide lock
that both nextest and `cargo test` respect, or sizing the test rings and
buffer pools so the worst-case concurrency fits. Raise the limit only with
a measured reason, not as the fix.

Public API: none. Wire: none.
