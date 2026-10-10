# [S] A group request reports its demand

## Goal

`MoqGroupRequest` gains `demand()` in moq-ffi and every wrapper (Python, Go,
Swift, Kotlin, Dart, C++), matching Rust's `group::Request::demand`, so a
group server can see when nobody still wants the group and stop producing it.

## Plan

Decided in #4868; the bullet was lost when #4946 removed `net.md`, and it
moved out of the FFI shape README into its own quest when the line landed
(2026-10-09). Mirror `MoqTrackProducer`'s existing `demand()`, which returns a
`MoqTrackDemand`. Additive in every binding. Wire: none.
