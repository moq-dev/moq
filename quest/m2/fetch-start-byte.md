# [S] lite-07 freezes without FETCH Start Byte

## Goal

lite-07's FETCH carries no Start Byte: a failover mid-frame refetches the
frame and skips what was read, as before
[moq-dev/moq#8699](https://github.com/moq-dev/moq/pull/8699), and the draft,
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

Restore refetch-and-skip, and require a named consumer of
large frames before [byte holes](/quest/m3/unbounded-groups/byte-holes.md)
starts.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ (a) back Start Byte out of lite-07 and restore refetch-and-skip (rejected: (b) keep it).

Public API: (a) removes the start-byte surface #8699 added. Wire: lite-07-wip
FETCH loses Start Byte.

