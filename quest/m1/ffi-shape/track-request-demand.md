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
`demand()` on `MoqTrackProducer` and the media and JSON producers. Follow
#5139's `MoqGroupRequest.demand()` shape, including returning `Closed` once
the request is answered or aborted.

Additive in every binding. It edits the same wrappers as
[Bindings](/quest/m0/broadcast-epoch/bindings.md) (#5146), so land it after
that PR to avoid conflicts, without blocking on it. Run
`just test interop --all`. Wire: none.

## Related

- [Bindings](/quest/m0/broadcast-epoch/bindings.md) - edits the same wrappers; land after it
