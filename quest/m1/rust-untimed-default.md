# [M] An undeclared Rust timescale means untimed

## Goal

A Rust track that declares no timescale is untimed, as in `@moq/net`:
`track::Info::default()` and `track::Request::accept(None)` leave
`timescale` as `None`. Every publisher that stamps from the shared broadcast
clock declares milliseconds explicitly, so data tracks and media share one
timeline.

## Plan

Decided 2026-10-07 (with #4968, where `@moq/net` treats an omitted or `null`
timescale as untimed and `@moq/publish` declares milliseconds):

- Remove the implicit millisecond default: `impl Default for Timescale`
  (`rs/moq-net/src/model/time.rs`) and its use in `track::Info::default()`
  and `accept(None)` (`rs/moq-net/src/model/track.rs`).
- Declare milliseconds in every Rust track that relies on the default today,
  in the same PR, or its writers' stamped frames are refused on a track that
  turned untimed. Known: `hang` catalog (`default_track_info`), moq-mux MSF
  and timeline tracks, moq-boy status, moq-c `data_track`, moq-ffi json and
  flate tracks, moq-room chat (`chat::info`), and moq-stats' JSON tracks
  (`create_track(name, None)`). Find the rest by `Info::default()`,
  `create_track(_, None)`, and `accept(None)`. moq-ffi and moq-c media tracks
  already pin microseconds. The shared timeline itself is
  [Shared import clock](/quest/m1/shared-clock.md)'s.
- This quest only declares the timescale. Who stamps a frame, and with which
  clock, is [Publishing never invents a timestamp](/quest/m1/publish-timestamp.md)'s
  (decided 2026-10-08); writers keep stamping as they do today until it
  lands.
- The untimed model (#4822) keeps `Default` as the timescale a lite encoder
  declares for an untimed track's send times, and the lite datagram decoder
  falls back with `entry.timescale.unwrap_or_default()`
  (`rs/moq-net/src/lite/subscriber.rs`); both name `Timescale::MILLI`
  explicitly instead, as does moq-audio's decode quantum floor
  (`rs/moq-audio/src/decode/consumer.rs`).
- Update the bindings and `doc/lib` pages that describe the default.

Public API: breaking (`Timescale` loses `Default`; an undeclared track is
untimed). Wire: none.

## Related

- [Publishing never invents a timestamp](/quest/m1/publish-timestamp.md) - requires this: the same publishers stop filling in a timestamp once each track declares its timescale
- [FFI shape](/quest/m1/ffi-shape/README.md) - #4519 moves the moq-ffi json and flate tracks this edits; whichever lands second rebases
- [lite-07 untimed](/quest/m1/lite-untimed.md) - requires this: the same `rs/moq-net` lines
