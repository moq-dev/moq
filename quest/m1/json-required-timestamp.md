# [S] JSON writes take a broadcast-clock timestamp in every binding

## Goal

Every JSON snapshot update and stream append takes a required timestamp on
the broadcast clock: `moq_mux::json`'s `Snapshot` and `Stream`, moq-ffi's
JSON producers, and moq-c's `moq_publish_json_snapshot_update` and
`moq_publish_json_stream_append`, where a NULL timestamp fails instead of
being stamped with `now()`. moq-c gains `moq_publish_catalog_now()`, so a C
publisher stamps its media and its data in the broadcast clock's epoch. A C
publisher's JSON tracks no longer give its media an 8 s catalog `delay`.

## Plan

Reported in [moq-dev/moq#7573](https://github.com/moq-dev/moq/issues/7573)
and verified on `main`. moq-c stamps a NULL-timestamp JSON write with the
catalog clock's `now()` (`json_snapshot_update` and `json_stream_append` in
`rs/moq-c/src/publish.rs`). That write takes a seat in the catalog's write
`Order` against C media on the publisher's own timeline, so every media
track advertises the gap between the two timelines, about 8 s, as `delay`.
Rust and moq-ffi publish a null timestamp untimed instead, and moq-ffi
already exposes `MoqMediaCatalogProducer::now()`.

Decided 2026-11-03 (maintainer, `/quest-import`):

- Fix: require a timestamp, and expose `now()` to C, "because every track in
  a broadcast needs to use the same epoch". Options were: NULL writes
  unordered; ✅ require a timestamp (with C `now()`); won't fix.
- Scope: ✅ every binding in one quest; moq-c only; moq-c now and the others
  later.
- This is the JSON part of
  [time-addressing's required timestamps](/quest/m1/time-addressing/model.md),
  which removes untimed writes everywhere. It lands first because #7573 is a
  live bug.

`moq_publish_catalog_now(broadcast, moq_timestamp *dst)` mirrors moq-ffi's
`now()`. `doc/lib/c` tells a C publisher to stamp media and timed data from
it. A NULL timestamp returns the invalid-argument error and publishes
nothing. Test that a C broadcast with JSON tracks advertises no `delay` on
its media, and that NULL fails, and update the moq-ffi and Rust JSON tests to
pass a timestamp.

Public API: breaking in moq-c (NULL refused, `moq_publish_catalog_now`
added), moq-ffi (JSON `timestamp` required), and `moq_mux::json` (a write
takes a timestamp). Wire: none.

## Closes

- [#7573](https://github.com/moq-dev/moq/issues/7573) - moq-c JSON writes without a timestamp give every media track ~8 s of delay
