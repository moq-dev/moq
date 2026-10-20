# [XS] Cargo.lock pins wasm-bindgen

## Goal

`Cargo.lock` is the only pin for wasm-bindgen: the Nix dev shell's
`wasm-bindgen-cli` follows the locked `wasm-bindgen` crate version, so a
nixpkgs bump can no longer move the CLI away from the crate. A locked version
nixpkgs does not package fails loudly at Nix eval.

## Plan

Why: [#5896](https://github.com/moq-dev/moq/pull/5896) changed only
`flake.lock`, which moved the CLI to 0.2.129 while the crates were still
`=0.2.127`. Dependabot auto-merged it on the required Check and Test while
WASM was red, and [#5911](https://github.com/moq-dev/moq/pull/5911) re-pinned.
Any nixpkgs bump can repeat it.

- `flake.nix` reads the `wasm-bindgen` version from `Cargo.lock`
  (`builtins.fromTOML`) and selects nixpkgs' versioned
  `wasm-bindgen-cli_0_2_<n>`. The pinned nixpkgs (8edc0c72) carries
  `wasm-bindgen-cli_0_2_93` through `_0_2_129`.
- Update the comments beside `wasm-bindgen-cli` in `flake.nix` and beside the
  `wasm-bindgen` pin in `rs/moq-wasm/Cargo.toml`, which today say to bump
  both together.

Decided 2026-10-19 (maintainer): `Cargo.lock` selects the CLI. Rejected: a CI
step comparing the two versions, which detects drift instead of removing it,
and making Dependabot's auto-merge wait for WASM.

Public API: none. Wire: none.
