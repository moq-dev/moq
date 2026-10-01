# [L] Publishing requires a timestamp

## Goal

Every publish API takes a timestamp, and nothing below it invents one. A
frame's timestamp is the publisher's statement of when the content happened.
A library never fills in "now" on the caller's behalf. Covers moq-net,
moq-json, moq-binary, moq-mux, moq-ffi, and every binding wrapper (Python,
Swift, Kotlin, Go, Dart). Not libmoq, which the
[Generated C bindings](/quest/m1/c/README.md) replace.

## Plan

Decided (2026-09-30): the wire timestamp stays required, so the publish side
must be too. Today `Timed.at: None` means "stamp when written": moq-json and
moq-binary fill `Timestamp::now()` (moq-net's clock, not the broadcast's), and
moq-mux's `Clock::stamp` fills the broadcast clock's `now()`. The FFI raw
`MoqFrame.timestamp_us` and `MoqDatagram.timestamp_us` default to 0.

- `moq_net::Timed<P, T>` keeps its name and gets a required `at: T`, with no
  `From<bare payload>`. The json/binary consumers return the same type (see
  [Data consumer timestamps](/quest/m1/data-consumer-timestamps.md)).
- moq-json (snapshot, stream, and window, whose `push` always stamps now) and
  moq-binary producers take `Timed<_, Timestamp>`. The moq-mux data producers
  take `Timed<_, Instant>` and map it onto the broadcast clock, refusing one
  ahead of now (`Error::InvalidCapture`), as today; `Clock::stamp` stops
  filling `None`. `catalog::data::Listing::record` then always has a capture
  time for `delay` and `jitter`.
- Publishers inside the repository (hang catalog snapshots, MSF, stats, room
  chat, examples) pass their clock's now explicitly.
- moq-ffi: the
  capture time is a media timestamp on the broadcast's timeline. moq-ffi
  exposes the broadcast clock's `now()` as a timestamp; callers stamp payloads
  with values taken from it, and moq refuses one ahead of now. That keeps a
  device or process clock out, as the Rust `Instant` mapping does. Either map
  the timestamp back through the clock inside moq-ffi or give the clock a
  typed timestamp moq-mux accepts; keep a raw `Timestamp` from compiling
  there. The json/binary `update` and `append` take it as a required
  argument, and the raw frame and datagram records lose their `default = 0`.
- Go (`go/wrapper/moq`) and Python (`py/moq-rs`) wrap only the JSON
  producers; [#4137](https://github.com/moq-dev/moq/pull/4137) added
  `publish_binary_snapshot` / `publish_binary_stream` to moq-ffi without
  them. Add hand-written binary wrappers there. The maintainer asked for this.
  Each wrapper gets a test that a past capture time is accepted and a future
  one refused.
- Docs: "stamped when written" in `doc/lib/rs/{moq-json,moq-binary}.md` and
  `rs/moq-net/src/model/timed.rs`, plus `doc/lib/{py,swift,kt,go,dart}`.

What a receiver fills in when a peer sends no timestamp is out of scope:
[Plan: untimed peer objects](/quest/m1/plan-untimed-objects.md).

Public API: breaking, on `dev`. Wire: none.

## Required

- [JSON and flate namespaces](/quest/m1/ffi-shape/json.md) - moves the data producers this changes, so the two breaks land in order rather than colliding

## Related

- [Data track clock](/quest/m1/data-track-clock.md) - the moq-mux mapping this keeps reads the catalog's clock at write time
- [JS publishing requires a timestamp](/quest/m1/js-publish-timestamp.md) - the same change in the JS packages
