# [S] Decide whether lite-07 freezes with FETCH Start Byte

## Goal

Before lite-07 freezes, the maintainer settles whether FETCH keeps
[moq-dev/moq#8699](https://github.com/moq-dev/moq/pull/8699)'s Start Byte,
which continues a half-read frame by byte after a failover, and the draft,
Rust, and JS match.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
Frames are capped at `MAX_CACHE_BYTES`, 32 MiB (`rs/moq-net/src/model/group.rs`).
Whole-frame reads move `read_idx` only once a frame completes, so they ask
byte 0 anyway; only chunk readers gain: a relay forwarding a frame, and the
TCP pump of [moq-dev/moq#8741](https://github.com/moq-dev/moq/pull/8741),
whose own measurements found 64 KiB frames fastest. The saving is at most one
frame's prefix, on one hop, per failover. The cost is a `Lead` type in the group code,
66 start-byte sites across `rs/moq-net` and `js/net`, and a field frozen in
lite-07. No quest names a consumer of frames over 32 MiB.

Recommended: (a), restoring refetch-and-skip, and require a named consumer of
large frames before [byte holes](/quest/m3/unbounded-groups/byte-holes.md)
starts.

Public API: (a) removes the start-byte surface #8699 added. Wire: lite-07-wip
FETCH loses Start Byte.

## Waiting

- Maintainer: (a) back Start Byte out of lite-07 (recommended); (b) keep it, since it is built and tested
