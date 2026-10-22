# [L] Studio destinations

## Goal

A creator adds any number of RTMP, RTMPS, and WHIP destinations alongside
MoQ, each with its own codec and bitrate, and goes live to all of them at
once. Destinations that take the same canvas and audio mix with identical
encode settings share one encoder. Each destination's status and errors show
in the UI.

The fan-out lands here as a UI-free egress library, with the studio as its
first consumer. The library subscribes once to a MoQ broadcast and remuxes it
to any number of destinations, never decoding or encoding. A hosted service
can run the same code server-side.

## Plan

Decided 2026-10-22 with the maintainer:

- **A library, not a gateway:** this repository ships no gateway service and
  no CLI mode for it. It lands in the same PR as its studio consumer, not
  ahead of it.
- **Library behavior:**
  - **Name:** settle the crate's name in this quest with the maintainer.
  - **Destinations:** each one is added and removed at runtime and names the
    renditions it takes. The publisher already produced every codec it
    needs, so there is nothing to transcode.
  - **Isolation:** each destination has its own bounded buffer. A
    destination that accepts a connection and then stops reading drops its
    own media, never blocks the others, and never grows memory. A test
    covers that stalled peer.
  - **Redials:** a dropped destination redials with backoff and can be
    cancelled. A refusal such as a rejected stream key is terminal and is
    reported instead of retried.
  - **RTMPS:** moq-rtmp dials `rtmps://` natively and keeps the URL-derived
    `tcUrl` that plaintext `Client::connect` advertises. Today the TLS route,
    `Client::with_stream`, sends no `tcUrl`, and several ingest servers
    reject that.
  - **Untrusted destination URLs:** a server-side host must be able to
    refuse private and loopback addresses, so dialing goes through a
    caller-supplied policy rather than resolving any URL it is handed.
- **The studio chooses codecs, not a server:**
  - Opus by default.
  - AAC for RTMP ingests, which commonly require H.264 and AAC.
  - E-RTMP HEVC or AV1 only when the creator picks it for a destination that
    accepts it.
- **AAC encoders:** platform encoders only, from the AudioToolbox and Media
  Foundation quests. moq-audio has no software AAC tier, and none is
  planned. On Linux, choosing AAC for a destination fails loudly when no
  encoder is available. It never silently falls back to another codec.
- **Stream keys** go in the OS credential store, never in the scene file,
  which is meant to be shared and versioned. Prefer a maintained crate such
  as `keyring`.
- **Benchmarks:**
  - Sweep concurrent broadcasts against destinations per broadcast in the
    library.
  - Sweep distinct encoder-sharing keys (canvas, mix, and settings) against
    destinations per key in the studio. Finding a destination's encoder must
    not scan every encoder group, and identical keys must never spawn a
    duplicate encoder.
- **Defaults:** offer presets (resolution, frame rate, keyframe interval,
  CBR) for common ingest constraints without naming platforms. The creator
  can override every field.

## Required

- [The studio app](/quest/m3/studio/app.md) - the canvas and MoQ output these destinations join
- [AudioToolbox AAC encode](/quest/m2/audio-encode-audiotoolbox.md) - AAC on macOS
- [Media Foundation AAC encode](/quest/m2/audio-encode-mediafoundation.md) - AAC on Windows
