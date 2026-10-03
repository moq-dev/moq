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
- A new session's RTP time restarts, and moq-mux ends the import on a group
  that starts before the previous one. So reconnect keeps the importers and
  shifts every track by one offset that lands the new session's first frame
  at its arrival time on the existing catalog clock, after a `discontinuity()`
  marker. Group sequence and A/V alignment carry on, and video resumes on a
  keyframe. A timestamp jump inside one session (retina #64) takes the same
  path instead of failing the track.
- Credentials ride the URL's userinfo, as every RTSP tool takes them. retina
  refuses a URL with userinfo, so strip it into `SessionOptions::creds`
  before `describe`, and log only the redacted URL (`RedactedUrl`).
- Prior art: moq.pro's `pronto/truck/src/camera.rs` is a working retina pull
  client, video only, run against mediamtx and real cameras.

Test against a local RTSP source serving a known H.264 + AAC clip (Pronto
uses mediamtx), checking the catalog and first frames; a G.711 source that
publishes video only; and a source restarted mid-stream, whose RTP time
resets, that resumes on a keyframe without a rewind or A/V drift.
`doc/bin/cli.md` documents `import rtsp`.

Public API: the `moq-rtsp` crate and the `moq import rtsp` subcommand.
Wire: none.

## Related

- [moq.pro's Pronto truck](https://github.com/moq-dev/moq.pro/blob/main/pronto/truck/src/camera.rs) - the prior art this generalizes, and the first consumer
