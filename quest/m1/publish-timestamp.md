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
How absence travels through the model and over the wire is the untimed model
([#4822](https://github.com/moq-dev/moq/pull/4822)) and
[lite-07 encodes an absent timestamp](/quest/m1/lite-untimed.md); this quest
removes every place that fills one in.

Today `Timed.at: None` means "stamp when written": moq-json and moq-flate
fill `Timestamp::now()` (moq-net's clock, not the broadcast's), and moq-mux's
data producers fill the broadcast clock's `now()`. The FFI raw
`MoqFrame.timestamp_us` and `MoqDatagram.timestamp_us` are optional, but a
raw track published through moq-ffi is always timed, and the Python and
Swift wrappers default them to 0.

- `moq_net::Timed` drops its unused clock parameter `T` (decided 2026-10-06:
  here rather than in #4822, which is already large). That's a breaking
  change and gets an upgrade note. Timedness is per track
  (the untimed model, [#4822](https://github.com/moq-dev/moq/pull/4822), decided 2026-10-05), so an
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
  clock's `now()` so callers have a value to stamp with.
  Decided 2026-10-08, replacing the 2026-10-07 rule that a raw track
  published through moq-ffi is always timed: `MoqTrackInfo`'s null
  `timescale` means untimed when publishing, as it already does on a
  received track. A received info then round-trips, and an untimed raw track
  needs no API of its own. A timed raw track names its timescale, and a
  write that doesn't match the track's timedness is refused. The wrappers'
  `= 0` defaults go with it.
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

- [An undeclared Rust timescale means untimed](/quest/m1/rust-untimed-default.md) - each track declares its timescale first, so a publisher that stops stamping writes onto a track that already says whether it is timed

## Related

- [JS publishing never invents a timestamp](/quest/m1/js-publish-timestamp.md) - the same change in the JS packages
- [FFI shape](/quest/m1/ffi-shape/README.md) - moves the data producers this changes into `json` and `flate` namespaces in the same merge, so this is ready once #4519 lands rather than waiting on the codec child (2026-10-06 audit)
