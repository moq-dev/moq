# [XS] One Ruff pin

## Goal

Only the dev shell pins Ruff: `ruff` leaves the uv dev group in
`pyproject.toml` (and `uv.lock`).

## Plan

Decided 2026-10-31 (maintainer, the /quest-complete requeue session), a
follow-up of [moq-dev/moq#7319](https://github.com/moq-dev/moq/pull/7319)
(format gates), which has landed.

#7319 makes the repo-wide `python` check module run the dev shell's Ruff and
keeps `ruff` in the uv dev group (`pyproject.toml:61`) only for editors, so
two Ruff versions are pinned. Drop it from the dev group and `uv.lock`, and
point editor setup at the dev shell's Ruff.

Dropped 2026-11-12 (/quest-audit): checking Swift formatting on aarch64-linux
once nixpkgs builds `swiftc` there. The x86 gates already check it, and only
the release-PR Check on `ubuntu-24.04-arm` misses it.

Public API: none. Wire: none.
