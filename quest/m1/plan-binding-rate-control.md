# [S] Plan: encoder rate control from the bindings

## Goal

A settled shape for how a non-Rust publisher follows the connection's send
estimate, so an OBS, Python, Swift, Kotlin, or Go publisher stops encoding at
its configured bitrate regardless of congestion. Run `/plan-quest`; the settled
plan becomes the implementing quest that closes the issue.

## Plan

`moq-ffi` and `libmoq` publish video (`moq_publish_video_raw`) and audio with
default encoder options and never set `Options::bandwidth`. The send estimate
reaches a binding only as a snapshot on `moq_connection_stats` /
`Session::stats`, which a caller can read but cannot hand to an encoder. The
Rust path has followed the estimate since `moq_video::encode::rate` landed, and
the allocator ([moq#2854](https://github.com/moq-dev/moq/pull/2854)) gave it a
second live handle bindings cannot reach.

The awkward part is that `bandwidth::Consumer` and `bandwidth::Allocator` are
live handles with wakeups, which UniFFI and a C ABI do not carry naturally.
Candidates, cheapest first, and the first two compose:

- A flag on the publish call: "follow this connection's estimate", with the
  binding creating the allocator and registering the track. No new handle
  crosses the boundary; loses sharing one allocator across separately created
  publishers.
- An opaque allocator handle minted from a session and passed into each
  publish call. Mirrors the Rust API and composes, at the cost of a new object
  in five wrappers.
- A polled getter returning the current target for a track, for applications
  that own their encoder (OBS does); useless for the built-in encode path.

Whichever shape wins touches `rs/moq-ffi`, `rs/libmoq`, every wrapper, and
`doc/lib/*` per the Cross-Package Sync table, plus `cpp/obs` if the plugin
adopts it.

## Related

- [#2857](https://github.com/moq-dev/moq/issues/2857) - the issue the implementing quest closes
- [#2709](/quest/m1/2709-per-broadcast-bandwidth-estimates-and-reservation.md) - the same allocator mirrored in js/net
- [Ladder](/quest/m1/ladder/README.md) - the transcode consumer of the same estimate
