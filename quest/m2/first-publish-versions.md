# [XS] New experimental packages first publish at 0.0.x

## Goal

`moq-rtsp` and `@moq/stats` are at `0.0.1` and `moq-bench` sets
`publish = false`, before the cut publishes them for the first time. At the
cut `sh/version.py` moves every package not at `0.0.x` onto the shared version
line for good, and all three are new on `main` at `0.1.0`.

## Plan

Decided 2026-11-12 (maintainer, /quest-audit before the cut): ✅ experiments
start at `0.0.x` as [Versions](/RELEASE.md#versions) says, and moq-bench stays
off crates.io since it is a binary with no dependents (rejected: only
unpublishing moq-bench; leaving all three on the shared line). `moq-quic` and
`moq-congestion` are also new at `0.1.0` and are core, so they join the
shared line.

- Check each dependent's version requirement after the change, and that
  `just version` leaves the three alone.
- [Claim new npm names](/quest/m3/release/npm-first-publish.md) still claims
  `@moq/stats`: a `0.0.x` package publishes too.
- Sweep for any other package first publishing at this cut above `0.0.x`
  that the list above misses (compare `main`'s packages against the
  registries), and bring each to the maintainer rather than deciding it.

Public API: none. Wire: none. A version on the shared line cannot go back,
which is why this lands before the cut.
