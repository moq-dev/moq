# [S] Preserve the FFI frame cursor across empty groups and cancellation

## Goal

The raw first-frame-per-group convenience returns EOF only when its track
ends, and cancelling one pending read does not silently discard that group's
eventual first frame.

## Plan

At dev `e2350b39a`, `TrackInner::read_frame`
(`rs/moq-ffi/src/consumer.rs:425`) takes the next group into a local variable
and directly returns its first frame. An empty completed group produces
`None`, contradicting the track-EOF promise at `:523`. Empty groups are
constructible through `producer.rs:701,824`. Cancellation while waiting for
the first frame drops the local group after the ordered cursor advanced.
These are source-traced cases, not executed regressions yet.

Keep the pending group in the reader state, loop past completed empty groups,
and preserve it when only an individual async call is cancelled. Retain the
documented first-frame-per-group semantics; this quest does not turn the
convenience into an all-frames reader. Define mixed calls to `next_group` and
`read_frame` explicitly so persistent state cannot duplicate or lose a group.

Add FFI regressions for an empty group on an open track, empty then populated
groups, and cancellation after group acquisition followed by a successful
read of its first frame. Verify terminal cancellation still releases demand.
Check the Python, Swift, Kotlin, and Go conveniences against that behavior.

Public API: behavioral correction without a signature change. Wire: none.
Run `just check`, `just test`, and `just test smoke-full`.

## Related

- [FFI read lanes](/quest/m1/api-ffi-read-lanes.md) - changes the same reader state; coordinate ownership without making either fix depend on the other
