# [S] Verify shared local check admission

## Goal

Finish validating automatic host admission and bounded local worker fanout across
root, direct language, and fixture entrypoints, preserving selected tests,
failure status, and cancellation cleanup.

## Plan

- [Implementation PR](https://github.com/moq-dev/moq/pull/6256) adds shared
  admission before expensive builds and fixture launch, outside test timeout
  clocks. Nested work reuses admission and supervision; hosted CI keeps its
  existing scheduling. Rust compilation still uses mbx.
- Measured warm solo and paired checkouts with admission delay separated from
  work wall time and CPU time. Local nextest defaults to four workers, with
  the existing io-uring group limit retained. Bun workspace scripts run
  sequentially. The PR records the measurements and budget tradeoff.
- Launcher and dispatch tests verify hosted CI, nested and external admission,
  failure status, worker overrides, and cancellation of descendants. They run
  in the existing nightly tooling suite.
- Remaining: run the complete `just ci` gate on a host whose Nix runner-image
  builder can create the required user namespace. The current builder fails
  writing `/proc/self/uid_map` with `Operation not permitted`; other PRs report
  the same restriction, including [the listener tests](https://github.com/moq-dev/moq/pull/6234).
- The direct Rust test phase also stops at
  `stats::a_late_sink_reads_the_final_stats`, which fails with "the import
  exited before its input ended". The CLI source is unchanged and
  [the listener PR](https://github.com/moq-dev/moq/pull/6234) reproduces the
  failure on the base. Resolve that independently before the full gate can pass.
- Two existing nightly fixture assertions are owned by
  [the CI audit](https://github.com/moq-dev/moq/pull/6266).
