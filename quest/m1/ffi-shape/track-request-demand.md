# [S] A track request reports its demand

## Goal

`MoqTrackRequest` gains `demand()` in moq-ffi and every wrapper (Python, Go,
Swift, Kotlin, Dart, C++), returning a `MoqTrackDemand` like
`MoqTrackProducer.demand()`, so a dynamic track server can see when nobody
still wants the track and stop producing it. Mirrors Rust's
`track::Request::demand`.

## Plan

Found in #5139 (group request demand), decided 2026-10-09: that quest
assumed `MoqTrackRequest.demand()` already existed, but moq-ffi only has
`demand()` on `MoqTrackProducer` and the media and JSON producers. Start
after #5139 lands, and follow its `MoqGroupRequest.demand()` binding shape,
but keep Rust's lifetime: `accept` hands the same track state to the
producer, so the handle keeps watching it and returns `Closed` only once the
request is rejected or the track closes. A dynamic server can then stop
producing when the last subscriber leaves.

Decided in the 2026-10-10 audit: preserve the
[hand-written C freeze](/quest/m1/c/README.md). Add the feature to moq-ffi
and its wrappers; generated C inherits it when that package lands. Do not
extend `rs/moq-c`'s hand-written ABI. Update the applicable `doc/lib` pages.

Additive in every binding. It edits the same wrappers as
[Bindings](/quest/m0/broadcast-epoch/bindings.md) (#5146), so land it after
that PR to avoid conflicts, without blocking on it. Run
`just test interop --all`. Wire: none.

## Required

- [Group request demand](/quest/m1/ffi-shape/group-request-demand.md) - establishes the binding shape this follows; the 2026-10-10 audit makes the existing start-after rule explicit

## Related

- [Bindings](/quest/m0/broadcast-epoch/bindings.md) - edits the same wrappers; land after it
