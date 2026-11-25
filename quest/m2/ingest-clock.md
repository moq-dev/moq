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
  - Correlate each stream on its own. retina's `Permissive` policy uses
    `rtptime` only when every stream carries it; otherwise each stream's
    elapsed time starts at its own first packet, so two streams starting 1 s
    apart both read 0. The first placed stream places the shared `Timebase`;
    every other stream maps its frames to wall time through its own sender
    report (falling back to its own first frame's arrival) and carries the
    difference from the shared placement as a fixed per-stream offset applied
    before `shift`. When RTP-Info aligned every stream, streams without a
    usable report of their own take the shared placement with no offset.
  - Fold in [#9611](https://github.com/moq-dev/moq/issues/9611) on the same
    start-up path: instead of failing on frames before the announced
    `rtptime` (`MAX_BACKWARD`), start each stream at its first keyframe at or
    after `rtptime`. The report wait already holds about one GOP, so this
    costs almost nothing.
  - Reference, not cherry-pick (they go through the rejected `Placement`):
    j4y-v's [`f578df9ef`](https://github.com/j4y-v/moq/commit/f578df9effec9ba5ddea6a9f027ea156b4a59ee2)
    and [`26e8d4818`](https://github.com/j4y-v/moq/commit/26e8d481850734d4779794a7ae2f0ef636ff27b4)
    for the wait deadline polled alongside media (keep
    `a_wait_expires_at_its_deadline` and
    `a_silent_camera_is_placed_when_the_report_wait_ends`), and
    [`03603914c`](https://github.com/j4y-v/moq/commit/03603914c9b6f1a4a7af1a2a66bba837e11c6325)
    for `moq-webrtc`'s one timebase per ingest, placed at its first frame's
    arrival (`place(first_pts, arrival)` then `shift`).
- ✅ Take over [moq-dev/moq#9506](https://github.com/moq-dev/moq/pull/9506)'s
  branch (`fix/moq-rtsp-sync-clock`, allows maintainer edits) (rejected: asking
  j4y-v to rework #9506; opening a fresh PR and closing #9506). Keep its
  `fake::Server` tests and j4y-v's credit, drop `Client::with_report_wait` and
  the pre-fixed-clock warning, replace its RTSP-only workaround with
  `Timebase::shift`, and ask j4y-v to re-measure `now - wall_clock(pts)` on
  their camera once landed. j4y-v measured the equivalent prototype on
  [#9507](https://github.com/moq-dev/moq/issues/9507#issuecomment-7167621758)
  at 224-293 ms `now - wall_clock(pts)` (against 10.4-11.6 s on main), with
  the catalog publishing 0.9-2.7 s after PLAY instead of 0.05-0.18 s; expect
  the same numbers, and a camera whose reports run ~15 min ahead rejected.

Test fresh catalogs, delayed pulls, pre-fixed `Config::with_clock`, two
sessions sharing one catalog, and (RTSP) streams without RTP-Info starting
apart, across `moq-rtsp`, `moq-webrtc`, and `moq-gst`,
plus `moq-mux` `Timebase` tests. Run `just test interop --all`, since `moq-ffi`
updates its internal container timebase wiring.

Public API: breaking in `moq-mux` (`Timebase::reserve()` removed; container
importers take a shared `Timebase` explicitly; `Timebase::shift` becomes `pub`),
so this moves from `m3` to `m2` to land before the 0.22 cut. Include the
migration note under `## Impact` and mark the PR title `!`. No `moq-ffi`,
`libmoq`, or wire breaks.

## Closes

- [#9507](https://github.com/moq-dev/moq/issues/9507) - moq-rtsp's `sync.clock` puts every frame 10 s or more before capture
- [#9611](https://github.com/moq-dev/moq/issues/9611) - moq-rtsp fails a session whose server replays frames from before its announced `rtptime`
