# [S] Segment cuts hold bounded state

## Goal

The moq-mux HLS segmenter's pending cuts stay bounded, so an input that
declares segments faster than it opens groups cannot grow its state.

## Plan

Found while landing [moq-dev/moq#6783](https://github.com/moq-dev/moq/pull/6783).
`Segmenter.cuts` (`rs/moq-mux/src/hls.rs`) is a `VecDeque<Timestamp>` that
`cut()` appends to without bound, and only a new group start drains it. Cuts
come from a declared fMP4 segment, including a `styp` box in network input
(for example `moq-cli` HLS ingest of a remote origin with `--hls`). An origin
that sends `styp` segments with rising pts and no sync sample never starts a
group, so the queue grows by one entry per segment.

A group start drops every cut at or before its start, so of the cuts at or
before the latest reported frame only the latest can still end a segment.
Collapse those into one as frames arrive, and queue only the cuts beyond the
latest frame. A lone `Option<Timestamp>` would assume more than the callers
promise: both callers in `container/fmp4/import.rs` declare a fragment's cut at
its earliest pts just before reporting that fragment's frames, so a network
input holds at most one cut beyond the latest frame, but `cut()` itself accepts
cuts ahead of any frame. With cuts at 10 and 20 pending, a group at 15 must take the 10
boundary and keep 20; add a test of that case. The existing
`explicit_cuts_override_the_grid` test stays as a regression guard: it declares
both cuts before any group, so it needs both boundaries.

Public API: none. Wire: none.

## Related

- [fMP4 boundary sync](/quest/m1/fmp4-boundary-sync.md) - the same non-sync declared boundary, on the group side
