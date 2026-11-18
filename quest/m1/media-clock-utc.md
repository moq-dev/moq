# [L] A broadcast's wall times track UTC for any session length

## Goal

A broadcast's wall times (`sync.clock.wall + pts`, as HLS
`EXT-X-PROGRAM-DATE-TIME`, DASH `availabilityStartTime`, and app
`wallClockTime` read them) stay within about 1 s of the publishing host's
UTC for any session length, on Linux, macOS, Windows, and in browsers,
including after a suspend. Media timing never jumps on an NTP step.

Non-goals: drift of an audio device's crystal against the host clock
(moq-dev/moq#9059 and its `browser-audio-drift` follow-up), skew between
hosts (JWT and grant leeway, DASH `UTCTiming`, derivatives on another host),
an MPEG-TS stream's own clock steps, and WHIP senders' RTCP clocks.

## Plan

Today the anchor is sampled once (`moq_mux::Clock::new`,
`rs/moq-mux/src/clock.rs`; `CatalogProducer` from `performance.timeOrigin` in
`js/publish/src/catalog.ts`) and PTS advances by monotonic elapsed time. The
mapping is right on Linux, whose CLOCK_MONOTONIC takes NTP's rate
correction. It drifts on macOS, Windows, and in browsers by the crystal's
error, about 1 to 4 s a day and up to 43 s at 500 ppm. A suspend adds its
full length, because the monotonic clock stops while the host sleeps.

Decided 2026-11-18 (maintainer, /quest-plan):

- ✅ Slew the PTS clock (rejected: republish the catalog clock, re-anchor per
  publisher run). The media clock runs at UTC's rate, nudged toward the wall
  clock by at most about 500 ppm (NTP's own slew cap), the way Linux slews
  CLOCK_MONOTONIC. The catalog mapping stays fixed for the broadcast, so the
  hang draft does not change. Republishing would make wall time jump between
  frames and need a draft change; re-anchoring per run leaves a single long
  session drifting. Do it where media is stamped (`moq_mux::Clock` and
  js/publish's clock), not kio-wide, so timers and group aging keep the
  plain monotonic clock.
- ✅ A suspend is a gap in the timeline (rejected: slew it out, out of
  scope). Natively, read a clock that counts suspend (CLOCK_BOOTTIME on
  Linux, `mach_continuous_time` on macOS, the interrupt-time equivalent on
  Windows), so PTS jumps forward by the sleep. No media exists for that
  time, and #9059 handles a large audio jump the same way. Slewing a 1 h
  sleep at 500 ppm would take about 80 days.
- ✅ In browsers, a forward jump of `Date.now()` against `performance.now()`
  of more than about 2 s is a suspend gap, and anything smaller is slewed
  (rejected: always slew). Browsers have no boot-time clock to tell a
  suspend from a forward NTP step; a step that big in a browser is rare, and
  a gap only moves forward, so media never runs backward.
- An NTP step natively, or a backward jump anywhere, is slewed out at the cap
  and never jumps media, as the Goal requires.

Recommendations, not decided:

- Rust and JS land in one PR, per the cross-package sync rule.
- Prove it with kio's sim, which models `step_wall` but not slew or suspend:
  add both, and show the mapping's error stays within 1 s across a simulated
  day at ±500 ppm, a step each way, and a suspend.
- gst `moqsink` stamps from the pipeline's running time and places its anchor
  itself (`rs/moq-gst/src/sink/wall.rs`); GStreamer users pick an NTP or
  realtime pipeline clock there, so leave it unless it maps through
  `moq_mux::Clock`.
