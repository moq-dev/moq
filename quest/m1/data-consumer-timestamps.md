# [M] Data consumers return each value's timestamp

## Goal

A JSON or binary data consumer returns each value with its frame's timestamp,
so an application can carry data onto another track, or sync it with video,
at the exact time it was published. Covers moq-json, moq-flate, the moq-mux
wrappers, moq-ffi, and every binding wrapper.

## Plan

Requested by an external consumer (OneTooMany, Discord), who translates
MAVLink into application telemetry and must keep its timestamps to stay in
sync with video.

Every `moq_net::Frame` has a timestamp, but the consumers decode only
`frame.payload` and drop it: `moq_mux::{json,binary}::Consumer::next` and the
moq-json and moq-flate snapshot and stream consumers they wrap. Each snapshot
state carries the timestamp of the frame that produced it.

Decided: `next()` and `poll_next()` return `Timed<T>`, the type the producers
take in [Publishing never invents a timestamp](/quest/m1/publish-timestamp.md).
`at` is the frame's media timestamp on the track's timescale, and `None` for
an untimed frame: absence survives the wire rather than becoming arrival
time (decided 2026-10-01, see
[moq-net carries untimed frames faithfully](/quest/m1/untimed-model.md)). A republisher passes `at` straight to a moq-mux data producer.

Decided (2026-10-01): snapshot consumers get two reads, both returning
`Timed<T>`. Today the moq-json snapshot consumer applies every buffered delta
but yields only the newest state, and moq-flate jumps to the newest group, so
a 9s state is lost when 11s is already buffered. A caller syncing to a
playhead needs the newest state at or before it.

- `next()` (and `poll_next()`) yields every state in order, like a stream
  consumer. A reader that falls behind the drift budget (`Lagged`) still
  jumps to the newest group, so a slow reader stays bounded.
- `latest()` (and `poll_latest()`) skips to the newest state, today's
  behavior, kept as the optimization for callers that only want the current
  value.
- Same names in the moq-mux wrappers, moq-ffi, and every binding.

moq-ffi's json/flate consumers return the timestamp too, and the py, swift,
kt, go, and dart wrappers and `doc/lib/*` follow. JS is
[JS data consumer timestamps](/quest/m1/js-data-consumer-timestamps.md).

Public API: breaking. Wire: none.

## Required

- [Publishing never invents a timestamp](/quest/m1/publish-timestamp.md) - gives `Timed.at` its untimed meaning, the type this returns
- [moq-net carries untimed frames faithfully](/quest/m1/untimed-model.md) - the model must carry an absent timestamp to consumers
