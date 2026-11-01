# [XS] Every io_uring test runs in the nextest io-uring group

## Goal

Every test that opens an io_uring, including the relay's `wire_stats::uring_*`, `peer_limits::uring_*`, and `refusals::uring_*` integration tests and the `bitrate_caps::uring_*` tests [moq-dev/moq#7535](https://github.com/moq-dev/moq/pull/7535) adds, runs inside the `io-uring` nextest test group, so a parallel `just ci` no longer exhausts the user's shared `RLIMIT_MEMLOCK`. An io_uring test that still hits too low a limit fails with a message naming `RLIMIT_MEMLOCK` and how to raise it, not a generic setup error or a timeout.

## Plan

Found by [moq-dev/moq#7535](https://github.com/moq-dev/moq/pull/7535) and [moq-dev/moq#7211](https://github.com/moq-dev/moq/pull/7211): #7535's io_uring tests passed serially but could hit the shared limit in parallel, and #7211's `just ci` failed once in `uring_peer_session_widens_quic_limits` on `RLIMIT_MEMLOCK`.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m0, since each such failure fails a whole merge-gate batch.

- The group's filter in `.config/nextest.toml` names only `runtime_uring::` and `embed::.*uring` in `moq-relay::integration`. Match every io_uring test instead, for example `test(/uring/)` in that binary plus moq-relay's own unit tests that start ring workers, so a new test joins by its name. Check that the inventory of uring tests (`rg 'fn uring_' rs/moq-relay`) all land in the group with `cargo nextest list`.
- moq-uring already names the limit on a ring's `ENOMEM` (`rs/moq-uring/src/error.rs`). Check that this error reaches the failing test's output on the relay paths (worker startup, `start_relay`) instead of being wrapped or swallowed; if a test sees only a timeout, surface the startup error there.
- Keep the group's `max-threads` unless measured otherwise; the limit is shared with other checkouts, which `rs/moq-uring/README.md` documents.

Public API: none. Wire: none.

## Related

- [More tests under load](/quest/m0/test-flakes-2/README.md) - the flake epic this would otherwise land in
