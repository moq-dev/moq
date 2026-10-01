# [M] Data consumers return each value's timestamp

## Goal

A JSON or binary data consumer returns each value with its frame's timestamp,
so an application can carry data onto another track, or sync it with video,
at the exact time it was published. Covers moq-json, moq-binary, the moq-mux
wrappers, moq-ffi, and every binding wrapper.

## Plan

Requested by an external consumer (OneTooMany, Discord), who translates
MAVLink into application telemetry and must keep its timestamps to stay in
sync with video.

Every `moq_net::Frame` has a timestamp, but the consumers decode only
`frame.payload` and drop it: `moq_mux::{json,binary}::Consumer::next` and the
moq-json and moq-binary snapshot and stream consumers they wrap. A snapshot
consumer returns the timestamp of the frame it decoded last.

Decided: `next()` and `poll_next()` return `Timed<T>`, the type the producers
take, whose `at` becomes required in
[Publishing requires a timestamp](/quest/m1/publish-timestamp.md). The
timestamp is the frame's media timestamp on the track's timescale.
[Plan: untimed peer objects](/quest/m1/plan-untimed-objects.md) decides
whether a peer can deliver a frame without one; if it can, `at` becomes an
`Option` here, so this waits for it and breaks once.

moq-ffi's json/binary consumers return the timestamp too, and the py, swift,
kt, go, and dart wrappers and `doc/lib/*` follow. JS readers stay with
[Data sync in watch](/quest/m3/watch-data-sync.md).

Public API: breaking, on `dev`. Wire: none.

## Required

- [Publishing requires a timestamp](/quest/m1/publish-timestamp.md) - makes `Timed.at` required, the type this returns
- [Plan: untimed peer objects](/quest/m1/plan-untimed-objects.md) - decides whether the returned timestamp can be absent
