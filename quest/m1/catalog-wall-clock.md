# [XS] Catalog wall clock keeps sub-millisecond precision

## Goal

`hang::catalog::Clock::wall_clock` returns the wall time at the precision the
catalog carries, not truncated to milliseconds. The wire field is `{ wall,
timescale }` with a `u32` timescale defaulting to microseconds, and `pts`
arrives at its own timescale, but `wall_clock` divides down to Unix
milliseconds before building the `SystemTime`, which holds nanoseconds. The
capture clock fixtures from [#4125](https://github.com/moq-dev/moq/pull/4125)
assert at millisecond precision because of it.

## Plan

- Rust: compute the offset from the moq epoch in nanoseconds (or as a
  `Duration` from whole seconds plus the remainder at the clock's timescale)
  and add it to `UNIX_EPOCH + MOQ_EPOCH_UNIX_MILLIS`. Keep the existing
  overflow and JSON-safe range checks. Tighten the fixtures that currently
  assert milliseconds.
- JS: `wallClockTime` in `js/hang/src/catalog/clock.ts` returns a `Date`,
  which only holds milliseconds. Leave its return type alone unless a caller
  needs more; note the platform limit in its doc so the two sides are not
  mistaken for a mismatch.
- Callers that format the value (HLS `EXT-X-PROGRAM-DATE-TIME`, DASH
  `availabilityStartTime`) choose their own output precision; check none
  relied on the truncation.

Public API and wire: none.
