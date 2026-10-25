# [S] Build queue entries concurrently

## Goal

The merge queue builds two or more entries at once on self-hosted capacity, so
merged PRs per hour rise measurably over the 2026-10-23 baseline while every
entry still runs the same gate.

## Plan

Decided with the maintainer on 2026-10-23: structural queue changes come before
faster toolchains.

Measured 2026-10-23 over 32 merge-queue runs: `max_entries_to_build: 1` with one
`moq-queue` runner makes the queue fully serial, about 7 merged PRs per hour,
with a median entry of 135 s (p90 516 s) and 3 to 12 s of runner wait.

Decided 2026-10-25 (maintainer): no VM. The second runner goes into the
deployed `container@moq-ci` systemd-nspawn container, sandboxed by kernel
primitives (see the [CI epic](/quest/m0/ci/README.md)), not into the
`nix/runner` VM module. The container is defined in the host's own flake, not
in this repository, so registering the runner is a host change.

Each runner keeps its own work directory, since the nixpkgs runner service
empties it on start. The self-hosted cache step keeps its mbx version stamp per
workspace, so a runner sharing a home with another still drops its own
workspace's state after an mbx version change.

Decided 2026-10-25 (maintainer): ✅ one home under one static user for both
runners. Concurrent entries share mbx's store, wait on a compilation the other
entry is running instead of repeating it, and draw on one machine-wide compile
budget, which mbx documents as supported
([how it works](https://github.com/jdx/mr-boxington/blob/v1.25.0/docs/how-it-works.md#machine-wide-scheduling)).
nixpkgs gives each runner its own `DynamicUser` by default, so both need the
same `user` and `HOME`. Rejected: a home per runner (two cold caches, double
the disk, two mbx schedulers each budgeting the whole container's CPUs) and a
second container (those costs plus another container). A NixOS container
builds through the host's Nix daemon by default, so the Nix store is shared
either way.

Remaining (maintainer, on the host):

- Register a second runner in `container@moq-ci` with its own name and work
  directory, under the same static user and `HOME` as the first, with a fresh
  registration token.
- Size the container's share of `ci.slice`, which it shares with the `moq-pro`
  runner and a desktop, so concurrent entries do not slow each other past the
  gain. Queue entries skip `quest gate` (`sh/local.sh` execs directly under
  GitHub Actions), so the cgroup quota, not admission, keeps them apart.
- Keep `NEXTEST_TEST_THREADS=24` per entry (48 workers across two entries;
  mbx budgets compiles, not tests) and watch the first day for timing flakes
  rather than guess a lower number.
- Raise the main ruleset's `max_entries_to_build` from 1 to 2.
- Measure merged PRs per hour and per-entry wall time for a day against the
  baseline before going past 2.

Public API: none. Wire: none.
