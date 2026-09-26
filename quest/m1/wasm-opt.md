# [XS] wasm-opt for moq-wasm

## Goal

`just wasm` runs binaryen's `wasm-opt -Oz` after `wasm-bindgen`, so the
module is size-optimized the way browser wasm usually is. The module is
528 KB gzip today, up from the ~471 KB in the `wasm-release` comment in the
workspace `Cargo.toml`, which is now stale.

## Plan

Decided in planning: do it now, even though the only consumer is
`test/wasm`. The cost is small and the module is meant to ship.

Guidance:

- Add binaryen to the nix dev shell so CI and local builds match.
- wasm-opt must allow the wasm features rustc emits (bulk memory, sign-ext,
  and others), or it will reject the module. Pass the matching `--enable-*`
  flags.
- Update the comment with measured sizes, and confirm `test/wasm` still
  passes.

## Related

- [Size report](/quest/m1/size-report.md) - tracks the module nightly
