# [XS] @moq/hang's legacy producer compares starts in the track's timescale

## Goal

`js/hang/src/container/legacy.ts`'s `startsPast` compares a group start
with `track.largest()` in the track's own timescale, as Rust's
`moq_mux::start` converts, instead of assuming microseconds.

## Plan

Today it is correct only because `trackInfo`
(`js/hang/src/container/track.ts`) pins `Timescale.MICRO`; a track with any
other timescale would compare microseconds against scaled keys. Convert the
frame's timestamp with the track's timescale once the producer has it
synchronously, and cover a non-microsecond track in a test.
