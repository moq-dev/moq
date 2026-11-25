# [M] Foreign-timeline ingests shift onto the catalog clock

## Goal

Every foreign-timeline ingest (`moq-rtsp`, `moq-webrtc`, and `moq-gst`) lands its
frames on the catalog's `sync.clock` (including pre-fixed and shared clocks, with
RTSP using RTCP sender reports when available) by shifting through
`Timebase::shift` at the ingest boundary, so sibling broadcasts align by wall
time without adding shifting complexity to codec importers, encoders, or
bindings.

## Plan

Reported in [moq-dev/moq#9507](https://github.com/moq-dev/moq/issues/9507): an
RTSP camera's `wall_clock(pts)` reads 10 s or more before capture. `Clock::new`
starts 10 s back (`LEAD`, `rs/moq-mux/src/clock.rs`), `Reserved` carried a
`Timebase` that only container importers (`fmp4`, `ts`, `flv`, `mkv`) read while
codec importers silently dropped it, and `Timebase::shift` was `pub(crate)`.
`moq-rtsp` passed `timebase.reserve()` to `h264`/`h265`/`aac::Import` assuming
it shifted; `moq-webrtc` emitted 0-based `IngestClock` timestamps without
touching `Timebase`; `moq-gst` called `wall.timebase.place` without shifting,
leaving pre-fixed clocks unshifted. No single PR caused it.

Decided 2026-11-25 (maintainer, `/quest-plan`, superseding the 2026-11-23
`/quest-import` plan in [moq-dev/moq#9512](https://github.com/moq-dev/moq/pull/9512)):

- ✅ Keep codec importers and `container::Producer` verbatim on the catalog
  clock, expose `Timebase::shift` as `pub`, and decouple `Reserved` from
  `Timebase` in one breaking quest (rejected: shifting inside `Rendition` with
  `catalog.clocked()`, which leaks into `cut`/`seek`/bindings and risks
  double-shifting `ts`, `flv`, `moq-video`, and `catalog_now()` callers; keeping
  `Timebase::reserve()` as an additive-only change; splitting the additive fix
  and breaking decouple into two quests).
  - Remove `Timebase::reserve()`. `Reserved` comes only from
    `Producer::reserve()` as the initial-catalog publish gate.
  - Give container importers (`fmp4::Import`, `ts::Import`, `ts::Passthrough`,
    `flv::Import`, `mkv::Import`, and `import::Container`) an explicit
    shared-timebase option (e.g. `.with_timebase(timebase)`, noting
    `import::Container::new` decodes `init.data` immediately so a shared
    timebase must reach it before that decode), defaulting to a fresh
    `catalog.timebase()`. Update the ~6 non-test callers (`moq-hls`, `moq-cli`,
    `moq-rtsp`, `moq-gst`, `moq-ffi`, `libmoq`).
  - Shift at each foreign-timeline ingest boundary:
    - `moq-rtsp`: shift published frames via `timebase.shift(pts)` and hold one
      `Reserved` until placed so audio-only sessions do not publish the catalog
      before PLAY.
    - `moq-webrtc`: hold one `catalog.timebase()` (plus one `Reserved` until the
      first frame) and shift `IngestClock`'s output via `timebase.shift(pts)`
      (placing at arrival; do not wait 5 s for str0m's audio sender report).
    - `moq-gst`: shift buffer timestamps through `wall.timebase.shift(pts)` in
      `rs/moq-gst/src/sink/pad.rs` so pre-fixed catalog clocks
      (`Config::with_clock`) work too.
  - Leave `moq-video`, `moq-audio`, `moq-ffi` public API, and `libmoq` public
    API unchanged (binding callers keep stamping `write_frame` / `media_frame`
    from `catalog_now()` / `moq_publish_catalog_now()`).
- ✅ Keep RTSP RTCP sender-report waiting local to `moq-rtsp`, waiting up to one
  report interval by default with no config knob (rejected: a shared `Placement`
  helper in `moq-mux`, since `moq-webrtc` places at arrival and only `moq-rtsp`
  buffers frames with retina's per-frame `VideoParameters`;
  `Client::with_report_wait(Duration)` defaulting to `0` or configurable).
  - Hold frames from the latest keyframe onward with their per-frame
    `VideoParameters`, place via `timebase.place(pts, wall)` on the first sane
    RTCP sender report, or fall back to arrival on an unusable report
    (unset/out-of-range NTP) or once one report interval expires (polled
    alongside media reception so an idle stream still releases). Waiting ~2 s at
    ingest connect is fine; egress latency is what matters.
- ✅ Take over [moq-dev/moq#9506](https://github.com/moq-dev/moq/pull/9506)'s
  branch (`fix/moq-rtsp-sync-clock`, allows maintainer edits) (rejected: asking
  j4y-v to rework #9506; opening a fresh PR and closing #9506). Keep its
  `fake::Server` tests and j4y-v's credit, drop `Client::with_report_wait` and
  the pre-fixed-clock warning, replace its RTSP-only workaround with
  `Timebase::shift`, and ask j4y-v to re-measure `now - wall_clock(pts)` on
  their camera once landed.

Test fresh catalogs, delayed pulls, pre-fixed `Config::with_clock`, and two
sessions sharing one catalog across `moq-rtsp`, `moq-webrtc`, and `moq-gst`,
plus `moq-mux` `Timebase` tests. Run `just test interop --all`, since `moq-ffi`
updates its internal container timebase wiring.

Public API: breaking in `moq-mux` (`Timebase::reserve()` removed; container
importers take a shared `Timebase` explicitly; `Timebase::shift` becomes `pub`),
so this moves from `m3` to `m2` to land before the 0.22 cut. Include the
migration note under `## Impact` and mark the PR title `!`. No `moq-ffi`,
`libmoq`, or wire breaks.

## Closes

- [#9507](https://github.com/moq-dev/moq/issues/9507) - moq-rtsp's `sync.clock` puts every frame 10 s or more before capture
