# [XS] Tests own their temp directories

## Goal

No Rust test writes to a fixed path under the system temp directory, so
concurrent test processes (parallel `just check` runs in separate worktrees,
CI shards) cannot overwrite each other's files.

## Plan

The implementation is in [moq-dev/moq#7260](https://github.com/moq-dev/moq/pull/7260).
Finish local validation before marking it ready. `quest gate just ci` is
blocked by mbx failing to launch Cargo with `Argument list too long` (Rust
lint and the gate's lint regression), and the flake runner check failing
`unshare` with `Operation not permitted` while writing `/proc/self/uid_map`.
Resolve the tooling/environment blockers, rerun the gate, then remove this
quest and its links. The affected crates' default-feature tests pass. The
two changed `cluster-lan` fixture tests still need validation; their gated
command was stopped before it started while host capacity was unavailable.

Decided 2026-10-28 (maintainer): cover every fixed temp path in the
workspace, not just the three moq-relay tests first seen. Each test takes its
own `tempfile::tempdir()` (already a workspace dependency), which also cleans
up on drop. Paths already made unique per process or per test (a PID or
counter suffix) are out of scope.

Public API: none. Wire: none.
