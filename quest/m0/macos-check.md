# [S] just check passes on macOS

## Goal

`just check` on a clean `main` checkout passes on a macOS host inside the Nix
dev shell, as it already does on Linux. Three failures block it today, all
reproduced on `origin/main` by
[moq-dev/moq#8392](https://github.com/moq-dev/moq/pull/8392) and
[moq-dev/moq#8394](https://github.com/moq-dev/moq/pull/8394):

- `test gh`: `sh/version.test.py`'s
  `test_cut_promotes_only_published_library_dependencies_and_preserves_exemptions`
  (`'0.0.1' != '0.22.0'`) and `test_release_plz_keeps_manual_cut_despite_breaking_commits`,
  with release-plz 0.3.149.
- `check js`: `doc/.vitepress/api/index.test.ts`, "a re-export from another
  workspace package links to its owner"; the rendered link holds
  `/private/var/folders/...`.
- `check rs`: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p moq-carla-bridge`
  reports unresolved links to `Bridge`, `Source`, and `protocol`.

Non-goal: a macOS runner on the PR gate (rejected in
[macos.md](/quest/m0/nightly/macos.md)).

## Plan

Planned 2026-11-09 (maintainer, /quest-complete follow-up of #8392 and #8394).
Milestone: ✅ m0 (rejected: a later milestone).

Causes, read from source on Linux (no macOS host was available):

- carla bridge, **confirmed**: `demo/carla/bridge/src/lib.rs` puts
  `#![cfg(target_os = "linux")]` after its `//!` docs, so on macOS the crate is
  empty and the intra-doc links dangle. Either gate the links too or build the
  crate (and its docs) only on Linux, e.g. by keeping it out of the macOS doc
  selection.
- doc re-export link, **guess**: `tmpdir()` is `/var/folders/...` on macOS,
  a symlink to `/private/var/...`. `index.ts` computes package paths with
  `relative(root, dir)`; if one side is resolved through the symlink (by
  TypeScript or the workspace scan) and the other is not, the link becomes a
  `../../private/var/...` path. Confirm by checking which side resolves, then
  realpath both sides in `index.ts` (production paths can be symlinked too),
  not only in the test.
- `sh/version.test.py`, **guess**: the Linux dev shell provides release-plz
  0.4.1 (`pkgs.release-plz` in `flake.nix`), while the failing host ran
  0.3.149, so it likely picked up a non-Nix `release-plz` (e.g. from
  `~/.cargo/bin`) ahead of the dev shell. The flake uses one nixpkgs for both
  systems, so a different darwin build is less likely. If it is PATH, the
  test should fail loud on a release-plz older than it supports instead of
  asserting wrong versions; if the dev shell's darwin release-plz really is
  0.3.149, align it.

Verify each fix on a macOS host (a maintainer machine or a manual Platform
dispatch), and on Linux.

Public API: none. Wire: none.
