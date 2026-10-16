# [M] JS unbounded groups

## Goal

`@moq/net` has no group frame-count, group byte, or frame size cap. A huge
frame can be read as a stream of chunks instead of one `Uint8Array`, so a
browser can receive a file-sized frame without holding it whole. A reader
that falls behind an evicted front gets the same refill-or-`Evicted` behavior
as Rust.

## Plan

See the [epic](/quest/m2/unbounded-groups/README.md) for the decisions.

- Today `js/net/src/group.ts` caps frames and bytes and throws
  `GroupTooLarge`, and JS has no byte pool. Its eviction is whole groups by
  idle time. Match the Rust contract; don't port the Rust pool unless a
  browser workload needs it.
- Mirror the Rust names for the streaming frame read.
- Update `js/json`, `js/flate`, and their tests, which lean on the removed
  constants.
- If [Rust-backed @moq/net](/quest/m2/rs2ts/README.md) is adopted first,
  this shrinks to exposing the streaming read.

Public API: removes the JS cap constants, and adds a chunked frame read.
Wire: none beyond Byte holes.

## Required

- [Byte holes](/quest/m2/unbounded-groups/byte-holes.md) - the Rust contract and the lite wire this mirrors
