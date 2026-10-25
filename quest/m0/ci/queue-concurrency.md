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

The runner VM module's `runner.count` registers several runners in one guest.
They share the home, so concurrent entries share mbx's store, its dedup of
in-flight compilations, and its compiler CPU and memory budget; each has its own
work directory. mbx documents concurrent builds on one store as supported, and
the self-hosted cache step keeps a version stamp per workspace.

Remaining:

- Confirm which runner is intended: the documented `nix/runner` VM, or the
  `container@moq-ci` systemd-nspawn container deployed on the host (see the
  [CI epic](/quest/m0/ci/README.md)). Give whichever stays two runners sharing
  its cache.
- Deploy with `runner.count = 2`, a fresh registration token, and a VM restart.
  The host also runs the `moq-pro` runner under the same `ci.slice` quota and a
  desktop; size the guest's `resources` so concurrent entries do not slow each
  other past the gain. Each entry runs 24 nextest workers.
- Raise the main ruleset's `max_entries_to_build` to the runner count.
- Measure merged PRs per hour and per-entry wall time before and after.

Public API: none. Wire: none.

## Related

- [Share local check capacity](/quest/m0/ci/local-gate.md) - admission for the same host
