# [S] RTSP import

## Goal

`moq import rtsp --connect rtsp://camera/stream` publishes an IP camera's
H.264 or H.265 video and AAC audio as a broadcast. It runs on the camera's
own network, so a camera behind NAT needs no inbound port. A `moq-rtsp`
library crate carries it, so other publishers reuse it the way they reuse
moq-srt and moq-rtmp.

Non-goals: an RTSP server (push ingest), a relay pulling cameras itself, and
codecs nothing in the stack plays (G.711, MJPEG).

Parked in m3 until a camera customer asks (2026-10-01).

## Plan

Decided 2026-10-01:

- retina is the RTSP client and depacketizer. RTP over TCP interleaved is
  the default: it crosses NAT and firewalls with the session it already has.
- Video passes through without re-encoding into moq-mux's H.264 and H.265
  importers. Ask retina for Annex-B framing so parameter sets arrive in-band
  with each keyframe (cameras often send them only in the SDP), and publish
  nothing until the first keyframe.
- AAC maps to moq-mux's AAC importer. Audio the stack cannot play, G.711
  above all (common on cameras), is skipped with one clear warning and the
  broadcast goes on video-only. Transcoding it waits for a customer who
  needs it.
- A camera reboot or network blip reconnects in-process with backoff rather
  than exiting for a supervisor to restart. This deliberately differs from
  Pronto's truck, which exits so a supervisor surfaces half-dead sessions.
- Each camera session publishes a new broadcast under a fresh epoch, per
  [Broadcast epochs](/quest/m1/broadcast-epoch/README.md), never spliced onto
  the last one. A new session's RTP time restarts, so a reconnect finishes the
  old broadcast cleanly and builds a fresh catalog, tracks, and importers,
  starting on a keyframe. A timestamp jump inside one session (retina #64)
  also ends that session's broadcast and starts a new one, instead of failing
  the track. Decided 2026-10-02; shifting timestamps onto the existing
  catalog clock behind a `discontinuity()` marker was rejected, since a
  broadcast name always means the same content.
- Credentials ride the URL's userinfo, as every RTSP tool takes them. retina
  refuses a URL with userinfo, so strip it into `SessionOptions::creds`
  before `describe`, and log only the redacted URL (`RedactedUrl`). A
  `--connect` URL is visible to other local users through `/proc` cmdline;
  accepted for now, and revisit with an env var or file for shared hosts.
- Prior art: moq.pro's `pronto/truck/src/camera.rs` is a working retina pull
  client, video only, run against mediamtx and real cameras.

Test against a local RTSP source serving a known H.264 + AAC clip (Pronto
uses mediamtx), checking the catalog and first frames; a G.711 source that
publishes video only; and a source restarted mid-stream, whose RTP time
resets, that ends the first broadcast cleanly and publishes a second one under
a new epoch, starting on a keyframe with its own catalog.
`doc/bin/cli.md` documents `import rtsp`.

Public API: the `moq-rtsp` crate and the `moq import rtsp` subcommand.
Wire: none.

## Related

- [Broadcast epochs](/quest/m1/broadcast-epoch/README.md) - the epoch each camera session's broadcast publishes under
- [moq.pro's Pronto truck](https://github.com/moq-dev/moq.pro/blob/main/pronto/truck/src/camera.rs) - the prior art this generalizes, and the first consumer
