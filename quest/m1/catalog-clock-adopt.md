# [S] The catalog's root clock is the producer's clock

## Goal

A `moq_mux::catalog::Producer` never advertises a root `clock` that its own
`Clock` disagrees with. A derivative broadcast (a transcode ladder, or one
assembled from other broadcasts) adopts its input's wall mapping by writing
it through `Producer::modify()`, and the producer re-bases its clock on it.
Requested downstream.

A derivative republishes its input's timestamps, which only mean something
under the input's mapping. Today `modify()` setting `clock` rewrites the
advertised field but leaves the producer's internal `Clock` fresh. That
internal clock drives `Producer::clock()`, `timestamp(None)`, the json and
binary data tracks' stamping, and importer offsets (`Producer::anchor`), so
it disagrees with the mapping the catalog advertises. moq-transcode does
exactly this (`populate` in `rs/moq-transcode/src/catalog.rs` sets
`out.clock = source.clock`), and cannot use `Config::with_clock` instead: it
registers its catalog tracks in `Transcoder::new`, before it has read the
source's clock. `Clock::at(Instant, SystemTime)` is no way out either: the
caller has to invent an `Instant` for the input's PTS zero, and on Windows,
whose `Instant` cannot go below its origin, that fails for a PTS zero before
the host's boot.

Non-goals: the sub-millisecond loss in `hang::catalog::Clock::wall_clock`,
owned by [its own quest](/quest/m1/catalog-wall-clock-precision.md); JS
publishers, which derive their clock from `performance.timeOrigin`; browser
support. Every `moq_mux::Clock` reads `SystemTime::now()`, which panics in
the browser, so none works there today (moq-mux is only compile-checked for
wasm32). Browser wall time comes with
[moq-net on moq-time](/quest/m1/time/net.md) (decided 2026-10-12 from
review, over adding a browser wall source here or waiting on moq-time).

## Plan

Decided 2026-10-12 by the maintainer, after an API review, over a public
`Clock::from_wall` constructor passed to `with_clock` (which left the
`modify()` disagreement possible and could not serve moq-transcode) and over
offering both:

- A commit whose root `clock` differs from the producer's clock re-bases the
  producer's clock on it while nothing has fixed the clock. Once something
  has (a handed-out `Producer::clock()`, an anchored importer, the first
  publish), a changed `clock` is refused, as is clearing it. A commit that
  leaves `clock` equal is unaffected, so moq-transcode re-applying its source
  clock on every update keeps working, and a source that changes its mapping
  fails loud. Tests that stage a clockless catalog through `modify()` (one in
  moq-hls) need another route.
- No new public API: the constructor that builds a `Clock` from a catalog
  clock stays `pub(crate)`, beside `placed` and `arrival`. Build it on
  `placed`'s approach: anchor `instant` at now and compute `reading` from the
  wall, with no `Instant` arithmetic, taking the catalog wall `Timestamp` as
  given rather than round-tripping it through `SystemTime`, so the stored
  mapping equals the input's (factor the shared part out of `placed`).
- Timescale: rescale the wall to `Clock::TIMESCALE` (micro) exactly, and
  refuse one that does not land on a whole microsecond (some 90 kHz values).
  Every first-party publisher already emits micro.
- A PTS zero after this host's now (cross-host skew beyond the input's 10 s
  lead) is refused with `UnmappableTimestamp`, as `placed` does: `now()`
  cannot read a negative timestamp, and saturating would stamp data tracks
  and importers early by the skew without saying so.
- Docs: rustdoc only, no `doc/` page. `Producer::modify` and
  `Config::with_clock` say that the root `clock` is the producer's clock and
  when it may change, and that writes through `Reserved::video` and the codec
  producers keep their timestamps verbatim.
- Kept apart from `hang::catalog::Clock`, which is the wire mapping alone.
  `moq_mux::Clock` adds a monotonic anchor and reads the host's time, which
  neither the format crate nor the JS catalog schema should carry.
- No JS or binding mirror. js/publish derives its clock from
  `performance.timeOrigin`, so every broadcast on a page already shares one,
  and no binding exposes a mux `Clock`; bindings get theirs through
  [the shared timebase](/quest/m2/ffi-timebase.md).

Tests: a micro catalog clock written before anything fixes the clock is
advertised unchanged and `Producer::clock().wall()` equals it; a millisecond
wall converts; an inexact wall, a future PTS zero, a change after the clock
is fixed, and a clear are refused; `now()` reads the elapsed time since the
wall's PTS zero on a paused clock; moq-transcode's derivative stamps data on
its source's mapping.

Public API: none; `Producer::modify` refuses a root `clock` change it used to
accept silently. Wire: none.

## Related

- [Exact catalog wall clock](/quest/m1/catalog-wall-clock-precision.md) - the same mapping read back at full precision
- [moq-net on moq-time](/quest/m1/time/net.md) - moves this clock's reads to moq-time
