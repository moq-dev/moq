# [XS] Tests own their temp directories

## Goal

No Rust test writes to a fixed path under the system temp directory, so
concurrent test processes (parallel `just check` runs in separate worktrees,
CI shards) cannot overwrite each other's files.

## Plan

moq-relay's `config` and `cluster` tests and moq-bench's `config` and `stats`
tests write under fixed `std::env::temp_dir()` subdirectories such as
`moq-relay-config-test`. Found while landing
[moq-dev/moq#6986](https://github.com/moq-dev/moq/pull/6986).

Decided 2026-10-28 (maintainer): cover every fixed temp path in the
workspace, not just the three moq-relay tests first seen. Each test takes its
own `tempfile::tempdir()` (already a workspace dependency), which also cleans
up on drop. Paths already made unique per process or per test (a PID or
counter suffix) are out of scope.

Public API: none. Wire: none.
