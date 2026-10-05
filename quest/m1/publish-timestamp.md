# [M] Publishing never invents a timestamp

## Goal

No publish API fills in a timestamp the caller didn't give. A frame's
timestamp is the publisher's statement of when the content happened; a payload
published without one goes out untimed and arrives untimed. Covers moq-net,
moq-json, moq-flate, moq-mux, moq-ffi, and every binding wrapper (Python,
Swift, Kotlin, Go, Dart). Not libmoq, which the
[Generated C bindings](/quest/m1/c/README.md) replace.

## Plan

Decided (2026-10-01), reversing the 2026-09-30 "timestamp required" plan: the
timestamp is optional end to end, on the wire and in every API, and absent
means untimed. A library never substitutes now, wall clock, or arrival time.
How absence travels through the model and over the wire is
[moq-net carries untimed frames faithfully](/quest/m1/untimed-model.md) and
[lite-07 encodes an absent timestamp](/quest/m1/lite-untimed.md); this quest
removes every place that fills one in.

Today `Timed.at: None` means "stamp when written": moq-json and moq-flate
fill `Timestamp::now()` (moq-net's clock, not the broadcast's), and moq-mux's
`Clock::stamp` fills the broadcast clock's `now()`. The FFI raw
`MoqFrame.timestamp_us` and `MoqDatagram.timestamp_us` default to 0.

- `moq_net::Timed<P, T>` keeps its name and its `at: Option<T>`; only the
  meaning of `None` changes. The json/flate consumers return the same type
  (see [Data consumer timestamps](/quest/m1/data-consumer-timestamps.md)).
- moq-json (snapshot, stream, and window, whose `push` always stamps now) and
  moq-flate producers take `Timed<_, Timestamp>` and write `None` as untimed.
- moq-mux data producers already take `Timed<_, Timestamp>` on the broadcast
  clock after [moq-mux data producers take a broadcast-clock timestamp](/quest/m1/mux-data-timestamp.md).
  Here `None` stops meaning the clock's now and goes out untimed.
  `Clock::stamp` is deleted. `catalog::data::Listing::record` samples `delay`
  and `jitter` only for timed writes.
- Publishers inside the repository (hang catalog snapshots, MSF, stats, room
  chat, examples) pass their clock's now explicitly.
- moq-ffi: data producers take an optional timestamp in microseconds on the
  broadcast clock, unchecked, as Rust does. moq-ffi exposes the broadcast
  clock's `now()` so callers have a value to stamp with. The raw frame and
  datagram records' `timestamp_us` is already optional after
  [moq-net carries untimed frames faithfully](/quest/m1/untimed-model.md); publishing one without it
  goes out untimed instead of at `default = 0`.
- Go (`go/wrapper/moq`) and Python (`py/moq-rs`) wrap only the JSON
  producers; [#4137](https://github.com/moq-dev/moq/pull/4137) added
  `publish_binary_snapshot` / `publish_binary_stream` (now
  `publish_flate_*`) to moq-ffi without them. Add hand-written flate wrappers there. The maintainer asked for this.
  Each wrapper gets a test that a timestamp round-trips and an untimed payload
  arrives untimed.
- Docs: "stamped when written" in `doc/lib/rs/{moq-json,moq-flate}.md` and
  `rs/moq-net/src/model/timed.rs`, plus `doc/lib/{py,swift,kt,go,dart}`.

The broadcast-clock input, requested by OneTooMany, was split out into
[moq-mux data producers take a broadcast-clock timestamp](/quest/m1/mux-data-timestamp.md) (2026-10-02),
because it needs neither blocker below.

Public API: breaking. Wire: none here; absence on the wire lands
with the untimed implementation quests.

## Required

- [moq-mux data producers take a broadcast-clock timestamp](/quest/m1/mux-data-timestamp.md) - changes the same producers' input type first, so the two signature changes land in order
- [JSON and flate namespaces](/quest/m1/ffi-shape/json.md) - moves the data producers this changes, so the two breaks land in order rather than colliding
- [moq-net carries untimed frames faithfully](/quest/m1/untimed-model.md) - the model must hold an untimed payload before producers stop filling in now; until lite-07 encodes absence, a lite encoder writes its send time, as producers effectively do today

## Related

- [JS publishing never invents a timestamp](/quest/m1/js-publish-timestamp.md) - the same change in the JS packages
