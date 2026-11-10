# [S] MoQ Boy is a demo, not a published package

## Goal

The `moq-boy` crate and `@moq/boy` npm package stop publishing, and MoQ Boy
lives as a demo: no semver, CHANGELOG, MIGRATE, API report, or release tag.
The Solid UI it alone carries leaves the published surface with it. The
hosted demo keeps working.

## Plan

Planned 2026-11-09 (maintainer, /quest-plan from the deletion audit).
Decided: ✅ unpublish (rejected: keep publishing; delete MoQ Boy).
Joins m2 because ending a published package is breaking.

- `rs/moq-boy`: `publish = false`, drop its CHANGELOG and MIGRATE, and drop
  the `moq-boy-v*` tags from `release-nix.yml`. Keep the flake package
  `moq-boy`: moq.pro's demo hosts build it from their pin.
- `js/moq-boy`: move into `demo/boy` as a private package with no API
  report, MIGRATE, or trusted-publisher entry.
- `@moq/signals/solid` has no other consumer: delete it. Either rewrite the
  Boy UI on `@moq/signals/dom`, as watch and publish do, or keep Solid
  private to `demo/boy`; the implementer picks the smaller change.
- The release cut deprecates the last published versions on crates.io and
  npm with a pointer to the demo.

Public API: breaking. `moq-boy` and `@moq/boy` stop publishing, and
`@moq/signals/solid` is removed. Wire: none.
