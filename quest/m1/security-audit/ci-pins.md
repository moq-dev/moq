# [S] CI tool pins

## Goal

Every CI and release job that installs a binding generator from git pins it
by full commit in one shared place, and the remaining loose supply-chain ends
in workflows are tightened: Windows `nasm` and the scanner's nextest are
version-pinned, `release-winget.yml` checks its trigger like
`release-brew.yml`, and `id-token: write` is granted only to jobs that use
OIDC.

## Plan

From the 2026-10-27 audit (SUP-1, SUP-2, SUP-4, SUP-6, SUP-7), verified:

- `cargo install uniffi-bindgen-cpp --git ... --tag v0.11.0-kixelated.7+v0.32.2`
  is copied 7 times (`c.yml`, `cpp.yml`, `platform.yml` twice, `release-c.yml`,
  `release-cpp.yml`, `release-shared-ffi.yml`), and `release-ffi.yml` passes a
  tag as `--rev` for uniffi-bindgen-go. `--locked` pins dependencies, not the
  tag. Replace the copies with one composite action carrying the peeled commit
  (`a5a1ac411772b3f589859edf50aef7bc7538100b` for the cpp tag at audit time;
  re-check with `git ls-remote`), and keep it in the flake.nix pin-together
  list. Windows release jobs cannot use the Nix-built tool.
- `choco install nasm` is unpinned in `release-binary.yml`, `release-c.yml`,
  `release-cpp.yml`, and `platform.yml`.
- `.oss-scanner/Dockerfile` pipes `get.nexte.st/latest`; pin the nextest
  version CI uses. The base image stays a tag (no secrets, nothing shipped).
- `release-winget.yml` gates `workflow_run` on success and a branch prefix
  only; add `release-brew.yml`'s `event == 'push'` and same-repo checks.
- `check.yml`, `ts.yml`, and `interop.yml` grant `id-token: write`
  workflow-wide with no OIDC consumer; drop those grants and the ts caller's
  in `release-gate.yml`, keeping interop's Pages job grant and the
  release-gate grant it needs.

Public API: none. Wire: none.
