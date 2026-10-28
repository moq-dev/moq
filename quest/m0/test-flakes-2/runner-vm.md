# [S] Runner VM test under load

## Goal

`checks.x86_64-linux.github-runner-vm` (`nix/runner/test.nix`) passes in
every merge group it runs in, and a failed flake check prints the full log
of the failing derivation so the failing subtest is named.

## Plan

Decided 2026-10-28 (maintainer): an m0 flake quest, fixed at its cause.

- It failed once, in merge-group run
  [40609795466](https://github.com/moq-dev/moq/actions/runs/40609795466),
  which dropped [moq-dev/moq#6934](https://github.com/moq-dev/moq/pull/6934)
  from the queue. The PR did not change the derivation, and it passed when
  built locally (10 subtests, about 4 minutes).
- It rarely runs: in 18 earlier merge groups on `nixos-moq-ci` the result was
  already in the runner's store (`built=0`). This time it had been collected,
  so it rebuilt, probably alongside the heavy `check rs` and `test rs` work.
- `sh/dispatch.sh` runs `nix flake check` without `-L`, so the log kept only
  the last 25 lines (QEMU cleanup). First make a failed check print the full
  build log (`-L`, or `nix log` on failure), so the next failure names its
  subtest.
- Then reproduce under the same contention (rebuild with the store result
  removed while a Rust test run loads the host) and fix the subtest at its
  cause, not with a longer timeout.

Public API: none. Wire: none.
