# [M] Track demand is watched through demand() alone

## Goal

In Rust and JS, a track's subscribers are watched only through its `Demand`:
`track::Producer` drops `is_used`/`used`/`unused` and every layer producer
built on a track (moq-json, moq-mux import, moq-audio, moq-binary) exposes
`demand()` instead of its own copies. Group and broadcast `used`/`unused` stay;
`Demand` is a track concept, and group demand drives fetch coalescing.

## Plan

About 150 call sites move, across moq-net, moq-mux, moq-json, moq-audio,
moq-binary, moq-relay, moq-transcode, moq-stats, and libmoq. Check the error
each wait returns: the producer's maps to its abort reason, `Demand`'s to
dropped. Keep what callers rely on, and keep `abort_unused` if its race still
needs an owner. JS mirrors the Rust shape in `js/net`.

Lands after the release, alongside the [FFI shape](/quest/next/ffi-shape/README.md)
line, so moq-net breaks once.

Public API: breaking in moq-net and the layer crates, and in `@moq/net`. Wire:
none.

## Required

- [Release](/quest/dev/release.md) - the break follows the release
