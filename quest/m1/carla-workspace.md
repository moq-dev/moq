# [S] The CARLA bridge builds outside the root workspace

## Goal

`demo/carla/bridge` is no longer a root Cargo workspace member, so workspace
builds, checks, and `cargo` metadata stop compiling a demo bridge, while the
bridge still builds and is checked when its own files change.

## Plan

Planned 2026-11-09 (maintainer, /quest-plan from the deletion audit).
Decided: ✅ exclude it from the workspace (rejected: keep it a member;
move it to its own repository).

The bridge is the root `Cargo.toml`'s first `members` entry. Give it its own
workspace and lock file, keep its check in the dispatch for `demo/carla/**`
changes, and keep the files where they are: moq.pro reads
`demo/carla/sim/trucks.json` from its pin. Check the GPU end-to-end test in
[vendor-free CARLA bridge](/quest/m5/carla-bridge.md), which touches the
bridge, still finds it.

Public API: none. Wire: none.
