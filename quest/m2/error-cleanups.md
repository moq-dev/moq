# [XS] Error cleanups

## Goal

`moq_net::Error::SessionClosed`, which nothing outside tests constructs, is
deleted. In `js/net`, a raw transport failure (a session that failed without a
close code) resets a stream with `Unroutable`, as Rust maps `Transport` and
`IdleTimeout`; today `localStreamCode` in `js/net/src/error.ts` falls through
to `Internal`.

## Plan

Decided with the maintainer on 2026-10-17: the goal as drafted, from the
follow-ups of [#5729](https://github.com/moq-dev/moq/pull/5729).

#5729 was still open at planning, and its head already maps a
session-sourced transport error to `Unroutable` in `localStreamCode`. If it
landed that way, confirm a test covers it and finish only the Rust half.

`SessionClosed` has match arms in `rs/moq-net` (`error.rs`,
`model/resume.rs`), `rs/moq-ffi/src/error.rs` and `rs/moq-relay`, and
test-only uses in `rs/moq-hls`, `rs/moq-gst` and `rs/moq-relay`. Removing a
public variant is a breaking Rust change: add the `rs/moq-net/MIGRATE.md`
entry. Wire: none.
