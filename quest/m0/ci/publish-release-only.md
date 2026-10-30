# [M] Release credentials off main

## Goal

The `publish` Environment, which holds registry tokens, signing keys, and
deploy credentials, admits only `release` and the protected release tags.
Code that lands on `main` can no longer publish a package or sign a release.
Every release publisher still runs, from the tag or `release` that triggers
it.

## Plan

Decided 2026-10-29 (maintainer): the moq-bot key moved to its own `bot`
Environment, which keeps `main` for the land gate, Dependabot, and the Smoke
toolchain update ([land](/quest/m0/ci/land.md)). Release credentials should
not follow it there.

`publish` still admits `main` because GitHub runs `workflow_run`, `schedule`,
and the default `workflow_dispatch` on the default branch, whatever triggered
them. As of 2026-10-29 these jobs run as `main` and name `publish`:

- `release-kt-lib` (Maven and signing) and `release-winget` (`WINGET_TOKEN`),
  started by `workflow_run` after a release build.
- `alert`, for `DISCORD_WEBHOOK`, also `workflow_run`.
- `smoke`'s Nix channel on `main`, for `CACHIX_AUTH_TOKEN`, which goes away
  with Cachix ([Nix cache](/quest/m0/ci/nix-cache-verify.md)).
- `apt-repo` and `rpm-repo`, dispatched by hand, which can name `release`.

Move each publisher onto its tag's ref, for example a `workflow_call` from the
tag workflow that built it instead of `workflow_run`, and give `alert` a
credential home that is not `publish`. Then the maintainer drops `main` from
`publish`'s deployment policy. `release-brew`, `release-go`, and
`release-swift-lib` already use `bot` and are not blocked on this.

Public API: none. Wire: none.
