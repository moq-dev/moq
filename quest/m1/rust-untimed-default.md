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
- Declare milliseconds, stamped from the shared broadcast clock, in every
  Rust data track that relies on the default today: `hang` catalog
  (`default_track_info`), moq-mux MSF and timeline tracks, moq-boy status,
  moq-c `data_track`, and moq-ffi json and flate tracks. moq-ffi and moq-c
  media tracks already pin microseconds. The shared timeline itself is
  [Shared import clock](/quest/m1/shared-clock.md)'s; this quest only makes
  each data track declare and use it.
- The untimed model (#4822) keeps `Default` as the timescale a lite encoder
  declares for an untimed track's send times; that path names
  `Timescale::MILLI` explicitly instead.
- Update the bindings and `doc/lib` pages that describe the default.

Public API: breaking (`Timescale` loses `Default`; an undeclared track is
untimed). Wire: none.

## Required

- [Untimed model](/quest/m1/untimed-model.md) - `track::Info.timescale` becomes an `Option` there

## Related

- [Publishing never invents a timestamp](/quest/m1/publish-timestamp.md) - the same publishers stop filling in a timestamp
