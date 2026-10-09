# [XS] @moq/net drops insertTrack

## Goal

`BroadcastProducer.insertTrack` is gone from `@moq/net`; `createTrack` is the
only way to add a track by hand, mirroring Rust's `create_track` (Rust has
`create_track` and `reserve_track`, no insert). It has no production caller.

## Plan

Decided 2026-10-08, from js-track-takeover's
[#5068](https://github.com/moq-dev/moq/pull/5068) report: `insertTrack` takes
a caller-built producer, so it can't take over a queued request (the waiting
subscribers belong to the request's own producer) and keeps throwing
`duplicate track` where `createTrack` answers. Delete it rather than keep a
second, weaker path. Rejected: making it internal, and keeping it public with
docs.

Inline the method into `createTrack`, move the tests that call it onto
`createTrack`, and fix the stale comment in `js/net/examples/publish.ts`
("Mirrors the Rust createTrack/insertTrack") and the `insertTrack` mention in
`doc/lib/js/net.md`.

Public API: breaking (`insertTrack` removed). Wire: none.

## Required

- [JS createTrack takes over a queued request](/quest/m0/js-track-takeover.md) - edits the same `broadcast.ts` methods
