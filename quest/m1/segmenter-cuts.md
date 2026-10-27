# [S] Segment cuts hold bounded state

## Goal

The moq-mux HLS segmenter holds at most one pending cut, so an input that
declares segments faster than it opens groups cannot grow its state.

## Plan

Found while landing [moq-dev/moq#6783](https://github.com/moq-dev/moq/pull/6783).
`Segmenter.cuts` (`rs/moq-mux/src/hls.rs`) is a `VecDeque<Timestamp>` that
`cut()` appends to without bound, and only a new group start drains it. Cuts
come from a declared fMP4 segment, including a `styp` box in network input
(for example `moq-cli` HLS ingest of a remote origin with `--hls`). An origin
that sends `styp` segments with rising pts and no sync sample never starts a
group, so the queue grows by one entry per segment.

A group start drops every cut at or before its start, and cuts only arrive at
or before the latest frame, so only the latest pending cut can ever matter.
Replace the queue with a single `Option<Timestamp>`, after confirming that
equivalence with a test of several cuts before one group start.
The equivalence rests on the production callers' invariant that each `cut(pts)`
arrives in rising order at or before the latest reported frame, so establish it
from those callers rather than assume it for arbitrary `cut()` calls. The
existing `explicit_cuts_override_the_grid` test stays as a regression guard: it
declares both cuts before any group, so it needs both boundaries.

Public API: none. Wire: none.

## Related

- [fMP4 boundary sync](/quest/m1/fmp4-boundary-sync.md) - the same non-sync declared boundary, on the group side
