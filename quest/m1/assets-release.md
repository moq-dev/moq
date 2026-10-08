# [XS] A watch and publish release ships assets()

## Goal

The newest `@moq/watch` and `@moq/publish` on npm export `assets()` and ship
their worklet and worker files under `assets/`.

This quest tracks a condition outside the repository. When it holds, delete
this quest and every `Required` entry that links it.

## Plan

As of 2026-10-08 `main` carries `assets()` (#4518) but `release` does not, so
the newest releases (`@moq/watch` 0.6.2, `@moq/publish` 0.5.2) still lack it. Check with
`npm view @moq/watch version` and `npm view @moq/publish version` for a newer
version, then confirm its `exports` lists `./assets/*`.
