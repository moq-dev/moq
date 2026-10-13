# [S] An unset host clock publishes without a wall mapping

## Goal

A `moq_mux::catalog::Producer` on a host whose wall clock reads before 2020,
such as an embedded or Android device booted before NTP sync, publishes
normally with no root `clock` in its catalog, instead of panicking in
`Clock::new()` and aborting the host process. Rust and `@moq/publish` alike.

Non-goals: moving the epoch, and panic isolation, which
[its own quest](/quest/m0/panic-isolation.md) owns.

## Plan

Reported in [#5407](https://github.com/moq-dev/moq/issues/5407) from an
Android device (libmoq 0.6.13). Verified on `main` 2026-10-13:
`Producer::new` (`rs/moq-mux/src/catalog/producer.rs`) falls back to
`Clock::default()`, whose `Clock::new()` `expect`s `arrival(LEAD)`, and
`wall_clock()` refuses a time before the 2020 epoch. Every production
publisher without `Config::with_clock` reaches it: moq-ffi (every binding),
moq-c, moq-cli, moq-gst, moq-rtmp, moq-srt, moq-rtc, moq-hls import,
moq-transcode. `js/publish/src/catalog.ts` `pageClock()` throws the same way
through `u53`.

Decided 2026-10-13 (maintainer):

- Keep the 2020 epoch. The root `clock` is optional on the wire (hang draft,
  Rust `Option`, zod `optional`), and only feeds HLS `PROGRAM-DATE-TIME` and
  DASH `availabilityStartTime`. A reading before 2020 reliably means the
  clock is unset. No clock is better for consumers than a false one: DASH
  falls back to the relay's anchor, while a 1970 AST is unplayable.
  Rejected: moving to a 1970 epoch under a renamed root key (hang-04), which
  buys consistency at the cost of a wire rename and a mapping consumers
  can't use. Rejected: failing `Producer::new`, which stops an offline
  device from ever publishing, though media delivery needs no wall.
- `moq_mux::Clock`'s wall mapping becomes optional and `Clock::wall()`
  returns `Option`. `Clock::new()` stays infallible: the monotonic part
  needs no wall. `Producer::new` keeps its signature and sets
  `catalog.clock = clock.wall()`.
- Fail loud where a wall is actually needed: `reading(wall)`, and an
  importer placing a source wall on a wall-less clock, return an error.
- Clock identity can't be the wall mapping. `Listing::stamp`
  (`rs/moq-mux/src/catalog/data.rs`) detects an importer re-anchoring the
  clock by comparing `wall()`, which is `None` for both clocks on an unset
  host, so it would measure jitter, delay, and bitrate across unrelated
  timelines. Compare whole clocks (monotonic anchor, reading, and wall).
  Cover a withheld catalog plus a data producer and an importer in the test.
  Found in review on #5418.
- Mirror in `pageClock()`: omit `clock` instead of throwing.
- Interplay with [catalog clock adopt](/quest/m1/catalog-clock-adopt.md),
  which assumes `wall()` is always present: whichever lands second
  reconciles. An absent wall matches an absent catalog clock.
- Test through a private `now` seam (until [Controlled time](/quest/m1/time/README.md)
  makes the wall mockable): a pre-2020 host yields a clock-less catalog and
  media still publishes.
- Fix the `Clock::TIMESCALE` doc's "until the year 2255", the 1970 figure
  (2020 lasts until 2305).
- Then backport ([release line](/quest/m0/release-22/wall-clock-omit.md)).

Public API: `moq_mux::Clock::wall()` returns `Option`. Wire: none.

## Related

- [Release backport](/quest/m0/release-22/wall-clock-omit.md) - cherry-picks this onto the 0.17 line
- [Panic isolation](/quest/m0/panic-isolation.md) - a library panic no longer aborts the host
