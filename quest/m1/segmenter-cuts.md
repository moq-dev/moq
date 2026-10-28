# [S] Segment cuts hold bounded state

## Goal

The moq-mux HLS segmenter's pending cuts stay bounded, so an input that
declares segments faster than it opens groups cannot grow its state.

## Plan

Found while landing [moq-dev/moq#6783](https://github.com/moq-dev/moq/pull/6783).
`Segmenter.cuts` (`rs/moq-mux/src/hls.rs`) is a `VecDeque<Timestamp>` that
`cut()` appends to without bound, and only a new group start drains it. Cuts
come from a declared fMP4 segment, including a `styp` box in network input
(for example `moq-cli` HLS ingest of a remote origin). An origin
that sends `styp` segments with rising pts and no sync sample never starts a
group, so the queue grows by one entry per segment.

No compaction is equivalent to the queue. A group start drops the cuts at or
before its start, but the only lower bound on the next group start is the
current one: `container::Producer::write` lets a keyframe dip below the previous
group's content, and fMP4 validates rising DTS, not PTS. So with cuts at 10 and
20 and frames reported at both, a group at 15 still needs the 10 boundary, and
neither a lone `Option<Timestamp>` nor collapsing against the latest frame keeps
it. Both callers in `container/fmp4/import.rs` declare a fragment's cut at its
earliest pts just before reporting that fragment's frames.

Open question for the maintainer: which bounded policy drops a boundary when a
source declares more cuts than groups, given the ceiling already caps segment
length (for example, keep the earliest and latest pending cuts), or whether a
reorder window can supply a tighter lower bound. Add a test of the 10, 20, 15
case. The existing `explicit_cuts_override_the_grid` test stays as a
regression guard: it declares both cuts before any group, so it needs both
boundaries.

Public API: none. Wire: none.
