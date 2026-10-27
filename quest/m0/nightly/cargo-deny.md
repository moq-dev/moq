# [S] cargo deny passes on main

## Goal

`just rs audit` (`cargo deny check`) passes on main, so Nightly's Audit job
goes green.

## Plan

Audit has failed on every scheduled run since at least 2026-10-23 (for
example run [40506938673](https://github.com/moq-dev/moq/actions/runs/40506938673)):

- Yanked `crypto-bigint` 0.7.8, through elliptic-curve 0.14 and p256/p384
  into moq-auth, moq-token, and their dependents. `cargo update -p
  crypto-bigint`, then confirm nothing pins the yanked version.
- Licenses from `rs/moq-studio`'s egui stack
  ([moq-dev/moq#6797](https://github.com/moq-dev/moq/pull/6797)):
  `clipboard-win` and `error-code` (BSL-1.0), and `epaint_default_fonts`
  (OFL-1.1, Ubuntu-font-1.0).

Decided 2026-10-27 (maintainer): add BSL-1.0 to the global `allow` list (a
permissive, OSI-approved license), and give OFL-1.1 and Ubuntu-font-1.0
per-crate `exceptions` for `epaint_default_fonts` only, so font licenses do not
spread to code. Studio keeps its clipboard and default fonts.

The `RUSTSEC-2023-0071` (`rsa`) ignore stays: `rsa` still enters through
moq-auth's dev-only compatibility fixtures (published moq-auth 0.2.1 and
moq-token 0.7). Drop it when those fixtures move past `rsa`.

Public API: none. Wire: none.
