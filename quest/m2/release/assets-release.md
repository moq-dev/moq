# [XS] A watch and publish release ships assets()

## Goal

The newest `@moq/watch` and `@moq/publish` on npm export `assets()` and ship
their worklet and worker files under `assets/`.

This quest tracks a condition outside the repository. When it holds, delete
this quest and every `Required` entry that links it.

## Plan

As of 2026-10-20 `main` carries `assets()` (#4518) but `release` does not, so
the newest releases (`@moq/watch` 0.6.3, `@moq/publish` 0.5.3) still lack it;
`@moq/watch` 0.6.3's `exports` has no `./assets/*`. moq.pro's dashboard
adoption of the hosted files lives in moq.pro's package adoption, not here. Check with
`npm view @moq/watch version` and `npm view @moq/publish version` for a newer
version, then confirm its `exports` lists `./assets/*`.
