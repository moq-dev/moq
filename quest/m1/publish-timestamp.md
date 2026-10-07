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
data producers fill the broadcast clock's `now()`. The FFI raw
`MoqFrame.timestamp_us` and `MoqDatagram.timestamp_us` default to 0.

- `moq_net::Timed` drops its unused clock parameter `T` (decided 2026-10-06:
  here rather than in #4822, which is already large). That's a breaking
  change and gets an upgrade note. Timedness is per track
  ([untimed model](/quest/m1/untimed-model.md), decided 2026-10-05), so an
  untimed payload belongs on an untimed track, and one appended to a timed
  track is refused. The json/flate consumers return the same type
  (see [Data consumer timestamps](/quest/m1/data-consumer-timestamps.md)).
- moq-json (snapshot, stream, and window, whose `push` always stamps now) and
  moq-flate producers take `Timed<_, Timestamp>` and write `None` as untimed.
- moq-mux data producers already take `Timed<_, Timestamp>` on the broadcast
  clock. Here `None` stops meaning the clock's now and goes out untimed.
  `catalog::data::Listing::record` already samples `delay` and `jitter` only
  for timed writes.
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

Public API: breaking. Wire: none here; absence on the wire lands
with the untimed implementation quests.

## Required

- [moq-net carries untimed frames faithfully](/quest/m1/untimed-model.md) - the model must hold an untimed payload before producers stop filling in now; until lite-07 encodes absence, a lite encoder writes its send time, as producers effectively do today
- [FFI shape](/quest/m1/ffi-shape/README.md) - moves the data producers this changes into `json` and `flate` namespaces in the same merge, so this is ready once #4519 lands rather than waiting on the codec child (2026-10-06 audit)

## Related

- [JS publishing never invents a timestamp](/quest/m1/js-publish-timestamp.md) - the same change in the JS packages
