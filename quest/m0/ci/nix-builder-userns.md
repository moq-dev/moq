# [S] The CI host's Nix builders may map user namespaces

## Goal

On the maintainer's CI host, `check flake` runs its user-namespace checks
instead of skipping, so `just ci` there covers the runner VM image tests.

## Plan

Decided 2026-11-05 (maintainer, follow-up seen in
[moq-dev/moq#7918](https://github.com/moq-dev/moq/pull/7918),
[moq-dev/moq#7684](https://github.com/moq-dev/moq/pull/7684),
[moq-dev/moq#7858](https://github.com/moq-dev/moq/pull/7858), and
[moq-dev/moq#7883](https://github.com/moq-dev/moq/pull/7883)): ✅ m0.

Builders there refuse `unshare --map-root-user` with
`unshare: write failed /proc/self/uid_map: Operation not permitted`.
[moq-dev/moq#7763](https://github.com/moq-dev/moq/pull/7763) retired the
earlier quest because the local check already detects this and skips
(`nix/runner/userns-probe.nix`, `sh/dispatch.sh`). This quest fixes the host
instead, so nothing is skipped. The host is configured in the kixelated/nixos
repository and reachable as `ssh nixos`. Change the Nix builder sandbox
settings there, apply when the merge queue is idle, and check that
`nix/runner/userns-probe.nix` succeeds and `check flake` runs its user-namespace
tests.
