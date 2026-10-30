# [S] MKV block headers never panic

## Goal

The MKV importer returns `InvalidSimpleBlock` or `InvalidBlock` for a
SimpleBlock or Block too short for its header, instead of panicking. The
`mkv` target's 4-byte input (`u6OBgA==`) is committed under
`rs/moq-mux/fuzz/regressions/` and passes.

## Plan

webm-iterable 0.7.1's `Block::try_from` (`blocks/block.rs:136,141`) indexes
`data[position..position + 2]` and `data[position]` after the track vint
without a length check, so a block of the vint alone panics. moq-mux calls
it through `SimpleBlock::try_from` in
`rs/moq-mux/src/container/mkv/import.rs` (the SimpleBlock arm and
`handle_block_group`). Any MKV publisher can send it.

Recommended: parse the block header in moq-mux (track vint, 16-bit relative
timestamp, flags, lacing), returning an error on a short header, and stop
calling webm-iterable for it. [MKV import streams
blocks](/quest/m1/security-audit/mkv-stream-blocks.md) needs the same small
header reader to stream the payload, so whichever lands first writes it and
the other reuses it. webm-iterable is outside the orgs this repository may
post to, so reporting the panic upstream needs the maintainer's approval.

Public API: none. Wire: none.
