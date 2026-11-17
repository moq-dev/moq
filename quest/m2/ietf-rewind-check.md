# [S] moq-transport tracks get the rewind check

## Goal

A moq-transport track is checked for timestamp rewinds like any other timed
track. The exemption goes: `Info.ietf` in Rust (`track::Info::ietf()`, the
`unchecked` copy path in `rs/moq-net/src/model/track.rs` and
`rs/moq-net/src/lite/ietf.rs`, and the moq-transport subscriber's
`unchecked` marking) and in JS (`Info.ietf` in `js/net/src/track.ts` and its
rewind check). Rust and JS change together.

## Plan

Decided 2026-11-16 (maintainer, /quest-run): this follows
[moq-dev/moq#8918](https://github.com/moq-dev/moq/pull/8918), where a
moq-transport frame's time is its Group ID in the track's TIMESCALE and a
third party's TIMESTAMP is never read, so a moq-transport timestamp cannot
rewind and the exemption no longer has a reason. A moq-lite track that is
`unchecked` for another reason (a pre-media-time version) keeps its flag.
Confirm each remaining reader of `ietf()` is only the exemption before
deleting the field; the lite-07 `ietf` hop provenance goes with it if
nothing else reads it.
