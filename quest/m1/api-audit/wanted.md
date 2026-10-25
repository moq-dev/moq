# [M] Demand is wanted, everywhere

## Goal

Every demand signal reads as `wanted`: a snapshot `wanted()` plus an async
`wanted_changed()` (and its poll), where "not wanted" is `false` or `None`.
No `used`/`unused`/`is_used` methods remain in kio, moq-net, `@moq/net`,
moq-ffi, moq-c, or the wrappers, and methods without a consumer are deleted
instead of renamed.

## Plan

Decided 2026-10-25 (maintainer), extending
[moq-net names](/quest/m1/api-audit/net-names.md)'s "Demand says `wanted`":

- Yes/no demand returns `bool`; `track::Demand::wanted() -> Option<u64>`
  keeps its meaning.
- `broadcast::Demand` is deleted (no production consumer), as
  [net-exports](/quest/m1/api-audit/net-exports.md) plans. The origin closes a
  superseded producer explicitly instead of a publisher watching demand.
- `group::Demand` has no production consumer in Rust; delete it rather than
  convert it, unless net-names lands the conversion first. The JS group
  demand stays internal.
- `bandwidth::Producer` gets a `demand()` handle with `wanted() -> bool` and
  `wanted_changed()`, like track.
- `track::Fill` merges `is_used()` and `next()` into
  `wanted() -> Option<u64>` (the next group owed), with `wanted_changed()`
  replacing `unused()`. Check that it returns `None` once no reader is left.
- kio `Producer`/`ProducerWeak` rename too: `wanted() -> bool` and
  `wanted_changed()`; delete the test-only async variants.
- The Announce consumer count from
  [lifecycle](/quest/m1/api-audit/lifecycle.md) is a `wanted`-shaped signal,
  not a new `unused()`.
- moq-c's `MOQ_DEMAND_USED/UNUSED` become `WANTED/UNWANTED`; moq-c takes the
  break with every binding.

Fix the stale `is_used()` advice in `rs/moq-json/MIGRATE.md`,
`rs/moq-flate/MIGRATE.md`, and the Go wrapper README.

Public API: breaking renames and deletions in every language. Wire: none.

## Required

- [moq-net names say one thing](/quest/m1/api-audit/net-names.md) - lands group demand's first `wanted` shape
- [One lifecycle vocabulary](/quest/m1/api-audit/lifecycle.md) - settles close and demand semantics this renames
