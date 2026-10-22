# [XS] Error cleanups

## Goal

`moq_net::Error::SessionClosed`, which has no meaning of its own, is
deleted.

## Plan

Moved to m1 on 2026-10-22 (maintainer, from an external API audit): the
cut is breaking anyway, so this break ships with it instead of forcing
another breaking release later.

Decided with the maintainer on 2026-10-17: the goal as drafted, from the
follow-ups of [#5729](https://github.com/moq-dev/moq/pull/5729), widened the
same day to Rust after review found Rust sends `Internal` too.

#5729 landed as `50da59e67`. It maps `Transport`, `IdleTimeout` and a
codeless session failure to `Unroutable` in both languages, with unit tests
in `rs/moq-net/src/error.rs` and `js/net/src/error.test.ts`. Keep that
behavior and its tests; only the `SessionClosed` removal remains here.

`SessionClosed` has match arms in `rs/moq-net` (`error.rs`,
`model/resume.rs`), `rs/moq-ffi/src/error.rs` and `rs/moq-relay`, and
test-only uses in `rs/moq-hls`, `rs/moq-gst` and `rs/moq-relay`.
`rs/moq-net/src/model/track.rs`'s `close` constructs it only to convert it;
decided 2026-10-22 (maintainer): keep the removal and construct
`StreamError::Session(SessionError::Cancel)` there directly. Removing a
public variant is a breaking Rust change: add the `rs/moq-net/MIGRATE.md`
entry. Wire: none.

## Related

- [One clean close](/quest/m1/api-audit/clean-close.md) - collapses the clean session-close spellings this removal touches
