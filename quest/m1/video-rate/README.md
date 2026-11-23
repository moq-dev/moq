# Capped VBR video

## Goal

Every video encoder we drive runs constant quality under a hard peak, the
rendition's cap: complex scenes reach the cap, and calm or static ones fall
well below it. The cap is what ABR, reservations, probing, and the catalog
`bitrate` price a rendition at, so a viewer never picks a rendition whose
peaks it cannot carry. The encoder's bitrate is `min(cap, grant)`, with quality
mode spending less when the scene allows. Screen capture stops re-encoding an
unchanged screen at full frame rate and sends a repeat at least every 250 ms
instead.

Non-goals: dropping frames from camera sources (sensor noise means no two
frames match, and over capped VBR it would save about 25 kbps); a public
quality knob, which nothing would use yet.

## Plan

Decided with the maintainer on 2026-11-15 (/quest-plan):

- **Scope.** ✅ Capped VBR on every encoder, plus screen DTX (rejected:
  capped VBR only, since PipeWire and WGC would keep re-sending what
  ScreenCaptureKit already drops; DTX for every source, since camera frames
  never match). Video DTX saves about 25 to 50 kbps per viewer over capped
  VBR (a skip P-frame of a few dozen bytes plus 70 to 100 bytes of packet
  overhead per frame at 30 to 60 fps). The bandwidth win is capped VBR over
  CBR; DTX mostly saves packets per second at relay fan-out, encoder work,
  and mobile radio time.
- **Pricing stays at the cap.** Reservations, the catalog `bitrate`, viewer
  selection, PROBE targets, and rung disable already use the configured
  ceiling, never the measured output (`rs/moq-net/src/model/bandwidth.rs`,
  `js/publish/src/video/encoder.ts`). Keep it that way.
- **Fallback.** ✅ A backend uses its quality mode with a peak where it
  actually honors one, decided at setup from its capabilities; otherwise it
  runs CBR at the cap (rejected: an average target with no hard peak, which
  can overshoot what ABR priced). A browser's CBR doesn't hold the cap
  exactly, and its fallback doesn't need to (decided 2026-11-17 on
  [moq-dev/moq#9026](https://github.com/moq-dev/moq/pull/9026#issuecomment-6966230347)).
- **Quiet scenes.** ✅ Keep `min(cap, grant)` and BBR3's app-limited aging
  ([moq-dev/moq#7017](https://github.com/moq-dev/moq/pull/7017), kept on
  2026-10-27), and measure a quiet-to-busy switch (rejected: ignoring
  estimate drops while the encoder sends below the estimate). Since
  [moq-dev/moq#9460](https://github.com/moq-dev/moq/pull/9460) only the
  reported estimate ages; BBR's pacing keeps the probed rate, so a backlog
  drains at the path's rate. If
  reaching the cap after a quiet stretch is slow, the fix belongs to
  [probing](/quest/m3/probe/README.md), not to an override of the estimate.

Milestone m1: every child changes behavior only (encoder settings, defaults,
capture cadence), with no public API, wire, or catalog JSON break, and the
maintainer wants it this cycle.

## Related

- [Probe up to a target bitrate](/quest/m3/probe/README.md) - keeps the estimate near the sum of caps while capped VBR idles
- [Video quality](/quest/m9/video-quality/README.md) - the VMAF harness that can score the quality levels this epic picks
