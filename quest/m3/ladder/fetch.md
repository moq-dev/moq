# [M] Fetch and catalog

## Goal

An uncached FETCH encodes at the ladder's shared applied target instead of
opening a fresh encoder at the configured maximum, and a source catalog
refresh cannot erase current rung state.

## Plan

Two independent leaks of controller state, both cheap to close once the
controller owns it.

The FETCH path opens a new encoder per requested group at
`Rung::bitrate`: `fetches` and `fetch`
(`rs/moq-transcode/src/rung.rs`) call `encode`, which sets
`config.bitrate = Some(self.info.bitrate)`, so a constrained ladder
still burns full-rate encodes on demand. Make it read the controller's
applied target and participate in the same allocation. A disabled rung stays
manually fetchable, and cache hits are untouched.

Catalog mutations publish full snapshots
(`rs/moq-mux/src/catalog/producer.rs`). The
[compression migration](https://github.com/moq-dev/moq/pull/7046) removed the HANGZ
sibling; coalesce every rung state change from one
controller iteration into a single publication of each supported catalog, and make a
later source catalog snapshot compose with current generated-rung state
rather than overwrite it.

Acceptance: a fresh FETCH using the shared applied target while a cache hit
encodes nothing, and one source catalog refresh leaving disabled rung state
intact.

## Required

- [Controller](/quest/m3/ladder/controller.md) - there is no shared applied
  target to read until the controller owns one

## Related

- [On-demand pops](/quest/m2/video-quality/on-demand.md) - grades and fixes cold-start pops in the same fetch encoder this quest retargets
