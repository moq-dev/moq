# [S] JS byte holes

## Goal

A huge frame can be read in `@moq/net` as a stream of chunks instead of one
`Uint8Array`, so a browser can receive a file-sized frame without holding it
whole, and `@moq/net` speaks the lite wip byte offset on range ends and group
headers as Rust does.

## Plan

Decided 2026-10-27 (maintainer): split out of
[JS unbounded groups](/quest/m1/unbounded-groups/js.md), which now waits only
on front eviction. This half mirrors
[Byte holes](/quest/m1/unbounded-groups/byte-holes.md).

- Mirror the Rust names for the chunked frame read.
- Mirror the byte offset on the same lite wip version as Rust, and refill a
  hole inside a frame the way Rust does.
- Test a frame several times the cache window through a relay, read as
  chunks, plus Rust/JS interop of a mid-frame refill. Run
  `just test interop --all`.
- If [JS-only @moq/net from Rust](/quest/m2/rs2ts-js.md) is adopted first,
  this shrinks to exposing the streaming read.

Public API: adds a chunked frame read. Wire: the lite wip byte offset, as
Byte holes defines it.

## Required

- [Byte holes](/quest/m1/unbounded-groups/byte-holes.md) - the Rust contract and the lite wire this mirrors
