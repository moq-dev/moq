# [M] Studio destinations

## Goal

A creator adds any number of RTMP, RTMPS, and WHIP destinations alongside
MoQ, each with its own codec and bitrate, and goes live to all of them at
once. Destinations that take the same canvas and audio mix with identical
encode settings share one encoder. The
fan-out runs on the [egress library](/quest/m3/studio/egress.md), and each
destination's status and errors show in the UI.

## Plan

- **The studio chooses codecs, not a server**, decided 2026-10-22:
  - Opus by default.
  - AAC for RTMP ingests, which commonly require H.264 and AAC.
  - E-RTMP HEVC or AV1 only when the creator picks it for a destination that
    accepts it.
- **AAC encoders**, decided 2026-10-22: platform encoders only, from the
  AudioToolbox and Media Foundation quests. moq-audio has no software AAC
  tier, and none is planned. On Linux, choosing AAC for a destination fails
  loudly when no encoder is available. It never silently falls back to
  another codec.
- **Stream keys** go in the OS credential store, never in the scene file,
  which is meant to be shared and versioned. Prefer a maintained crate such
  as `keyring`.
- **Benchmark:** sweep distinct encoder-sharing keys (canvas, mix, and
  settings) against destinations per key. Finding a destination's encoder
  must not scan every encoder group, and identical keys must never spawn a
  duplicate encoder.
- **Defaults:** offer presets (resolution, frame rate, keyframe interval,
  CBR) for common ingest constraints without naming platforms. The creator
  can override every field.

## Required

- [The studio app](/quest/m3/studio/app.md) - the canvas and MoQ output these destinations join
- [Broadcast egress library](/quest/m3/studio/egress.md) - subscribe once, remux to every destination
- [AudioToolbox AAC encode](/quest/m2/audio-encode-audiotoolbox.md) - AAC on macOS
- [Media Foundation AAC encode](/quest/m2/audio-encode-mediafoundation.md) - AAC on Windows
