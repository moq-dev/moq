# [XS] Windows cfg breaks in moq-video fail the PR

## Goal

A pull request that breaks moq-video's `#[cfg(target_os = "windows")]` code
fails its own checks instead of the next nightly. Today only `just rs
windows` compiles that code, on a Windows runner once a day, so
[#4036](https://github.com/moq-dev/moq/pull/4036) had to repair a Media
Foundation test that had been broken on `main` for a while. That PR showed a
Linux host can catch it:
`cargo check -p moq-video --no-default-features --features capture
--all-targets --target x86_64-pc-windows-msvc` reproduced the errors and
passed with the fix, with no Windows host and no C toolchain.

## Plan

- Run that check per PR whenever the impact map selects moq-video, as a
  recipe the workflow calls. `openh264` stays off because its vendored C++
  needs MSVC; the Media Foundation, D3D11, and capture code is what the
  check is for.
- The dev shell's toolchain carries only `wasm32-unknown-unknown`; add the
  `x86_64-pc-windows-msvc` std target to `flake.nix` so the check runs the
  same locally and in CI. Measure what it adds to the shell and the check.
- Fix the stale `rs/justfile` comment above `windows`: it says cross-compiling
  cannot stand in because openh264 is non-optional, but openh264 is now a
  feature. The nightly Windows job still owns linking, the other crates, and
  running the tests.
- Prove it by reintroducing one of #4036's errors on a branch and watching
  the PR check fail.

Public API: none. Wire: none.

## Required

- [Thin justfiles](/quest/m1/tooling/justfiles.md) - the impact map that
  selects moq-video
