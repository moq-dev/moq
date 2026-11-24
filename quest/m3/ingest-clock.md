# [L] Codec-importer ingests land their frames on the catalog's clock

## Goal

A frame published through moq-mux's codec importers with a caller's own
timestamp, as moq-rtsp, moq-webrtc, moq-ffi's `write_frame` and libmoq's
`media_frame` do, lands on the catalog's `sync.clock`:
`wall_clock(pts)` is the instant the frame was captured, per the source's
RTCP sender report, or the instant it arrived when the source sends none.
`hang.md`'s promise that sibling broadcasts align through the wall mapping
then holds for every live ingest, and for a clock fixed before the session
(`Config::with_clock`) or shared by two sessions.

## Plan

Reported in [moq-dev/moq#9507](https://github.com/moq-dev/moq/issues/9507):
an RTSP camera's `wall_clock(pts)` reads 10 s or more before capture. The
cause, verified at `888de61a12`: `Clock::new` starts 10 s back (`LEAD`,
`rs/moq-mux/src/clock.rs:182-192`), the codec importers stamp through
`Rendition::timestamp`, which hands back the caller's PTS unchanged
(`rs/moq-mux/src/catalog/tracks.rs:659`, `catalog/producer.rs:628`), and
nothing places the clock. Only the container importers (fMP4, TS, MKV, FLV)
anchor or shift. The same gap hits moq-webrtc (`rs/moq-webrtc/src/session.rs`
`IngestClock`), moq-ffi (`rs/moq-ffi/src/media.rs:1315`) and libmoq
(`rs/libmoq/src/publish.rs:614`). moq-gst places the clock itself
(`rs/moq-gst/src/sink/wall.rs`), and moq-video's encoder stamps from
`catalog.clock()`, so both are already right. The bug is as old as the codec
importers, with no single culprit PR.

Decided 2026-11-23 (maintainer, /quest-import):

- ✅ Fix it in moq-mux for every codec-importer caller (rejected: RTSP only,
  as [moq-dev/moq#9506](https://github.com/moq-dev/moq/pull/9506) does, which
  leaves WebRTC, ffi and libmoq off and only warns on a shared clock). A
  `Rendition` keeps its `Timebase` and routes a caller's PTS through
  `Timebase::shift`, as the container importers do. A timebase minted from
  `catalog.clock()` (moq-video's encoder) gets a zero offset, so its frames
  are not shifted by encode latency; make that difference a type rather than a
  flag. Today `Producer::reserve` is `timebase().reserve()`
  (`catalog/producer.rs:821`), which is the split to make.
- ✅ RTP ingests place on the sender report, through one helper in moq-mux
  that moq-rtsp and moq-webrtc share (rejected: arrival only; dropping every
  frame until the first report, where a camera without RTCP never starts).
  moq-webrtc's existing sender-report mapping (`session.rs:776-790`) and
  #9506's RTSP mapping fold into it. While it waits, the session keeps only
  the frames since the latest keyframe, and it waits at most one RTCP report
  interval before placing at arrival.
- ✅ Take over #9506's branch (rejected: inviting its author to rework it;
  a fresh PR). It allows maintainer edits. Keep its `fake::Server` tests and
  the author's credit, replace its RTSP-side placement with the moq-mux
  shift, and ask j4y-v to re-measure `now - wall_clock(pts)` on their camera.
  Drop `Client::with_report_wait`, which nothing calls, and #9506's warning
  on a pre-fixed clock, which the shift makes unnecessary.

Test the shift where it lives: a catalog whose clock was fixed earlier, and
two sessions sharing one, each places its frames at their capture or arrival
instant. Run `just test interop --all`, since moq-ffi changes.

Public API: moq-mux's timebase split may change `Producer::reserve` or
`Timebase`; bring any break to the maintainer, as m3's Plan asks, before
proceeding. moq-rtsp drops nothing published. A caller's PTS now shifts onto
the catalog clock, which is a behavior change for moq-ffi and libmoq. Wire:
none.

## Closes

- [#9507](https://github.com/moq-dev/moq/issues/9507) - moq-rtsp's `sync.clock` puts every frame 10 s or more before capture
