# [XS] A watch and publish release ships assets()

## Goal

The newest `@moq/watch` and `@moq/publish` on npm export `assets()` and ship
their worklet and worker files under `assets/`.

This quest tracks a condition outside the repository. When it holds, delete
this quest and every `Required` entry that links it.

## Plan

As of 2026-10-02 `main` carries `assets()` and the newest releases predate it
(`@moq/watch` 0.6.1, `@moq/publish` 0.5.1 in the repo). Check with
`npm view @moq/watch version` and `npm view @moq/publish version` for a newer
version, then confirm its `exports` lists `./assets/*`.
