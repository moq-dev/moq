# [XS] One Ruff pin and Swift formatting on aarch64-linux

## Goal

Only the dev shell pins Ruff: `ruff` leaves the uv dev group in
`pyproject.toml` (and `uv.lock`). The Linux gate checks Swift formatting on
aarch64-linux too, once nixpkgs builds `swiftc` there.

## Plan

Decided 2026-10-31 (maintainer, the /quest-complete requeue session), the
follow-ups of [moq-dev/moq#7319](https://github.com/moq-dev/moq/pull/7319)
(format gates), which has landed.

- **Ruff.** #7319 makes the repo-wide `python` check module run the dev
  shell's Ruff and keeps `ruff` in the uv dev group (`pyproject.toml:61`)
  only for editors, so two Ruff versions are pinned. Drop it from the dev
  group and `uv.lock`, and point editor setup at the dev shell's Ruff.
- **Swift on aarch64-linux.** #7319 adds `swift-format` to the dev shell
  only off aarch64-linux (`flake.nix`, around line 306), because nixpkgs
  marks its `swiftc` broken there, so `sh/swift/format.sh` skips and
  release-PR Check (`ubuntu-24.04-arm`) checks no Swift formatting. When
  nixpkgs builds it, add `swift-format` there and drop the skip. Until then
  this half waits; check nixpkgs' `swift` package `meta.broken` for
  aarch64-linux.

Public API: none. Wire: none.
