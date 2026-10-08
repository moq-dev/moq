# [S] Backport the draft-22 LOCATION_FILTER form to release

## Goal

On `release`, moqt-22 LOCATION_FILTER is encoded and decoded with a Location
Filter Type and no Length, in `moq-net` and `@moq/net`, matching the fix on
`main`. Drafts 20 and 21 keep the length-inferred form.

## Plan

Cherry-pick the main PR as its own PR onto `release`. `release` lacks #5028's
per-draft 0x21 rework in `js/net/src/ietf/parameters.ts`, `fetch.rs`, and
`subscribe.rs`, so expect to adapt rather than apply cleanly. Keep the main
PR's byte-vector tests for every type.

## Required

- [Draft-22 LOCATION_FILTER](/quest/m0/ietf-location-filter-22.md) - the fix lands on `main` first
