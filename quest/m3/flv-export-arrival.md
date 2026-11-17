# [XS] An FLV export judges a frame on when it landed

## Goal

`container::flv::Export` hands the jitter buffer each frame's real arrival, as the TS and MKV
exports do, so a track the buffer holds back (its reads gated by `Buffer::full`) is not judged as
sent further and further ahead of its decode time.

## Plan

Found 2026-11-17 while chasing a spurious `the source's clock runs ... ppm off ours` from
`moq import webrtc --connect <whep> -- export rtmp --connect <rtmp>` (the quest that fixed it
deleted its file). `FlvTrack::empty` is when the source last returned `Pending`, and
`Export::fill` uses it as every frame's `arrived` (`rs/moq-mux/src/container/flv/export.rs`),
ignoring `Event::Frame { arrived }`, which carries the time the frame landed in the cache. A track
whose reads `full` gates never returns `Pending`, so `empty` stops moving, and each frame read
looks older than the last by the time it waited: replaying a recorded trace, one audio track's
`decode - arrived` grew 0.5 s to 1.6 s over 1.5 s. Use `arrived.max(track.empty)` (the MKV export
takes `arrived.max(track.subscribed)`), and add a test that gates one track and checks the
buffer sees no lead growing.

The maintainer kept this quest (2026-11-17, [on the PR that found it](https://github.com/moq-dev/moq/pull/9032#issuecomment-6962042151)).

## Related

- [Container export ends cleanly](/quest/m3/export-close.md) - the same exports' shutdown
