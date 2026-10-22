# [M] Studio destinations

## Goal

A creator adds any number of RTMP, RTMPS, and WHIP destinations alongside
MoQ, each with its own codec and bitrate, and goes live to all of them at
once.
- **Encoders:** destinations with identical encode settings share one
  encoder.
- **Failure:** a destination that drops redials without disturbing the
  others, and its error shows on that destination.
- **RTMPS:** moq-rtmp's `Client` dials `rtmps://` itself. Today it connects
  over plaintext TCP only, so TLS needs a caller-supplied stream through
  `Client::with_stream`.

## Plan

- **The studio chooses codecs, not a server**, decided 2026-10-22:
  - Opus by default.
  - AAC for RTMP ingests, which commonly require H.264 and AAC.
  - E-RTMP HEVC or AV1 only when the creator picks it for a destination that
    accepts it.
- **Stream keys** go in the OS credential store, never in the scene file,
  which is meant to be shared and versioned. Prefer a maintained crate such
  as `keyring`.
- **Defaults:** offer presets (resolution, frame rate, keyframe interval,
  CBR) for common ingest constraints without naming platforms. The creator
  can override every field.

## Required

- [The studio app](/quest/m3/studio/app.md) - the canvas and MoQ output these destinations join
