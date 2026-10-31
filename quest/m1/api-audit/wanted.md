# [M] Demand is wanted, everywhere

## Goal

Every demand signal reads as `wanted`: a snapshot `wanted()` plus an async
`wanted_changed()` (and its poll), where "not wanted" is `false` or `None`.
No `used`/`unused`/`is_used` methods remain in kio, moq-net, `@moq/net`,
moq-ffi, moq-c, or the wrappers, and methods without a consumer are deleted
instead of renamed.

## Plan

Decided 2026-10-25 (maintainer), extending the `wanted` shape that
[#6751](https://github.com/moq-dev/moq/pull/6751) gave `group::Demand`:

- Yes/no demand returns `bool`; `track::Demand::wanted() -> Option<u64>`
  keeps its meaning.
- Rust's `broadcast::Demand`, deleted in
  [#6744](https://github.com/moq-dev/moq/pull/6744), is back in the `wanted`
  shape for publishers that steer on whether anyone reads a broadcast. The
  `@internal` JS `Broadcast.Demand` (read only by tests and a bench) goes. The
  origin closes a superseded producer explicitly instead of a publisher
  watching demand.
- `group::Demand` already says `wanted`. The JS group demand stays internal.
- `bandwidth::Producer` gets a `demand()` handle with `wanted() -> bool` and
  `wanted_changed()`, like track.
- `track::Fill` merges `is_used()` and `next()` into
  `wanted() -> Option<u64>` (the next group owed), with `wanted_changed()`
  replacing `unused()`. Check that it returns `None` once no reader is left.
- kio `Producer`/`ProducerWeak` rename too: `wanted() -> bool` and
  `wanted_changed()`; delete the test-only async variants.
- The Announce consumer count is a `wanted`-shaped signal, not a new `unused()`.
- moq-c's `MOQ_DEMAND_USED/UNUSED` become `WANTED/UNWANTED`; moq-c takes the
  break with every binding.

Fix the stale `Used`/`Unused` advice in the Go wrapper README
(`go/wrapper/README.md`, about line 143). The Impact note
corrects the `rs/moq-json` and `rs/moq-flate` Unreleased advice for the cut.

Public API: breaking renames and deletions in every language. Wire: none.
