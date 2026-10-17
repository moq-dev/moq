# [XS] Error cleanups

## Goal

`moq_net::Error::SessionClosed`, which nothing outside tests constructs, is
deleted. A raw transport failure or idle timeout (a session that failed
without a close code) resets a served stream with `Unroutable` in both Rust
and JS, so one condition sends one code. Today both send `Internal`: Rust's
`StreamError::from(&Error)` in `rs/moq-net/src/error.rs` reaches its
catch-all arm for `Transport` and `IdleTimeout`, and `localStreamCode` in
`js/net/src/error.ts` falls through.

## Plan

Decided with the maintainer on 2026-10-17: the goal as drafted, from the
follow-ups of [#5729](https://github.com/moq-dev/moq/pull/5729), widened the
same day to Rust after review found Rust sends `Internal` too.

#5729 was still open at planning, and its head already maps `Transport`,
`IdleTimeout` and a codeless session failure to `Unroutable` in both
languages, with unit tests in `rs/moq-net/src/error.rs` and
`js/net/src/error.test.ts`. If it lands that way, confirm those tests and
finish only the `SessionClosed` removal; otherwise add the mapping and a
test on each side here.

`SessionClosed` has match arms in `rs/moq-net` (`error.rs`,
`model/resume.rs`), `rs/moq-ffi/src/error.rs` and `rs/moq-relay`, and
test-only uses in `rs/moq-hls`, `rs/moq-gst` and `rs/moq-relay`. Removing a
public variant is a breaking Rust change: add the `rs/moq-net/MIGRATE.md`
entry. Wire: none.
