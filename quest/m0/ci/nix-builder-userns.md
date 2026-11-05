# [S] The desktop host's Nix builders may map user namespaces

## Goal

On the desktop host, where agents run their local gates, `just ci` builds the
flake's runner image instead of failing `check flake`, and `just check` runs
it instead of skipping.

## Plan

Decided 2026-11-05 (maintainer, follow-up seen in
[moq-dev/moq#7918](https://github.com/moq-dev/moq/pull/7918),
[moq-dev/moq#7684](https://github.com/moq-dev/moq/pull/7684),
[moq-dev/moq#7858](https://github.com/moq-dev/moq/pull/7858), and
[moq-dev/moq#7883](https://github.com/moq-dev/moq/pull/7883)): ✅ m0.

Each of those gates failed `check flake` while building `github-runner`, with
`unshare: write failed /proc/self/uid_map: Operation not permitted`. `just ci`
sets `CI_STRICT`, so the check fails there; without it,
`nix/runner/userns-probe.nix` and `sh/dispatch.sh` detect the refusal and skip,
which is why [moq-dev/moq#7763](https://github.com/moq-dev/moq/pull/7763)
retired the earlier quest. This quest fixes the host instead, so nothing fails
or skips.

As of 2026-11-05 the probe fails on the desktop host (Ubuntu 26.04, where
`kernel.apparmor_restrict_unprivileged_userns = 1` and even a plain
`unshare --map-root-user true` is refused) and passes on `nixos`. Lift
AppArmor's restriction for Nix builds there, persistently and as narrowly as
works, apply it when no gate is running, and check that the probe succeeds and
`just ci` passes `check flake`.
