# [XS] gpu-video releases on wgpu 30

## Goal

A [gpu-video](https://crates.io/crates/gpu-video) release on crates.io
depends on wgpu 30, the version moq-video's `render` uses.

This quest tracks a condition outside the repository. When it holds, delete
this quest and every `Required` entry that links it.

## Plan

As of 2026-10-05 the newest release is 0.4.0 (2026-05-12) on wgpu 29. Master
is on wgpu 30.0.0 (#2111) along with an unreleased API overhaul (#2039).
Check with `curl -s https://crates.io/api/v1/crates/gpu-video` for a newer
`max_version`, then its `wgpu` requirement.
