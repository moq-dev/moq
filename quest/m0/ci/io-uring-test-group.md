# [XS] Every io_uring test runs in the nextest io-uring group

## Goal

Every test that opens an io_uring, including the relay's `wire_stats::uring_*`, `peer_limits::uring_*`, and `refusals::uring_*` integration tests and the `bitrate_caps::uring_*` tests [moq-dev/moq#7535](https://github.com/moq-dev/moq/pull/7535) adds, runs inside the `io-uring` nextest test group, so the tests of one `just ci` running in parallel no longer exhaust the user's shared `RLIMIT_MEMLOCK`. The group caps rings only within one nextest invocation; separate invocations still share the budget, as `rs/moq-uring/README.md` documents, and this quest does not change that. An io_uring test that still hits too low a limit fails with a message naming `RLIMIT_MEMLOCK` and how to raise it, not a generic setup error or a timeout.

## Plan

The implementation is checkpointed in draft [moq-dev/moq#7593](https://github.com/moq-dev/moq/pull/7593): name-based grouping covers the relay's integration and unit tests, and shared fixture startup preserves worker failures before clients connect. The four-test group limit is unchanged.

Remaining work is validation and local review:

- Run the full `just ci` successfully, then `/quest-review` and mark the claim PR ready. The current environment fails in Cargo documentation and `sh/rs/test-lints.test.py` with mbx `E2BIG`; the separate [mbx quest](/quest/m0/ci/mbx-arg-max.md) owns that root cause.
- Preserve the completed inventory check: `cargo nextest show-config test-groups` with `moq-relay/io-uring,moq-relay/qlog` lists all 19 named `uring_*` tests and all three ring-opening relay unit tests in `io-uring`. The pending `bitrate_caps::uring_*` names from [moq-dev/moq#7535](https://github.com/moq-dev/moq/pull/7535) match the same filter.
- The full relay suite with `io-uring,qlog` passes: 437 tests, 9 ignored. A constrained-memory reproduction previously reported only `relay stopped before becoming ready: channel closed`; the new isolated-process test exercises the real worker startup and verifies `RLIMIT_MEMLOCK`, `ulimit -l`, and `LimitMEMLOCK=` survive the error chain.

Public API: none. Wire: none.

## Related

- [More tests under load](/quest/m0/test-flakes-2/README.md) - the flake epic this would otherwise land in
